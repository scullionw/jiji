//! Reading diffs: what names one, the small cache of the ones already
//! read and laid out, and the reads in flight, which everyone who asks for
//! the same diff shares.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures::future::{FutureExt as _, Shared};
use gpui::{AppContext as _, Context, Task};

use super::AppModel;
use crate::diff::model::Prepared;
use crate::host::{CommandError, HostResult};

/// Which diff: a repo at one operation, a change, and what it is measured
/// from (`None` is its parent).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiffKey {
    pub repo: String,
    pub op: String,
    pub from: Option<String>,
    pub id: String,
}

impl DiffKey {
    /// The same comparison, whatever operation it was read at.
    pub fn same_comparison(&self, other: &DiffKey) -> bool {
        self.repo == other.repo && self.from == other.from && self.id == other.id
    }
}

/// A read under way, for whoever is waiting on it.
pub type PendingDiff = Shared<Task<HostResult<Prepared>>>;

/// What asking for a diff gets.
pub enum DiffRead {
    /// Read and laid out already: it can go on screen now.
    Ready(Prepared),
    /// On its way, by this request or one before it.
    Pending(PendingDiff),
}

/// Diffs are cached per repo operation, laid out: re-selecting a change
/// shows it without a read or a rebuild, and snapshot republishes that
/// record no new operation (view switches, converged refreshes) skip the
/// backend entirely.
#[derive(Default)]
pub struct DiffCache {
    entries: Vec<(DiffKey, Prepared)>,
    /// Reads under way, by what they read: a selection that arrives while
    /// its diff is being prefetched waits on that read.
    pub(crate) inflight: HashMap<DiffKey, PendingDiff>,
}

const MAX_CACHED_DIFFS: usize = 16;

impl DiffCache {
    pub fn get(&mut self, key: &DiffKey) -> Option<Prepared> {
        let at = self.entries.iter().position(|(cached, _)| cached == key)?;
        // Most recently used last.
        let entry = self.entries.remove(at);
        let prepared = entry.1.clone();
        self.entries.push(entry);
        Some(prepared)
    }

    fn contains(&self, key: &DiffKey) -> bool {
        self.entries.iter().any(|(cached, _)| cached == key)
    }

    fn insert(&mut self, key: DiffKey, prepared: Prepared) {
        self.entries.retain(|(cached, _)| *cached != key);
        self.entries.push((key, prepared));
        if self.entries.len() > MAX_CACHED_DIFFS {
            self.entries.remove(0);
        }
    }
}

