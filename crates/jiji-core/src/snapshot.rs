//! Immutable UI snapshot types.
//!
//! These are Jiji-owned DTOs, deliberately denormalized for rendering. They
//! form the stable boundary between the interface and whatever produces
//! repo state, so the UI never consumes raw jj-lib objects.

use serde::{Deserialize, Serialize};

/// Which slice of the repo a snapshot renders. `Workstreams` is the working
/// default; the others are read-oriented lenses over more history. `Team`
/// and `Recent` add commits on top of the default set; `Mine`, `Trunk`,
/// `All`, and `Custom` replace it (keeping the working copy pinned so the
/// "where am I" anchor never disappears).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SnapshotView {
    /// Your mutable work on the immutable bases it sits on.
    Workstreams,
    /// Workstreams plus everyone's branches: every bookmarked head, local
    /// or remote, with its commits down to where it left trunk.
    Team,
    /// Workstreams plus everything committed in the last two weeks.
    Recent,
    /// Everything the configured user authored, including landed work.
    Mine,
    /// Trunk's own history, newest first.
    Trunk,
    /// The entire visible history.
    All,
    /// A user-written revset, parsed with the repo's alias context.
    Custom { revset: String },
}

impl Default for SnapshotView {
    fn default() -> Self {
        Self::Workstreams
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSnapshot {
    pub repo_path: String,
    pub repo_name: String,
    /// Which backend produced this snapshot ("jj-lib", or "mock" when the
    /// app runs with the mock backend).
    pub backend: String,
    /// The view this snapshot renders, echoed from the request.
    pub view: SnapshotView,
    /// True when the view's node cap was hit and older commits were dropped.
    pub truncated: bool,
    pub trunk_bookmark: String,
    /// Change id of the current working-copy node.
    pub working_copy: String,
    pub workspaces: Vec<WorkspaceSummary>,
    pub workstreams: Vec<WorkstreamSummary>,
    pub nodes: Vec<GraphNode>,
    pub bookmarks: Vec<BookmarkState>,
    /// The repo's configured git remotes (jj's hidden colocated `git`
    /// remote excluded). Forge detection — which GitHub project does this
    /// repo publish to — reads these; `jiji-core` itself only reports them.
    pub git_remotes: Vec<GitRemote>,
    pub conflicts: Vec<ConflictItem>,
    /// Newest first.
    pub operations: Vec<OperationItem>,
    /// The external merge tool a Resolve action would launch, by its
    /// configured name ("smerge", "meld", …): the user's `ui.merge-editor`,
    /// or Sublime Merge when nothing is configured and it is installed.
    /// `None` hides Resolve affordances — no usable tool is configured.
    pub resolve_tool: Option<String>,
    /// The bookmarks here that were someone else's branches and were taken
    /// over with Jiji: each is a local bookmark that tracks the branch on
    /// its remote. Empty for most repositories.
    #[serde(default)]
    pub taken_over: Vec<TakenOver>,
}

/// A branch someone else made that was taken over here: it is a bookmark
/// of this repository now, pushed and landed with the stack it is in, and
/// its pull request stays its author's on GitHub. Recorded when it is taken
/// over and until it is given back, so that every plan that pushes it can
/// say whose branch it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakenOver {
    pub bookmark: String,
    pub remote: String,
    /// The pull request it was taken over for.
    pub number: u64,
    /// Its author's login, as GitHub named it then. `None` when GitHub
    /// named none (a deleted account).
    #[serde(default)]
    pub author: Option<String>,
}

impl TakenOver {
    /// "#9 by @kim", or "#9" when the author is not known.
    pub fn by_author(&self) -> String {
        match &self.author {
            Some(login) => format!("#{} by @{login}", self.number),
            None => format!("#{}", self.number),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub name: String,
    pub is_default: bool,
    /// The workspace this snapshot was produced from — the one the app has
    /// open, whose staleness is detectable and recoverable. Sibling
    /// workspaces keep their working-copy state in their own roots, which
    /// this workspace cannot see, so their `is_stale` stays `false`.
    pub is_current: bool,
    pub is_stale: bool,
    pub working_copy_node: Option<String>,
}

/// One mutable line of work: an ordered chain of dependent changes,
/// optionally published through a bookmark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    /// Unique node id — what selection and every backend request use. The
    /// short change id (stable across rewrites) normally; when the change
    /// id does not name this commit alone — a divergent change (several
    /// visible commits share one change id) or a hidden commit (a remote
    /// bookmark's pre-rewrite target) — the commit keys by its short
    /// *commit* id instead, jj's own addressing rule for both. The two
    /// namespaces never collide: jj renders change ids in reverse hex (k–z)
    /// and commit ids in forward hex (0–9a–f).
    pub id: String,
    /// Short change id, for display. Equals `id` except on divergent and
    /// hidden nodes.
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
    /// Git tags on this commit (colocated repos), name order. Read-only
    /// markers — Jiji offers no tag management.
    pub tags: Vec<String>,
    pub is_empty: bool,
    pub has_conflict: bool,
    /// jj's `??` state: other visible commit(s) share this change id, so
    /// the change id no longer names one commit. Rendered first-class; the
    /// usual resolution is abandoning or rewriting the copies not wanted.
    pub is_divergent: bool,
    /// The user's own change, immutable only because a remote branch that
    /// is no bookmark here builds on it: someone else pushed a branch on
    /// top of this stack (or the same person did, from another clone),
    /// and jj treats everything an untracked remote bookmark contains as
    /// immutable. The change sits under a local bookmark and is not part
    /// of trunk. Its `kind` stays `Immutable`, since nothing here may
    /// rewrite it by hand; stacks still count it as theirs to publish and
    /// land, which is what tells it from landed history.
    #[serde(default)]
    pub held: bool,
    /// Divergent only on account of someone else's branch: every other
    /// visible commit of this change is immutable here and not part of
    /// trunk, kept in view by a branch that is no bookmark here and was
    /// built on an earlier version of the change. Nothing here can
    /// abandon those copies, so this is not a divergence to resolve
    /// before publishing: it ends when that branch is rebased onto this
    /// commit (Land does that with the stack) or is deleted.
    #[serde(default)]
    pub old_copies_held: bool,
}

impl GraphNode {
    /// Divergent in a way that is the user's to resolve: another visible
    /// commit of this change is theirs to abandon or rewrite.
    pub fn diverged_here(&self) -> bool {
        self.is_divergent && !self.old_copies_held
    }

