//! Filesystem watching for auto-refresh.
//!
//! `RepoWatcher` watches one workspace for the three kinds of out-of-band
//! change a refresh must pick up: working-copy file edits, operations
//! written by another client (the jj CLI in a terminal), and git refs moved
//! by plain git in a colocated checkout. Events are classified and
//! debounced here; the callback only says "something relevant changed" and
//! the caller refreshes through the backend.
//!
//! Filtering matters more than it looks: a `cargo build` writes thousands
//! of gitignored files, and Jiji's own refreshes rewrite working-copy state
//! under `.jj`. Without classification either would re-trigger refresh in a
//! storm (or, for `.jj`, a loop — though a converged refresh records no
//! operation, so even an unfiltered loop would settle after one echo).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jj_lib::gitignore::GitIgnoreFile;
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::backend::BackendError;

/// Quiet period after the last relevant event before the callback fires;
/// long enough to coalesce an editor's save burst, short enough that the
/// working-copy diff still feels live.
const DEBOUNCE: Duration = Duration::from_millis(400);
/// A steady event stream (a long-running process writing un-ignored files)
/// must not starve refresh forever; fire at least this often.
const MAX_DELAY: Duration = Duration::from_secs(2);

/// Watches a workspace and fires a debounced callback on relevant changes.
/// Dropping it stops the watcher; an already-debouncing callback may fire
/// once more while the drop races the worker thread.
pub struct RepoWatcher {
    // Dropping the notify watcher closes its event channel, which ends the
    // detached debounce thread.
    _watcher: RecommendedWatcher,
}

impl RepoWatcher {
    /// Starts watching `workspace_root` (and the repo's op store, when
    /// `repo_path` lives outside the root — non-default workspaces).
    /// `base_ignores` are the same global/repo-level gitignores the
    /// working-copy snapshot uses; per-directory `.gitignore` files chain
    /// onto them on demand.
    pub fn start(
        workspace_root: &Path,
        repo_path: &Path,
        base_ignores: Arc<GitIgnoreFile>,
        on_change: Box<dyn Fn() + Send + 'static>,
    ) -> Result<Self, BackendError> {
        let watch_err =
            |err: &notify::Error| BackendError::OpenFailed(format!("could not watch repo: {err}"));
        // FSEvents reports canonical paths (`/private/var/...` for `/var`
        // tmp dirs); canonicalize what we strip prefixes against.
        let root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_owned());
        let repo_dir = repo_path
            .canonicalize()
            .unwrap_or_else(|_| repo_path.to_owned());

        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = notify::recommended_watcher(tx).map_err(|err| watch_err(&err))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|err| watch_err(&err))?;
        if !repo_dir.starts_with(&root) {
            watcher
                .watch(&repo_dir, RecursiveMode::Recursive)
                .map_err(|err| watch_err(&err))?;
        }

        let mut filter = EventFilter::new(root, repo_dir, base_ignores);
        std::thread::Builder::new()
            .name("jiji-repo-watcher".into())
            .spawn(move || debounce_loop(&rx, &mut filter, on_change.as_ref()))
            .map_err(|err| BackendError::OpenFailed(format!("could not watch repo: {err}")))?;
        Ok(Self { _watcher: watcher })
    }
}

