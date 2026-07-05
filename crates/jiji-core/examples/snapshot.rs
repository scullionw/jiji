//! Dev tool: print the snapshot JSON for a real repo, or one change's
//! detail (file list) or full content diff — or run a mutation.
//!
//! Usage:
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id>
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --diff
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --compare <from-change-id>
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --describe "text"
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --new|--edit|--abandon|--squash
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --split "description" <path|path@hunk,hunk>...
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --squash-into <dest-change-id> <path|path@hunk,hunk>...
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --rebase <dest-change-id>
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --move <dest-change-id>
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --bookmark <name>
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --move-bookmark <name>
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <old-name> --rename-bookmark <new-name>
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <name> --delete-bookmark
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <name[,name]> --push [remote]
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo --fetch [remote[,remote]]
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <op-id> --revert-op
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <op-id> --restore-op
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo <change-id> --resolve <file-path>
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo --update-stale
//!   cargo run -p jiji-core --example snapshot -- /path/to/repo --watch
//!
//! `--resolve` launches the configured external merge tool (like the app's
//! Resolve action) and blocks until it exits.
//!
//! `--update-stale` recovers a stale working copy (`jj workspace
//! update-stale`), like the app's inbox recovery action.
//!
//! `--watch` syncs (working-copy snapshot + git import), then keeps watching
//! the repo and prints one line per auto-refresh until interrupted — the
//! same backend loop the app's auto-refresh runs.
//!
//! Set `JIJI_MOCK_BACKEND=1` to print the mock backend's data instead
//! (the path still has to be a `.jj` repo).

use jiji_core::snapshot::{DiffLineKind, FileDiffContent, SplitHunk, SplitSelection};
use jiji_core::{JjBackend, MockBackend, RepoBackend};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect(
            "usage: snapshot <repo-path> [change-id] \
             [--diff | --describe <text> | --new | --edit | --abandon | --squash]",
        );
    let change_id = std::env::args().nth(2);
    let mode = std::env::args().nth(3);
    let backend: Box<dyn RepoBackend> =
        if std::env::var("JIJI_MOCK_BACKEND").as_deref() == Ok("1") {
            Box::new(MockBackend::default())
        } else {
            Box::new(JjBackend::default())
        };

    let path = std::path::Path::new(&path);
    if change_id.as_deref() == Some("--watch") {
        return watch_loop(backend.as_ref(), path);
    }
    // `--fetch [remote[,remote]]`: fetch like the app's upstream check —
    // no remotes named resolves them like plain `jj git fetch`.
    if change_id.as_deref() == Some("--fetch") {
        let remotes: Option<Vec<String>> = mode
            .as_deref()
            .map(|names| names.split(',').map(str::to_owned).collect());
        match backend.git_fetch(path, remotes.as_deref()) {
            Ok(outcome) => {
                println!("{}", serde_json::to_string_pretty(&outcome).unwrap());
                return;
            }
            Err(err) => {
                eprintln!("error ({}): {}", err.code(), err);
                std::process::exit(1);
            }
        }
    }
    if change_id.as_deref() == Some("--update-stale") {
        match backend.update_stale_workspace(path) {
            Ok(outcome) => {
                println!("{}", serde_json::to_string_pretty(&outcome).unwrap());
                return;
            }
            Err(err) => {
                eprintln!("error ({}): {}", err.code(), err);
                std::process::exit(1);
            }
        }
    }
    let result = match (change_id, mode.as_deref()) {
        (Some(change_id), Some("--diff")) => backend
            .change_diff(path, &change_id)
            .map(|diff| serde_json::to_string_pretty(&diff).unwrap()),
        (Some(change_id), Some("--compare")) => {
            let from = std::env::args().nth(4).expect("--compare needs the from change id");
            backend
                .compare_diff(path, &from, &change_id)
                .map(|diff| serde_json::to_string_pretty(&diff).unwrap())
        }
        (Some(change_id), Some("--describe")) => {
            let text = std::env::args().nth(4).expect("--describe needs the text");
            backend
                .describe(path, &change_id, &text)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(change_id), Some("--new")) => backend
            .new_change(path, &change_id)
            .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap()),
        (Some(change_id), Some("--edit")) => backend
            .edit_change(path, &change_id)
            .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap()),
        // A comma-separated change-id list sweeps the set as one operation
        // (`jj abandon a b`); a single id keeps the single-change path.
        (Some(change_id), Some("--abandon")) if change_id.contains(',') => {
            let ids: Vec<String> = change_id.split(',').map(str::to_owned).collect();
            backend
                .abandon_changes(path, &ids)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(change_id), Some("--abandon")) => backend
            .abandon_change(path, &change_id)
            .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap()),
        (Some(change_id), Some("--squash")) => backend
            .squash_change(path, &change_id)
            .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap()),
        (Some(change_id), Some("--split")) => {
            let description = std::env::args().nth(4).expect("--split needs the description");
            let args: Vec<String> = std::env::args().skip(5).collect();
            let selection = parse_selection(backend.as_ref(), path, &change_id, &args);
            backend
                .split_change(path, &change_id, &selection, &description)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(change_id), Some("--squash-into")) => {
            let dest = std::env::args().nth(4).expect("--squash-into needs the destination");
            let args: Vec<String> = std::env::args().skip(5).collect();
            let selection = parse_selection(backend.as_ref(), path, &change_id, &args);
            backend
                .squash_into(path, &change_id, &selection, &dest)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(change_id), Some("--rebase")) => {
            let dest = std::env::args().nth(4).expect("--rebase needs the destination");
            backend
                .rebase_change(path, &change_id, &dest)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(change_id), Some("--move")) => {
            let dest = std::env::args().nth(4).expect("--move needs the destination");
            backend
                .move_change(path, &change_id, &dest)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(change_id), Some("--bookmark")) => {
            let name = std::env::args().nth(4).expect("--bookmark needs the name");
            backend
                .create_bookmark(path, &name, &change_id)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(change_id), Some("--move-bookmark")) => {
            let name = std::env::args().nth(4).expect("--move-bookmark needs the name");
            backend
                .move_bookmark(path, &name, &change_id)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        // For these two the second argument is the bookmark name, not a
        // change id.
        (Some(old_name), Some("--rename-bookmark")) => {
            let new_name = std::env::args()
                .nth(4)
                .expect("--rename-bookmark needs the new name");
            backend
                .rename_bookmark(path, &old_name, &new_name)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(name), Some("--delete-bookmark")) => backend
            .delete_bookmark(path, &name)
            .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap()),
        // The second argument is a comma-separated bookmark list; the
        // optional fourth is the remote name (default: the CLI's rules).
        (Some(names), Some("--push")) => {
            let names: Vec<String> = names.split(',').map(str::to_owned).collect();
            let remote = std::env::args().nth(4);
            backend
                .push_bookmarks(path, &names, remote.as_deref())
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        // For these two the second argument is an operation id.
        (Some(op_id), Some("--revert-op")) => backend
            .revert_operation(path, &op_id)
            .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap()),
        (Some(op_id), Some("--restore-op")) => backend
            .restore_operation(path, &op_id)
            .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap()),
        (Some(change_id), Some("--resolve")) => {
            let file = std::env::args().nth(4).expect("--resolve needs the file path");
            backend
                .resolve_conflict(path, &change_id, &file)
                .map(|outcome| serde_json::to_string_pretty(&outcome).unwrap())
        }
        (Some(change_id), _) => backend
            .change_detail(path, &change_id)
            .map(|detail| serde_json::to_string_pretty(&detail).unwrap()),
        (None, _) => backend
            .open(path)
            .map(|snapshot| serde_json::to_string_pretty(&snapshot).unwrap()),
    };
    match result {
        Ok(json) => println!("{json}"),
        Err(err) => {
            eprintln!("error ({}): {}", err.code(), err);
            std::process::exit(1);
        }
    }
}