    /// History nothing here works on any more: immutable, and not a
    /// change of the user's own that someone else's branch holds (`held`).
    /// Where a stack ends when it is walked down from its head.
    pub fn is_settled(&self) -> bool {
        self.kind == NodeKind::Immutable && !self.held
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    WorkingCopy,
    Mutable,
    Immutable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookmarkState {
    pub name: String,
    /// Change id the bookmark points at.
    pub target: String,
    pub remote: Option<String>,
    pub sync: SyncState,
    pub is_trunk: bool,
    /// False for remote-only entries — a remote branch with no local
    /// bookmark behind it (a teammate's work, or something to clean up).
    /// There is nothing local to move, rename, or delete; the offered
    /// actions are track and delete-on-remote.
    pub is_local: bool,
    /// Local entries: whether a tracked remote counterpart exists.
    /// Remote-only entries: whether jj tracks the remote ref (an untracked
    /// ref is what a plain fetch leaves behind for branches jj never saw).
    pub tracked: bool,
    /// The target is an ancestor of trunk (and not trunk itself): the work
    /// has landed, so deleting this bookmark is cleanup, not loss.
    pub is_merged: bool,
    /// Target commit metadata for surfaces that render bookmarks without
    /// the commit drawn (the bookmarks manager). First line of the
    /// description; empty when unavailable.
    pub target_description: String,
    pub target_author: String,
    /// RFC 3339 timestamp of the target commit; empty when unavailable.
    pub target_timestamp: String,
}

/// One configured git remote, as `git remote -v` would list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    Synced,
    Ahead,
    Behind,
    Diverged,
    LocalOnly,
    /// A remote-only entry: the branch exists on the remote with no local
    /// bookmark behind it, so there is no local position to compare.
    RemoteOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    /// The workspace a stale-workspace item is about. The UI offers the
    /// guided recovery only when this names the current workspace — the one
    /// `update_stale_workspace` can act on.
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKind {
    File,
    Bookmark,
    StaleWorkspace,
}

/// Which side of a 2-sided file conflict a take verb keeps — the CLI's
/// built-in `:ours`/`:theirs` merge tools: "ours" is side #1 of the
/// conflict, "theirs" side #2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictSide {
    Ours,
    Theirs,
}

/// Per-change data too expensive to compute for every node in a snapshot.
/// Fetched lazily when the UI inspects one change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeDetail {
    /// Change id this detail was computed for, echoed back from the request.
    pub id: String,
    /// Files changed relative to the parent tree(s), in repo path order.
    pub files: Vec<ChangedFile>,
    /// True when the file list was capped before the diff was exhausted.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    /// Repo-relative path, `/`-separated.
    pub path: String,
    pub status: FileStatus,
    /// Source path when the file was renamed or copied from elsewhere.
    pub renamed_from: Option<String>,
    pub has_conflict: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    /// Repo-relative path, `/`-separated.
    pub path: String,
    pub status: FileStatus,
    /// Source path when the file was renamed or copied from elsewhere.
    pub renamed_from: Option<String>,
    pub has_conflict: bool,
    pub content: FileDiffContent,
}

/// What the diff surface can render for one file. A conflicted file whose
/// sides are all text arrives as `conflict` — its merge structure rather
/// than a parent-relative diff of jj's marker text; binary or oversized
/// conflicts fall back to the marker-text pipeline like any other content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FileDiffContent {
    /// Unified hunks ready to render. An empty hunk list is a contentless
    /// change (empty file added, or a mode-only change).
    Text { hunks: Vec<DiffHunk>, truncated: bool },
    /// The file is conflicted in the shown tree: resolved runs (trimmed to
    /// context like diff hunks) interleaved with conflict regions, in file
    /// order. Read-only — the choice this view informs is made by the
    /// resolve verbs.
    Conflict {
        sections: Vec<ConflictSection>,
        truncated: bool,
    },
    /// Binary content at renderable image paths, compared as pictures.
    /// A missing side means the file was added or removed.
    Image {
        old: Option<ImageContent>,
        new: Option<ImageContent>,
    },
    /// Either side looks binary and is not a renderable image; the byte
    /// sizes still tell the story of the change. A `None` size is an
    /// absent side (added or removed file).
    #[serde(rename_all = "camelCase")]
    Binary {
        old_size: Option<u64>,
        new_size: Option<u64>,
    },
    /// Either side exceeds the per-file size limit for diffing.
    TooLarge,
    /// Content skipped because the change as a whole exceeded the diff
    /// budget; the file still appears in the list for navigation.
    Omitted,
}

/// One side of an image diff, ready for an `<img>` element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// MIME type inferred from this side's file extension.
    pub mime: String,
    /// This side's full byte size.
    pub size: u64,
    /// The image bytes as base64, sized for a `data:` URI; None when the
    /// side exceeds the inline cap and renders as metadata only.
    pub data: Option<String>,
}