/// Drains watcher events, fires `on_change` once per quiet period, and
/// exits when the watcher (the sender) is dropped.
fn debounce_loop(
    rx: &mpsc::Receiver<notify::Result<notify::Event>>,
    filter: &mut EventFilter,
    on_change: &(dyn Fn() + Send),
) {
    // (fire-at, latest-allowed-fire-at) while a change is pending.
    let mut pending: Option<(Instant, Instant)> = None;
    loop {
        let timeout = match pending {
            Some((at, cap)) => at.min(cap).saturating_duration_since(Instant::now()),
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                if event_is_relevant(&event, filter) {
                    let now = Instant::now();
                    let cap = pending.map_or(now + MAX_DELAY, |(_, cap)| cap);
                    pending = Some((now + DEBOUNCE, cap));
                }
            }
            Ok(Err(err)) => {
                // A lost or errored event means we cannot tell whether
                // something changed; refresh rather than miss one.
                tracing::warn!("repo watcher event error: {err}");
                let now = Instant::now();
                let cap = pending.map_or(now + MAX_DELAY, |(_, cap)| cap);
                pending = Some((now + DEBOUNCE, cap));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if pending.take().is_some() {
                    on_change();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn event_is_relevant(event: &notify::Event, filter: &mut EventFilter) -> bool {
    if matches!(event.kind, notify::EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|path| filter.is_relevant(path))
}

/// Classifies one event path. Pure with respect to the filesystem except
/// for reading `.gitignore` files (cached until one of them changes).
struct EventFilter {
    root: PathBuf,
    repo_dir: PathBuf,
    base_ignores: Arc<GitIgnoreFile>,
    /// Chained ignores per directory (relative to the root), built lazily.
    ignore_chains: HashMap<PathBuf, Arc<GitIgnoreFile>>,
}

impl EventFilter {
    fn new(root: PathBuf, repo_dir: PathBuf, base_ignores: Arc<GitIgnoreFile>) -> Self {
        Self {
            root,
            repo_dir,
            base_ignores,
            ignore_chains: HashMap::new(),
        }
    }

    fn is_relevant(&mut self, path: &Path) -> bool {
        // Op-store first: `.jj/repo` usually sits inside the root, and only
        // `op_heads` distinguishes a finished operation from index/lock
        // noise (every op — ours or another client's — moves a head file).
        if let Ok(rel) = path.strip_prefix(&self.repo_dir) {
            return rel.starts_with("op_heads");
        }
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };
        if rel.as_os_str().is_empty() {
            return false;
        }
        if rel.starts_with(".jj") {
            // Working-copy state, locks: rewritten by every refresh,
            // including ours. Real changes always reach op_heads above.
            return false;
        }
        if rel.starts_with(".git") {
            // Only ref movement matters (HEAD, loose refs, packed refs);
            // index/objects churn on every git and jj write. Lock files
            // are write-staging noise — the rename onto the real name is
            // the event that counts.
            if rel.extension().is_some_and(|ext| ext == "lock") {
                return false;
            }
            return rel == Path::new(".git/HEAD")
                || rel == Path::new(".git/packed-refs")
                || rel.starts_with(".git/refs");
        }
        // A .gitignore edit changes what the chains below would say.
        if rel.file_name().is_some_and(|name| name == ".gitignore") {
            self.ignore_chains.clear();
            return true;
        }
        !self.is_ignored(rel, path)
    }

    /// Whether gitignores hide `rel`: global/repo base ignores plus every
    /// `.gitignore` from the root down to the path's directory.
    fn is_ignored(&mut self, rel: &Path, abs: &Path) -> bool {
        let Some(rel_str) = slash_path(rel) else {
            return false; // non-UTF-8 path: refresh rather than guess
        };
        let chain = self.chain_for_dir(rel.parent().unwrap_or(Path::new("")));
        // A directory event must match directory patterns like `target/`.
        // The path may already be gone; treat unknown as a file.
        if abs.is_dir() {
            chain.matches(&format!("{rel_str}/"))
        } else {
            chain.matches(&rel_str)
        }
    }

    fn chain_for_dir(&mut self, rel_dir: &Path) -> Arc<GitIgnoreFile> {
        if let Some(chain) = self.ignore_chains.get(rel_dir) {
            return chain.clone();
        }
        let parent_chain = match rel_dir.parent() {
            Some(parent) => self.chain_for_dir(parent),
            None => self.base_ignores.clone(),
        };
        // Prefix is the directory the file applies to: "" at the root,
        // "sub/dir/" below it — the same form the snapshot walk uses.
        let chain = slash_path(rel_dir)
            .map(|dir| if dir.is_empty() { dir } else { format!("{dir}/") })
            .and_then(|prefix| {
                parent_chain
                    .chain_with_file(&prefix, self.root.join(rel_dir).join(".gitignore"))
                    .map_err(|err| tracing::warn!("unreadable .gitignore skipped: {err}"))
                    .ok()
            })
            .unwrap_or_else(|| parent_chain.clone());
        self.ignore_chains.insert(rel_dir.to_owned(), chain.clone());
        chain
    }
}

/// Slash-separated form of a relative path ("" for the root itself), the
/// shape gitignore matching expects. `None` for non-UTF-8 paths.
fn slash_path(rel: &Path) -> Option<String> {
    let parts: Option<Vec<&str>> = rel
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect();
    Some(parts?.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_filter(root: &Path) -> EventFilter {
        EventFilter::new(
            root.to_owned(),
            root.join(".jj/repo"),
            GitIgnoreFile::empty(),
        )
    }

    #[test]
    fn classifies_op_store_and_state_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut filter = test_filter(&root);

        assert!(filter.is_relevant(&root.join(".jj/repo/op_heads/heads/abc123")));
        assert!(!filter.is_relevant(&root.join(".jj/repo/index/segments/x")));
        assert!(!filter.is_relevant(&root.join(".jj/working_copy/tree_state")));
        assert!(!filter.is_relevant(&root.join(".jj/working_copy/lock")));
        assert!(!filter.is_relevant(&root));
    }

    #[test]
    fn classifies_git_ref_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut filter = test_filter(&root);

        assert!(filter.is_relevant(&root.join(".git/HEAD")));
        assert!(filter.is_relevant(&root.join(".git/refs/heads/main")));
        assert!(filter.is_relevant(&root.join(".git/packed-refs")));
        assert!(!filter.is_relevant(&root.join(".git/HEAD.lock")));
        assert!(!filter.is_relevant(&root.join(".git/refs/heads/main.lock")));
        assert!(!filter.is_relevant(&root.join(".git/index")));
        assert!(!filter.is_relevant(&root.join(".git/objects/ab/cdef")));
    }

    #[test]
    fn working_copy_paths_respect_gitignore_chain() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".gitignore"), "/target\n*.log\n").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/.gitignore"), "generated/\n").unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("sub/generated")).unwrap();
        let mut filter = test_filter(&root);

        assert!(filter.is_relevant(&root.join("src/main.rs")));
        assert!(filter.is_relevant(&root.join("sub/lib.rs")));
        assert!(!filter.is_relevant(&root.join("target/debug/build.o")));
        assert!(!filter.is_relevant(&root.join("debug.log")));
        assert!(!filter.is_relevant(&root.join("sub/generated/out.ts")));

        // Editing a .gitignore is itself a change and resets the cache.
        assert!(filter.is_relevant(&root.join("sub/.gitignore")));
        assert!(filter.ignore_chains.is_empty());
    }

    #[test]
    fn external_repo_dir_uses_op_heads_rule() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let repo_dir = root.join("elsewhere/repo");
        let mut filter = EventFilter::new(
            root.join("workspace"),
            repo_dir.clone(),
            GitIgnoreFile::empty(),
        );

        assert!(filter.is_relevant(&repo_dir.join("op_heads/heads/abc")));
        assert!(!filter.is_relevant(&repo_dir.join("index/x")));
        assert!(!filter.is_relevant(&root.join("unrelated/file.rs")));
    }

    #[test]
    fn watcher_fires_on_file_edits_and_settles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".jj/repo/op_heads/heads")).unwrap();
        std::fs::write(root.join(".gitignore"), "/ignored\n").unwrap();
        std::fs::create_dir_all(root.join("ignored")).unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = RepoWatcher::start(
            root,
            &root.join(".jj/repo"),
            GitIgnoreFile::empty(),
            Box::new(move || {
                tx.send(()).ok();
            }),
        )
        .unwrap();
        // FSEvents needs a beat to arm before it reports changes.
        std::thread::sleep(Duration::from_millis(300));

        std::fs::write(root.join("notes.txt"), "hello").unwrap();
        rx.recv_timeout(Duration::from_secs(10))
            .expect("watcher fires for a tracked-path edit");

        // The quiet period coalesces a burst into one callback.
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();
        rx.recv_timeout(Duration::from_secs(10))
            .expect("watcher fires for the burst");
        assert!(
            rx.recv_timeout(Duration::from_millis(700)).is_err(),
            "burst coalesces into one callback"
        );
    }
}