impl AppModel {
    /// One diff, from the cache when it has it, else from the read in
    /// flight for it or a new one.
    pub fn fetch_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) -> DiffRead {
        match self.diffs.get(&key) {
            Some(prepared) => DiffRead::Ready(prepared),
            None => DiffRead::Pending(self.read_diff(key, cx)),
        }
    }

    /// Warm the cache for a diff the reader is one step away from.
    pub fn prefetch_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) {
        if !self.diffs.contains(&key) {
            // Nobody waits on it: the read lands in the cache by itself.
            drop(self.read_diff(key, cx));
        }
    }

    /// The backend read and the layout, both off the UI thread, started
    /// once however many ask; the result lands in the cache.
    fn read_diff(&mut self, key: DiffKey, cx: &mut Context<Self>) -> PendingDiff {
        if let Some(pending) = self.diffs.inflight.get(&key) {
            return pending.clone();
        }
        let host = self.host.clone();
        let read = key.clone();
        let pending = cx
            .background_spawn(async move {
                let path = Path::new(&read.repo);
                let backend = host.backend();
                let diff = match &read.from {
                    Some(from) => backend.compare_diff(path, from, &read.id),
                    None => backend.change_diff(path, &read.id),
                }?;
                Ok::<_, CommandError>(Prepared::new(Arc::new(diff)))
            })
            .shared();
        self.diffs.inflight.insert(key.clone(), pending.clone());
        cx.spawn({
            let pending = pending.clone();
            async move |this, cx| {
                let read = pending.await;
                let _ = this.update(cx, |this, _| {
                    this.diffs.inflight.remove(&key);
                    if let Ok(prepared) = read {
                        this.diffs.insert(key, prepared);
                    }
                });
            }
        })
        .detach();
        pending
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use gpui::TestAppContext;

    use super::{DiffKey, DiffRead};

    #[gpui::test]
    fn a_prefetch_and_a_selection_of_one_diff_share_its_read(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("diff-read-shared");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let snapshot = jiji_core::mock::mock_snapshot(Path::new(&repo));
        let key = DiffKey {
            repo: repo.clone(),
            op: snapshot.operations[0].id.clone(),
            from: None,
            id: "mvkortsq".into(),
        };
        // The graph's neighbour, warmed; then selected before it lands.
        model.update(cx, |model, cx| model.prefetch_diff(key.clone(), cx));
        let prefetching = model.read_with(cx, |model, _| model.diffs.inflight[&key].clone());
        let selected = model.update(cx, |model, cx| model.fetch_diff(key.clone(), cx));
        let DiffRead::Pending(selected) = selected else {
            panic!("nothing has landed yet");
        };
        assert!(
            selected.ptr_eq(&prefetching),
            "a second read of the same diff"
        );
        cx.run_until_parked();
        let shown = selected
            .peek()
            .cloned()
            .expect("the read is done")
            .expect("the mock reads it");

        // Shown once, it is highlighted once.
        let executor = cx.executor();
        let highlighting = shown.highlighting(&executor).as_ptr();
        cx.run_until_parked();

        // Back to it later: the same laid-out document, at once, with the
        // colours its first showing gave it.
        let again = model.update(cx, |model, cx| model.fetch_diff(key.clone(), cx));
        let DiffRead::Ready(again) = again else {
            panic!("the cache has it");
        };
        assert!(Arc::ptr_eq(again.doc(), shown.doc()));
        let doc = again.doc();
        assert!((0..doc.lines.len() as u32).any(|line| !doc.marks(line).is_empty()));
        assert_eq!(again.highlighting(&executor).as_ptr(), highlighting);
        model.read_with(cx, |model, _| assert!(model.diffs.inflight.is_empty()));
        let _ = std::fs::remove_dir_all(dir);
    }

    // ── Ported from the Tauri app's diffcache.test.ts ──

    use super::{DiffCache, MAX_CACHED_DIFFS};
    use crate::diff::model::Prepared;

    fn key(repo: &str, op: &str, from: Option<&str>, id: &str) -> DiffKey {
        DiffKey {
            repo: repo.into(),
            op: op.into(),
            from: from.map(str::to_owned),
            id: id.into(),
        }
    }

    fn prepared(id: &str) -> Prepared {
        Prepared::new(Arc::new(jiji_core::snapshot::ChangeDiff {
            id: id.into(),
            from: None,
            files: vec![],
            truncated: false,
        }))
    }

    fn same(a: &Prepared, b: &Prepared) -> bool {
        Arc::ptr_eq(a.doc(), b.doc())
    }

    #[test]
    fn the_cache_keys_on_repo_operation_base_and_change() {
        let mut cache = DiffCache::default();
        // Nothing until something is read.
        assert!(cache.get(&key("/repo", "op", None, "zzz")).is_none());
        let (one, two) = (prepared("same"), prepared("same"));
        cache.insert(key("/repo one", "op", None, "same"), one.clone());
        cache.insert(key("/repo two", "op", None, "same"), two.clone());
        assert!(same(
            &cache.get(&key("/repo one", "op", None, "same")).unwrap(),
            &one
        ));
        assert!(same(
            &cache.get(&key("/repo two", "op", None, "same")).unwrap(),
            &two
        ));
        // A comparison is its own entry, and a new operation reads anew.
        let compared = prepared("same");
        cache.insert(
            key("/repo one", "op", Some("base"), "same"),
            compared.clone(),
        );
        assert!(same(
            &cache
                .get(&key("/repo one", "op", Some("base"), "same"))
                .unwrap(),
            &compared
        ));
        assert!(same(
            &cache.get(&key("/repo one", "op", None, "same")).unwrap(),
            &one
        ));
        assert!(cache
            .get(&key("/repo one", "op-next", None, "same"))
            .is_none());
    }

    #[test]
    fn past_its_cap_the_cache_lets_go_of_what_was_used_longest_ago() {
        let mut cache = DiffCache::default();
        let at = |id: &str| key("/repo", "op", None, id);
        cache.insert(at("keep"), prepared("keep"));
        for fill in 0..MAX_CACHED_DIFFS - 1 {
            cache.insert(at(&format!("fill{fill}")), prepared("fill"));
        }
        // Shown again: the oldest read is the one in use now.
        assert!(cache.get(&at("keep")).is_some());
        cache.insert(at("one-more"), prepared("one-more"));
        assert!(cache.get(&at("keep")).is_some());
        assert!(cache.get(&at("fill0")).is_none());
    }

    fn mock_key(repo: &str, from: Option<&str>, id: &str) -> DiffKey {
        let snapshot = jiji_core::mock::mock_snapshot(Path::new(repo));
        key(repo, &snapshot.operations[0].id, from, id)
    }

    #[gpui::test]
    fn reads_of_one_change_in_two_repos_are_two_reads(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("diff-read-two-repos");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let other = dir.join("other");
        std::fs::create_dir_all(other.join(".jj")).unwrap();
        let other = other.to_string_lossy().into_owned();
        let (first, second) = (
            mock_key(&repo, None, "mvkortsq"),
            mock_key(&other, None, "mvkortsq"),
        );
        let reads = model.update(cx, |model, cx| {
            (
                model.fetch_diff(first.clone(), cx),
                model.fetch_diff(second.clone(), cx),
            )
        });
        let (DiffRead::Pending(a), DiffRead::Pending(b)) = reads else {
            panic!("nothing is cached yet");
        };
        assert!(!a.ptr_eq(&b));
        cx.run_until_parked();
        model.update(cx, |model, _| {
            let (a, b) = (
                model.diffs.get(&first).unwrap(),
                model.diffs.get(&second).unwrap(),
            );
            assert!(!same(&a, &b));
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_comparison_is_read_from_its_base_and_cached_apart(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("diff-read-compare");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let (parent, compared) = (
            mock_key(&repo, None, "lwnouzpy"),
            mock_key(&repo, Some("uvkmrtpz"), "lwnouzpy"),
        );
        model.update(cx, |model, cx| {
            model.prefetch_diff(parent.clone(), cx);
            model.prefetch_diff(compared.clone(), cx);
        });
        cx.run_until_parked();
        model.update(cx, |model, _| {
            let parent = model.diffs.get(&parent).expect("the parent diff landed");
            let compared = model.diffs.get(&compared).expect("the comparison landed");
            assert_eq!(parent.diff().from, None);
            assert_eq!(compared.diff().from.as_deref(), Some("uvkmrtpz"));
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_failed_read_is_not_kept_and_a_prefetch_shrugs_one_off(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("diff-read-failure");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let at = mock_key(&repo, None, "mvkortsq");
        let (jj, away) = (Path::new(&repo).join(".jj"), dir.join("jj-away"));
        std::fs::rename(&jj, &away).unwrap();
        let read = model.update(cx, |model, cx| model.fetch_diff(at.clone(), cx));
        let DiffRead::Pending(read) = read else {
            panic!("nothing is cached yet");
        };
        // A warm-up of something else that fails is nobody's error.
        let warm = mock_key(&repo, None, "lwnouzpy");
        model.update(cx, |model, cx| model.prefetch_diff(warm.clone(), cx));
        cx.run_until_parked();
        assert!(read.peek().unwrap().is_err());
        model.update(cx, |model, _| {
            assert!(model.diffs.get(&at).is_none() && model.diffs.get(&warm).is_none());
            assert!(model.diffs.inflight.is_empty());
        });
        // The repository is back: asking again reads again, and it lands.
        std::fs::rename(&away, &jj).unwrap();
        let again = model.update(cx, |model, cx| model.fetch_diff(at.clone(), cx));
        assert!(matches!(again, DiffRead::Pending(_)));
        cx.run_until_parked();
        model.update(cx, |model, _| assert!(model.diffs.get(&at).is_some()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn a_prefetch_warms_the_cache_once(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("diff-prefetch");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let at = mock_key(&repo, None, "mvkortsq");
        model.update(cx, |model, cx| model.prefetch_diff(at.clone(), cx));
        cx.run_until_parked();
        let read = model.update(cx, |model, cx| model.fetch_diff(at.clone(), cx));
        assert!(matches!(read, DiffRead::Ready(_)));
        // Warming what is there already starts no read.
        model.update(cx, |model, cx| {
            model.prefetch_diff(at.clone(), cx);
            assert!(model.diffs.inflight.is_empty());
        });
        let _ = std::fs::remove_dir_all(dir);
    }
}