/// One unified hunk: a run of changed lines plus surrounding context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
    /// 1-based line number of the hunk's first line on the old side.
    pub old_start: u32,
    /// 1-based line number of the hunk's first line on the new side.
    pub new_start: u32,
    /// The nearest enclosing declaration above the hunk (git's
    /// "funcname"), labeling the elision band the surface renders before
    /// it; None when the language has no line-scan or nothing matched.
    pub context: Option<String>,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// The line's text (without its trailing newline) split into intraline
    /// segments; `changed` segments are the word-level differences against
    /// the paired line. An unpaired added/removed line is one changed
    /// segment.
    pub segments: Vec<DiffSegment>,
    /// Index into the same hunk's `lines` of this line's counterpart: the
    /// removed line an added line replaces, and vice versa. Pairing is by
    /// content similarity and order-preserving within a changed block —
    /// intraline emphasis exists only between pairs, and the split layout
    /// aligns a pair on one row. Context and unpaired lines carry None.
    pub pair: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffLineKind {
    Context,
    Removed,
    Added,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSegment {
    pub text: String,
    pub changed: bool,
}

/// One block of a conflicted file's merge structure (`FileDiffContent::
/// Conflict`), in file order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ConflictSection {
    /// A run every side agrees on, rendered as context (all lines carry
    /// `Context` kind). Long runs are trimmed to diff context around the
    /// neighboring regions, with an `Elided` section standing in for the
    /// skipped middle.
    Resolved { lines: Vec<DiffLine> },
    /// Count of resolved lines skipped between the flanking sections.
    Elided { lines: u32 },
    /// One conflict region: every side shown as its diff from the merge
    /// base — jj's diff-marker semantics made structural. Two-sided
    /// regions order sides #1 and #2 exactly as the take verbs' "ours"
    /// and "theirs".
    Region { sides: Vec<ConflictSideDiff> },
}