/// Selection args for `--split` and `--squash-into`: each is a bare path
/// (whole file), or `path@0,2` to pick only those hunks — 0-based indices
/// into the file's current diff as `--diff` prints it, mapped to the hunk
/// coordinates the backend verifies.
fn parse_selection(
    backend: &dyn RepoBackend,
    path: &std::path::Path,
    change_id: &str,
    args: &[String],
) -> Vec<SplitSelection> {
    let diff = if args.iter().any(|a| a.contains('@')) {
        Some(
            backend
                .change_diff(path, change_id)
                .expect("hunk selection needs the change diff"),
        )
    } else {
        None
    };
    let mut selection = Vec::new();
    for arg in args {
        match arg.split_once('@') {
            None => selection.push(SplitSelection {
                path: arg.clone(),
                hunks: None,
            }),
            Some((file, indices)) => {
                let file_diff = diff
                    .as_ref()
                    .unwrap()
                    .files
                    .iter()
                    .find(|f| f.path == file)
                    .unwrap_or_else(|| panic!("{file} is not in the diff"));
                let FileDiffContent::Text { hunks, .. } = &file_diff.content else {
                    panic!("{file} has no text hunks");
                };
                let chosen = indices
                    .split(',')
                    .map(|idx| {
                        let hunk = &hunks[idx.parse::<usize>().expect("hunk index")];
                        SplitHunk {
                            old_start: hunk.old_start,
                            new_start: hunk.new_start,
                            old_lines: hunk
                                .lines
                                .iter()
                                .filter(|l| !matches!(l.kind, DiffLineKind::Added))
                                .count() as u32,
                            new_lines: hunk
                                .lines
                                .iter()
                                .filter(|l| !matches!(l.kind, DiffLineKind::Removed))
                                .count() as u32,
                        }
                    })
                    .collect();
                selection.push(SplitSelection {
                    path: file.to_owned(),
                    hunks: Some(chosen),
                });
            }
        }
    }
    selection
}

/// The app's auto-refresh loop, headless: sync once, then refresh on every
/// watcher tick and print what changed.
fn watch_loop(backend: &dyn RepoBackend, path: &std::path::Path) {
    let describe = |snapshot: &jiji_core::RepoSnapshot| {
        let op = snapshot.operations.first();
        format!(
            "op {} ({}) — {} nodes, working copy {}",
            op.map(|o| o.id.as_str()).unwrap_or("?"),
            op.map(|o| o.description.as_str()).unwrap_or("?"),
            snapshot.nodes.len(),
            snapshot.working_copy,
        )
    };
    fn fail<T>(err: jiji_core::BackendError) -> T {
        eprintln!("error ({}): {}", err.code(), err);
        std::process::exit(1);
    }
    let mut last = backend.refresh(path).unwrap_or_else(|err| fail(err));
    println!("synced: {}", describe(&last));

    let (tx, rx) = std::sync::mpsc::channel();
    let watcher = backend
        .watch(
            path,
            Box::new(move || {
                let _ = tx.send(());
            }),
        )
        .unwrap_or_else(|err| fail(err));
    if watcher.is_none() {
        eprintln!("this backend has nothing to watch");
        std::process::exit(1);
    }
    println!("watching {} (Ctrl-C to stop)", path.display());
    for () in rx {
        match backend.refresh(path) {
            Ok(snapshot) => {
                if snapshot == last {
                    println!("refreshed: no changes");
                } else {
                    println!("refreshed: {}", describe(&snapshot));
                    last = snapshot;
                }
            }
            Err(err) => eprintln!("refresh failed ({}): {}", err.code(), err),
        }
    }
}
