//! Shapes the snapshot's bookmarks into the manager's model: a Local group
//! plus one group per remote (remote-only refs — teammates' work and
//! leftovers), searchable, cleanup candidates surfaced.

use std::cmp::Ordering;

use jiji_core::snapshot::BookmarkState;

#[derive(Debug, PartialEq)]
pub struct BookmarkGroups<'a> {
    pub local: Vec<&'a BookmarkState>,
    pub remotes: Vec<(&'a str, Vec<&'a BookmarkState>)>,
}

/// Well-known remotes first, the rest alphabetical — the same preference
/// order the backend uses to pick a local bookmark's tracked remote.
fn remote_rank(remote: &str) -> u8 {
    match remote {
        "origin" => 0,
        "upstream" => 1,
        _ => 2,
    }
}

fn matches(bookmark: &BookmarkState, tokens: &[String]) -> bool {
    let haystack = [
        bookmark.name.as_str(),
        bookmark.remote.as_deref().unwrap_or(""),
        &bookmark.target_description,
        &bookmark.target_author,
        &bookmark.target,
    ]
    .join(" ")
    .to_lowercase();
    tokens.iter().all(|token| haystack.contains(token.as_str()))
}

fn stamp(bookmark: &BookmarkState) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&bookmark.target_timestamp)
        .map_or(0, |at| at.timestamp_millis())
}

/// Trunk pins first; the rest read newest-first, like the graph. Missing
/// timestamps sink to the bottom.
fn by_recency(a: &&BookmarkState, b: &&BookmarkState) -> Ordering {
    b.is_trunk
        .cmp(&a.is_trunk)
        .then(stamp(b).cmp(&stamp(a)))
        .then(a.name.cmp(&b.name))
}

/// The manager's groups for one snapshot, filtered by the search query.
pub fn group_bookmarks<'a>(bookmarks: &'a [BookmarkState], query: &str) -> BookmarkGroups<'a> {
    let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let shown: Vec<&BookmarkState> = bookmarks
        .iter()
        .filter(|bookmark| tokens.is_empty() || matches(bookmark, &tokens))
        .collect();
    let mut local: Vec<&BookmarkState> = shown.iter().copied().filter(|b| b.is_local).collect();
    local.sort_by(by_recency);

    let mut remotes: Vec<(&str, Vec<&BookmarkState>)> = Vec::new();
    for bookmark in shown {
        let (false, Some(remote)) = (bookmark.is_local, bookmark.remote.as_deref()) else {
            continue;
        };
        match remotes.iter_mut().find(|(name, _)| *name == remote) {
            Some((_, list)) => list.push(bookmark),
            None => remotes.push((remote, vec![bookmark])),
        }
    }
    remotes.sort_by(|(a, _), (b, _)| remote_rank(a).cmp(&remote_rank(b)).then(a.cmp(b)));
    for (_, list) in &mut remotes {
        list.sort_by(by_recency);
    }
    BookmarkGroups { local, remotes }
}

/// Cleanup candidates: every entry whose target is already trunk ancestry.
/// Locals first (deleting them is the common sweep), then remote-only refs.
pub fn merged_bookmarks(bookmarks: &[BookmarkState]) -> Vec<&BookmarkState> {
    let mut merged: Vec<&BookmarkState> = bookmarks.iter().filter(|b| b.is_merged).collect();
    merged.sort_by(|a, b| b.is_local.cmp(&a.is_local).then(by_recency(a, b)));
    merged
}

