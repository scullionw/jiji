//! Immutable UI snapshot types.
//!
//! These are Jiji-owned DTOs, deliberately denormalized for rendering. They
//! form the stable boundary between the frontend and whatever produces repo
//! state, so the UI never consumes raw jj-lib objects. Regenerate the
//! TypeScript bindings with `bun run bindings` after changing them.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RepoSnapshot {
    pub repo_path: String,
    pub repo_name: String,
    /// Which backend produced this snapshot ("jj-lib", or "mock" when the
    /// app runs with the mock backend).
    pub backend: String,
    pub trunk_bookmark: String,
    /// Change id of the current working-copy node.
    pub working_copy: String,
    pub workspaces: Vec<WorkspaceSummary>,
    pub workstreams: Vec<WorkstreamSummary>,
    pub nodes: Vec<GraphNode>,
    pub bookmarks: Vec<BookmarkState>,
    pub conflicts: Vec<ConflictItem>,
    /// Newest first.
    pub operations: Vec<OperationItem>,
    /// The external merge tool a Resolve action would launch, by its
    /// configured name ("smerge", "meld", …): the user's `ui.merge-editor`,
    /// or Sublime Merge when nothing is configured and it is installed.
    /// `None` hides Resolve affordances — no usable tool is configured.
    pub resolve_tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WorkspaceSummary {
    pub name: String,
    pub is_default: bool,
    pub is_stale: bool,
    pub working_copy_node: Option<String>,
}