/// One side of a conflict region, as its changes from the base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictSideDiff {
    /// The side's conflict label when the tree carries one (a commit
    /// description, "revert destination", …); None for plain "side #N".
    pub label: Option<String>,
    /// Diff from the base to this side; `Context` lines are what the side
    /// keeps from the base.
    pub lines: Vec<DiffLine>,
}

/// One file's part in a split: either the whole file (the fast path) or a
/// subset of its diff hunks. What the split panel sends per checked row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitSelection {
    /// Repo-relative path, `/`-separated.
    pub path: String,
    /// `None` — the whole file moves into the carved-off change. `Some` —
    /// only these hunks of the file's current diff do; the rest of the file
    /// stays with the remainder. Hunks are identified by their unified-diff
    /// coordinates so the backend can verify them against a freshly
    /// computed diff and refuse when the change moved underneath the panel.
    pub hunks: Option<Vec<SplitHunk>>,
}

impl SplitSelection {
    /// A whole-file selection — the fast path.
    pub fn whole(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            hunks: None,
        }
    }
}

/// Coordinates of one selected unified hunk, as the diff surface rendered
/// it: 1-based start lines plus the per-side line counts (context lines
/// included). A hunk that no longer matches these exactly is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitHunk {
    pub old_start: u32,
    pub new_start: u32,
    pub old_lines: u32,
    pub new_lines: u32,
    /// `None` — the whole hunk. `Some` — only these lines of it, as
    /// indices into the hunk's rendered lines. The indices ride the same
    /// freshness verification as the coordinates: each must name an
    /// added or removed line of the freshly derived hunk (context rows
    /// carry no change to select).
    pub lines: Option<Vec<u32>>,
}

/// How a hunk selection reads in outcome summaries and op descriptions:
/// "1 hunk", "3 hunks". Shared by the real and mock backends so their
/// breadcrumbs match.
pub(crate) fn hunks_phrase(n: usize) -> String {
    if n == 1 {
        "1 hunk".to_owned()
    } else {
        format!("{n} hunks")
    }
}

/// One history search: which changes added or removed a string (pickaxe,
/// `jj log -S` semantics), or touched a file whose path contains it.
/// Fetched by the search surface; never part of a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySearch {
    /// The query this search answered, echoed back for stale-response
    /// matching.
    pub query: String,
    /// Matching changes, newest first.
    pub hits: Vec<SearchHit>,
    /// Commits examined before the walk finished or hit a cap.
    pub scanned: u32,
    /// True when the walk stopped early (commit cap, hit cap, or time
    /// budget) — older history may hold more matches.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// Node id, addressed like `GraphNode::id`: the short change id, or the
    /// short commit id for a divergent change or a hidden commit.
    pub id: String,
    pub change_id: String,
    pub commit_id: String,
    /// Empty string when the change has no description.
    pub description: String,
    pub author: String,
    /// RFC 3339 timestamp.
    pub timestamp: String,
    /// Files this change matched in, repo path order.
    pub files: Vec<SearchFileHit>,
    /// Count of matching files dropped past the per-hit cap.
    pub more_files: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFileHit {
    /// Repo-relative path, `/`-separated.
    pub path: String,
    /// Occurrences of the query on this file's new side. A pickaxe hit is
    /// `added != removed` — the change altered how often the string
    /// appears, which is what "introduced" and "removed" mean.
    pub added: u32,
    /// Occurrences on the old side.
    pub removed: u32,
    /// The path itself contains the query (filename search).
    pub path_match: bool,
}

/// One change's evolution (`jj evolog`): the versions its commit was
/// rewritten through, newest first. Fetched by the evolution panel; never
/// part of a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeEvolog {
    /// The change id this evolog answers, echoed back for stale-response
    /// matching.
    pub id: String,
    /// Versions newest first: the change's current commit, then each
    /// predecessor it was rewritten from. A never-rewritten change has one
    /// entry.
    pub entries: Vec<EvologEntry>,
    /// True when the predecessor walk stopped at the entry cap — older
    /// versions exist beyond the list.
    pub truncated: bool,
}