/// A stable key: local and remote-only entries can share a name.
pub fn bookmark_key(bookmark: &BookmarkState) -> String {
    if bookmark.is_local {
        format!("local:{}", bookmark.name)
    } else {
        format!(
            "{}:{}",
            bookmark.remote.as_deref().unwrap_or(""),
            bookmark.name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::snapshot::SyncState;

    fn mark(name: &str, remote: Option<&str>, local: bool, stamp: &str) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: format!("id-{name}"),
            remote: remote.map(str::to_owned),
            sync: SyncState::Synced,
            is_trunk: name == "main" && local,
            is_local: local,
            tracked: false,
            is_merged: false,
            target_description: format!("work on {name}"),
            target_author: "Ada".into(),
            target_timestamp: stamp.into(),
        }
    }

    #[test]
    fn groups_pin_trunk_then_read_newest_first_with_origin_leading() {
        let marks = vec![
            mark("old", None, true, "2026-01-01T00:00:00Z"),
            mark("main", Some("origin"), true, "2025-01-01T00:00:00Z"),
            mark("new", None, true, "2026-03-01T00:00:00Z"),
            mark("undated", None, true, ""),
            mark("theirs", Some("zeta"), false, "2026-02-01T00:00:00Z"),
            mark("wip", Some("origin"), false, "2026-02-01T00:00:00Z"),
            mark("fork", Some("upstream"), false, "2026-02-01T00:00:00Z"),
        ];
        let groups = group_bookmarks(&marks, "");
        let names =
            |list: &[&BookmarkState]| list.iter().map(|b| b.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&groups.local), ["main", "new", "old", "undated"]);
        assert_eq!(
            groups
                .remotes
                .iter()
                .map(|(remote, _)| *remote)
                .collect::<Vec<_>>(),
            ["origin", "upstream", "zeta"]
        );
    }

    #[test]
    fn the_filter_needs_every_token_somewhere() {
        let marks = vec![
            mark("feature", None, true, ""),
            mark("fix", Some("origin"), false, ""),
        ];
        let groups = group_bookmarks(&marks, "ada FIX");
        assert!(groups.local.is_empty());
        assert_eq!(groups.remotes.len(), 1);
        assert!(group_bookmarks(&marks, "nothing").remotes.is_empty());
    }

    #[test]
    fn cleanup_lists_merged_work_locals_first() {
        let mut marks = vec![
            mark("gone", Some("origin"), false, "2026-03-01T00:00:00Z"),
            mark("done", None, true, "2026-01-01T00:00:00Z"),
            mark("live", None, true, "2026-01-01T00:00:00Z"),
        ];
        marks[0].is_merged = true;
        marks[1].is_merged = true;
        let merged = merged_bookmarks(&marks);
        assert_eq!(
            merged.iter().map(|b| bookmark_key(b)).collect::<Vec<_>>(),
            ["local:done", "origin:gone"]
        );
    }

    // ── Ported from the Tauri app's bookmarks.test.ts ──

    fn fixture() -> Vec<BookmarkState> {
        let at = |name: &str, stamp: &str| BookmarkState {
            target_description: String::new(),
            target_author: String::new(),
            sync: SyncState::LocalOnly,
            ..mark(name, None, true, stamp)
        };
        vec![
            at("feature-x", "2026-06-10T10:00:00Z"),
            BookmarkState {
                remote: Some("origin".into()),
                sync: SyncState::Synced,
                tracked: true,
                ..at("main", "2026-06-01T10:00:00Z")
            },
            BookmarkState {
                is_merged: true,
                ..at("landed", "2026-05-01T10:00:00Z")
            },
            BookmarkState {
                is_local: false,
                remote: Some("origin".into()),
                sync: SyncState::RemoteOnly,
                target_author: "alice".into(),
                ..at("alice/api", "2026-06-09T10:00:00Z")
            },
            BookmarkState {
                is_local: false,
                remote: Some("upstream".into()),
                sync: SyncState::RemoteOnly,
                is_merged: true,
                ..at("old-spike", "2026-04-01T10:00:00Z")
            },
        ]
    }

    fn named(list: &[&BookmarkState]) -> Vec<String> {
        list.iter().map(|b| b.name.clone()).collect()
    }

    #[test]
    fn locals_and_each_remote_group_apart_trunk_pinned_newest_first() {
        let marks = fixture();
        let groups = group_bookmarks(&marks, "");
        assert_eq!(named(&groups.local), ["main", "feature-x", "landed"]);
        let remotes: Vec<&str> = groups.remotes.iter().map(|(remote, _)| *remote).collect();
        assert_eq!(remotes, ["origin", "upstream"]);
        assert_eq!(named(&groups.remotes[0].1), ["alice/api"]);
    }

    #[test]
    fn the_filter_reads_names_authors_and_remotes() {
        let marks = fixture();
        let alice = group_bookmarks(&marks, "alice");
        assert_eq!(named(&alice.remotes[0].1), ["alice/api"]);
        assert!(alice.local.is_empty());
        assert_eq!(group_bookmarks(&marks, "upstream").remotes.len(), 1);
        assert_eq!(
            named(&group_bookmarks(&marks, "feature").local),
            ["feature-x"]
        );
    }
}