/// One mutable line of work: an ordered chain of dependent changes,
/// optionally published through a bookmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WorkstreamSummary {
    pub id: String,
    pub title: String,
    /// Change ids, top-first. The working copy is included when it sits on
    /// top of this stack.
    pub node_ids: Vec<String>,
    pub bookmark: Option<String>,
    pub is_active: bool,
    pub behind_trunk: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GraphNode {
    /// Unique node id — what selection and every backend request use. The
    /// short change id (stable across rewrites) normally; for a divergent
    /// change (several visible commits share one change id) each commit
    /// keys by its short *commit* id instead, jj's own addressing rule for
    /// divergence. The two namespaces never collide: jj renders change ids
    /// in reverse hex (k–z) and commit ids in forward hex (0–9a–f).
    pub id: String,
    /// Short change id, for display. Equals `id` except on divergent nodes.
    pub change_id: String,
    pub commit_id: String,
    /// Empty string when the change has no description yet.
    pub description: String,
    pub author: String,
    /// RFC 3339 timestamp.
    pub timestamp: String,
    pub kind: NodeKind,
    /// Parent change ids, limited to parents present in the snapshot.
    pub parents: Vec<String>,
    /// Closest snapshot ancestors reachable only through history the
    /// snapshot omits (jj's `~` elided revisions). Disjoint from
    /// `parents`; set on immutable bases so the trunk line stays one
    /// connected spine.
    pub elided_parents: Vec<String>,
    pub bookmarks: Vec<String>,
    pub is_empty: bool,
    pub has_conflict: bool,
    /// jj's `??` state: other visible commit(s) share this change id, so
    /// the change id no longer names one commit. Rendered first-class; the
    /// usual resolution is abandoning or rewriting the copies not wanted.
    pub is_divergent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum NodeKind {
    WorkingCopy,
    Mutable,
    Immutable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BookmarkState {
    pub name: String,
    /// Change id the bookmark points at.
    pub target: String,
    pub remote: Option<String>,
    pub sync: SyncState,
    pub is_trunk: bool,
    /// False only for the synthetic entry a remote-only trunk gets: with no
    /// local bookmark behind it there is nothing to move, rename, or delete.
    pub is_local: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SyncState {
    Synced,
    Ahead,
    Behind,
    Diverged,
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ConflictItem {
    pub id: String,
    pub kind: ConflictKind,
    /// Plain-language explanation of what happened.
    pub summary: String,
    /// The change the conflict lives in (file conflicts) or the stale
    /// workspace's working copy, when that change is drawn in the snapshot.
    pub node_id: Option<String>,
    /// Conflicted file paths, repo-relative (file conflicts only). Lists
    /// the tree's own unresolved entries — what `jj resolve --list` shows —
    /// so a conflict inherited from a parent appears even though the
    /// parent-relative diff would not mention the file.
    pub paths: Vec<String>,
    /// Count of conflicted paths dropped past the per-item cap.
    pub more_paths: u32,
    /// Change ids a conflicted bookmark resolved to (bookmark conflicts
    /// only): the candidates the user can repoint it at.
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ConflictKind {
    File,
    Bookmark,
    StaleWorkspace,
}

/// Per-change data too expensive to compute for every node in a snapshot.
/// Fetched lazily when the UI inspects one change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChangeDetail {
    /// Change id this detail was computed for, echoed back from the request.
    pub id: String,
    /// Files changed relative to the parent tree(s), in repo path order.
    pub files: Vec<ChangedFile>,
    /// True when the file list was capped before the diff was exhausted.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChangedFile {
    /// Repo-relative path, `/`-separated.
    pub path: String,
    pub status: FileStatus,
    /// Source path when the file was renamed or copied from elsewhere.
    pub renamed_from: Option<String>,
    pub has_conflict: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FileStatus {
    Added,
    Modified,
    Removed,
    Renamed,
    Copied,
}

/// The full content diff for one change, fetched lazily when the diff
/// surface renders a selection. Heavier than `ChangeDetail`: every file's
/// contents are materialized and diffed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ChangeDiff {
    /// Change id this diff was computed for, echoed back from the request.
    pub id: String,
    /// Set when this diff compares against another change's tree instead of
    /// the parent tree(s) — commit-to-commit or stack-relative comparison.
    /// Echoes the requested from-change id back so the UI can match a
    /// response to the comparison it currently shows.
    pub from: Option<String>,
    /// Files changed relative to the parent tree(s), in repo path order.
    pub files: Vec<FileDiff>,
    /// True when the file list was capped before the diff was exhausted.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FileDiff {
    /// Repo-relative path, `/`-separated.
    pub path: String,
    pub status: FileStatus,
    /// Source path when the file was renamed or copied from elsewhere.
    pub renamed_from: Option<String>,
    pub has_conflict: bool,
    pub content: FileDiffContent,
}

/// What the diff surface can render for one file. Conflicted files arrive as
/// `text` with jj's conflict markers materialized into the content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export)]
pub enum FileDiffContent {
    /// Unified hunks ready to render. An empty hunk list is a contentless
    /// change (empty file added, or a mode-only change).
    Text { hunks: Vec<DiffHunk>, truncated: bool },
    /// Either side looks binary; no line diff is possible.
    Binary,
    /// Either side exceeds the per-file size limit for diffing.
    TooLarge,
    /// Content skipped because the change as a whole exceeded the diff
    /// budget; the file still appears in the list for navigation.
    Omitted,
}

/// One unified hunk: a run of changed lines plus surrounding context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DiffHunk {
    /// 1-based line number of the hunk's first line on the old side.
    pub old_start: u32,
    /// 1-based line number of the hunk's first line on the new side.
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// The line's text (without its trailing newline) split into intraline
    /// segments; `changed` segments are the word-level differences inside a
    /// modified run. A fully added/removed line is one changed segment.
    pub segments: Vec<DiffSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DiffLineKind {
    Context,
    Removed,
    Added,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DiffSegment {
    pub text: String,
    pub changed: bool,
}

/// What a mutation did, surfaced as the operation breadcrumb after the
/// snapshot refreshes. Every write action returns one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MutationOutcome {
    /// Short hex id of the jj operation this mutation recorded; `None` when
    /// the request was a no-op and nothing changed.
    pub operation_id: Option<String>,
    /// Plain-language description of what happened, e.g. "Described pqsrwxyz".
    pub summary: String,
    /// Change id the workbench selection should follow after the refresh:
    /// the new working copy for `new`, the parent after abandon/squash, the
    /// target itself otherwise.
    pub target_change: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OperationItem {
    pub id: String,
    pub description: String,
    /// RFC 3339 timestamp.
    pub timestamp: String,
    pub is_current: bool,
    /// `user@host` that recorded the operation.
    pub user: String,
    /// Pure working-copy snapshot operations; the timeline renders them
    /// quieter and collapses runs of them.
    pub is_snapshot: bool,
    /// Plain-language summaries of what changed vs the parent operation.
    /// Bookmark moves are reported at change-id granularity, so a rewrite
    /// that keeps a bookmark on the same change is not a "move".
    pub effects: Vec<OpEffect>,
    /// Count of effects dropped past the per-operation cap.
    pub more_effects: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OpEffect {
    pub kind: OpEffectKind,
    /// E.g. "main moved", "main@origin updated", "working copy moved".
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum OpEffectKind {
    Bookmark,
    RemoteBookmark,
    WorkingCopy,
}