/// One version of a change. Versions are addressed by commit id — the
/// change id names them all — and stay inspectable after being rewritten
/// away (a hidden commit id still resolves for diffs and restores).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvologEntry {
    /// Short commit id of this version.
    pub commit_id: String,
    /// The version's commit description; empty when it had none.
    pub description: String,
    pub author: String,
    /// RFC 3339 commit timestamp.
    pub timestamp: String,
    /// The operation that created this version, when the op log still
    /// carries it: short op id, its description, and its RFC 3339 time.
    /// All None when the operation is unknown (pruned op history).
    pub op_id: Option<String>,
    pub op_description: Option<String>,
    pub op_timestamp: Option<String>,
}

/// One file annotated line by line (`jj file annotate`): which change
/// introduced each line of the file as it stands in one change's tree.
/// Fetched by the annotate pane; never part of a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAnnotation {
    /// The change id this annotation answers, echoed back for
    /// stale-response matching.
    pub id: String,
    /// Repo-relative path, `/`-separated, echoed back likewise.
    pub path: String,
    /// Distinct originating changes, in order of first appearance in the
    /// file. Lines index into this list.
    pub origins: Vec<AnnotateOrigin>,
    /// One entry per line of the file at that change, top to bottom.
    pub lines: Vec<AnnotateLine>,
    /// True when the file has more lines than the answer carries — the
    /// pane shows how far it got.
    pub truncated: bool,
}

/// A change some annotated line came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotateOrigin {
    /// Node id addressed like `GraphNode::id`: the short change id, or the
    /// short commit id for a divergent change or a hidden commit. What a
    /// line click jumps the graph to — the commit may still sit outside the
    /// current view's nodes (deep immutable history), which the jump
    /// surface handles.
    pub id: String,
    pub change_id: String,
    pub commit_id: String,
    /// First line of the originating change's description; empty when it
    /// has none.
    pub summary: String,
    pub author: String,
    /// RFC 3339 commit timestamp.
    pub timestamp: String,
}

/// One line of an annotated file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotateLine {
    /// Index into `FileAnnotation::origins` of the change that introduced
    /// this line.
    pub origin: u32,
    /// The line's text, without its trailing newline.
    pub text: String,
}

/// Whether trunk holds a run of local changes on the strength of a pull
/// request the forge reports merged, as this repository sees it right
/// now. Answered by `RepoBackend::landed_check`; landing reads it before
/// it rebases, pushes, or abandons anything on the strength of a merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LandedCheck {
    /// The changes are part of trunk's history here: the commit the forge
    /// names as the merge is, the pull request was merged from exactly
    /// these commits, and applying them on top of that commit would alter
    /// nothing. Or the commits themselves are in trunk's history.
    Landed,
    /// The merge the forge names does not vouch for the changes (it is
    /// not part of trunk's history, it lacks something, or the pull
    /// request was merged from other commits), but trunk's tip holds all
    /// of them all the same: they arrived another way (a pull request
    /// merged into the branch below it, which then landed whole).
    Held,
    /// The landing commit is not in this repository. Nothing has fetched
    /// it yet.
    Unfetched,
    /// The landing commit is here, but trunk's history does not include
    /// it, and trunk's tip does not hold the changes: the pull request
    /// merged somewhere other than trunk.
    Elsewhere,
    /// The landing commit is part of trunk's history, yet applying the
    /// changes on top of the commit that was asked (the landing commit,
    /// or trunk's tip when the pull request was merged from other
    /// commits) would still alter `paths` (capped; `more` counts the
    /// rest). Trunk does not hold everything the changes hold.
    #[serde(rename_all = "camelCase")]
    Missing { paths: Vec<String>, more: u32 },
}

/// Whether landing a stack one pull request at a time would leave trunk
/// as one merge of the stack's tip would. Answered by
/// `RepoBackend::lands_as_tip`; landing reads it before the first merge
/// of a stack that has more than one pull request left.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TipCheck {
    /// Both ways end with the same tree.
    Same,
    /// They end with different content at `paths` (capped; `more` counts
    /// the rest).
    #[serde(rename_all = "camelCase")]
    Differs { paths: Vec<String>, more: u32 },
    /// The stack has a shape this does not judge: a merge inside it, more
    /// commits than it walks, or a base that is neither part of trunk's
    /// history nor content trunk already holds.
    Unknown,
}

/// What a mutation did, surfaced as the operation breadcrumb after the
/// snapshot refreshes. Every write action returns one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpEffect {
    pub kind: OpEffectKind,
    /// E.g. "main moved", "main@origin updated", "working copy moved".
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OpEffectKind {
    Bookmark,
    RemoteBookmark,
    WorkingCopy,
}
