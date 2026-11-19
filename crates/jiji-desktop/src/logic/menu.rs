//! What the context menu offers for whatever sits under a right-click.
//! Pure: targets in, sections of items out — the same affordance rules the
//! change header and the palette use, so the routes never disagree.

use jiji_core::snapshot::{
    BookmarkState, DiffHunk, DiffLineKind, FileDiff, FileDiffContent, FileStatus, GraphNode,
    NodeKind, RepoSnapshot, SplitHunk,
};
use jiji_forge::{ChecksRollup, PrState, PrSummary};

use super::conflicts::merge_tool_label;
use super::inspect::{action_availability, find_node, CompareMode};
use super::split::{can_select_hunks, can_select_lines, hunk_coords};
use crate::app::{Section, UiIntent};
use crate::host::integrations::AppInfo;
use crate::ui::IconName;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardOp {
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// What to do about a misspelled word, from its menu.
#[derive(Clone, Debug, PartialEq)]
pub enum SpellChoice {
    /// Put this in its place.
    Use(String),
    /// Stop flagging it in this field.
    Ignore,
    /// Add it to the user's dictionary.
    Learn,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MenuAction {
    Clipboard(ClipboardOp),
    Spell(SpellChoice),
    CopyText(String),
    NewChild(String),
    Edit(String),
    Duplicate(String),
    Revert(String),
    Absorb(String),
    Intent(UiIntent),
    Palette,
    Refresh,
    ChooseRepo,
    Undo,
    Section(Section),
    OpenUrl(String),
    OpenInEditor {
        editor: String,
        path: String,
        line: Option<u32>,
    },
    RevealInFinder(String),
    OpenInTerminal(String),
    ResolveConflict {
        change_id: String,
        path: String,
    },
    AnnotateFile {
        change_id: String,
        path: String,
    },
    RestoreFile {
        change_id: String,
        path: String,
    },
    RestoreHunk {
        change_id: String,
        path: String,
        hunk: SplitHunk,
    },
    RestoreOperation(String),
    RevertOperation(String),
    PushBookmark(String),
    TrackBookmark {
        name: String,
        remote: String,
    },
    UntrackBookmark {
        name: String,
        remote: String,
    },
    DeleteBookmark(String),
    RerunCi(u64),
    FetchPr {
        remote: String,
        number: u64,
        bookmark: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub id: &'static str,
    pub label: String,
    pub icon: Option<IconName>,
    pub shortcut: Option<&'static str>,
    pub danger: bool,
    pub disabled: bool,
    /// Set in bold: a spelling guess, apart from the commands.
    pub strong: bool,
    pub action: Option<MenuAction>,
    /// A submenu, when the item opens one instead of acting.
    pub children: Vec<MenuSection>,
}

pub type MenuSection = Vec<MenuItem>;

impl MenuItem {
    fn new(id: &'static str, label: impl Into<String>, icon: IconName, action: MenuAction) -> Self {
        Self {
            id,
            label: label.into(),
            icon: Some(icon),
            shortcut: None,
            danger: false,
            disabled: false,
            strong: false,
            action: Some(action),
            children: Vec::new(),
        }
    }

    fn plain(id: &'static str, label: impl Into<String>, action: MenuAction) -> Self {
        Self {
            icon: None,
            ..Self::new(id, label, IconName::Dot, action)
        }
    }

    fn shortcut(mut self, keys: &'static str) -> Self {
        self.shortcut = Some(keys);
        self
    }

    fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    fn submenu(
        id: &'static str,
        label: impl Into<String>,
        icon: IconName,
        children: MenuSection,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            icon: Some(icon),
            shortcut: None,
            danger: false,
            disabled: false,
            strong: false,
            action: None,
            children: vec![children],
        }
    }
}

fn copy_item(id: &'static str, label: &str, text: impl Into<String>) -> MenuItem {
    MenuItem::new(id, label, IconName::Copy, MenuAction::CopyText(text.into()))
}

/// What sits under the pointer.
pub enum MenuTarget<'a> {
    /// A text field.
    Editable {
        has_selection: bool,
        read_only: bool,
        /// What a misspelled word under the pointer might have been, best
        /// first: present only on a misspelling.
        guesses: Option<&'a [String]>,
    },
    Change {
        node: &'a GraphNode,
        /// Whether "against trunk" / "against stack base" can apply.
        compare_trunk: bool,
        compare_base: bool,
        /// The bookmark this change heads a landable stack under.
        land_bookmark: Option<String>,
        shippable: bool,
    },
    Diff {
        file: &'a FileDiff,
        change_id: &'a str,
        /// The diff is the change's own and the change can be rewritten.
        mutable: bool,
        /// The line under the pointer: its old and new numbers.
        line: Option<(Option<u32>, Option<u32>)>,
        resolve_tool: Option<&'a str>,
    },
    Bookmark {
        bookmark: &'a BookmarkState,
        pr: Option<&'a PrSummary>,
        has_remotes: bool,
    },
    Pr {
        pr: &'a PrSummary,
        remote: Option<&'a str>,
        review_bookmark_taken: bool,
    },
    Operation {
        op_id: &'a str,
        is_current: bool,
    },
    Conflict {
        change_id: Option<&'a str>,
        path: &'a str,
        is_working_copy: bool,
        resolve_tool: Option<&'a str>,
    },
    Breadcrumb,
    Surface,
}

pub struct MenuContext<'a> {
    pub snapshot: Option<&'a RepoSnapshot>,
    /// Text selected on the surface (not in a field).
    pub selection: &'a str,
    pub target: MenuTarget<'a>,
    /// Editors in the order menus offer them.
    pub editors: &'a [AppInfo],
    pub can_undo: bool,
}

/// The local bookmark a fetched PR head lands on.
pub fn review_bookmark(number: u64) -> String {
    format!("pr/{number}")
}

pub fn can_rerun(pr: &PrSummary) -> bool {
    pr.state == PrState::Open && pr.checks == ChecksRollup::Failing
}

// ── Patch text ──

/// The hunk a rendered line belongs to, matched on whichever side the row
/// carries.
pub fn hunk_at(hunks: &[DiffHunk], line: (Option<u32>, Option<u32>)) -> Option<&DiffHunk> {
    hunks.iter().find(|hunk| {
        let coords = hunk_coords(hunk);
        let in_new = line
            .1
            .is_some_and(|no| no >= hunk.new_start && no < hunk.new_start + coords.new_lines);
        let in_old = line
            .0
            .is_some_and(|no| no >= hunk.old_start && no < hunk.old_start + coords.old_lines);
        in_new || in_old
    })
}

/// The clicked line's index into its hunk's rendered lines — the
/// coordinate `SplitHunk::lines` speaks. `None` when the click landed on a
/// context line (no change to pick) or outside the hunk.
pub fn line_index_at(hunk: &DiffHunk, line: (Option<u32>, Option<u32>)) -> Option<u32> {
    let (mut old_no, mut new_no) = (hunk.old_start, hunk.new_start);
    for (index, l) in hunk.lines.iter().enumerate() {
        let hit = (line.0 == Some(old_no) && l.kind != DiffLineKind::Added)
            || (line.1 == Some(new_no) && l.kind != DiffLineKind::Removed);
        if hit {
            return (l.kind != DiffLineKind::Context).then_some(index as u32);
        }
        old_no += u32::from(l.kind != DiffLineKind::Added);
        new_no += u32::from(l.kind != DiffLineKind::Removed);
    }
    None
}

/// One file's diff — or a single hunk of it — serialized as a unified
/// patch, for pasting into reviews and chats. Empty for non-text content.
pub fn unified_patch(file: &FileDiff, hunk: Option<&DiffHunk>) -> String {
    let FileDiffContent::Text { hunks, .. } = &file.content else {
        return String::new();
    };
    let old_name = if file.status == FileStatus::Added {
        "/dev/null".to_owned()
    } else {
        format!("a/{}", file.renamed_from.as_deref().unwrap_or(&file.path))
    };
    let new_name = if file.status == FileStatus::Removed {
        "/dev/null".to_owned()
    } else {
        format!("b/{}", file.path)
    };
    let mut out = format!("--- {old_name}\n+++ {new_name}\n");
    let one;
    let hunks: &[DiffHunk] = match hunk {
        Some(hunk) => {
            one = [hunk.clone()];
            &one
        }
        None => hunks,
    };
    for hunk in hunks {
        let coords = hunk_coords(hunk);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, coords.old_lines, hunk.new_start, coords.new_lines
        ));
        for line in &hunk.lines {
            out.push(match line.kind {
                DiffLineKind::Context => ' ',
                DiffLineKind::Removed => '-',
                DiffLineKind::Added => '+',
            });
            for segment in &line.segments {
                out.push_str(&segment.text);
            }
            out.push('\n');
        }
    }
    out
}

// ── Menus ──

fn editor_items(
    editors: &[AppInfo],
    ids: (&'static str, &'static str),
    path: &str,
    line: Option<u32>,
) -> Vec<MenuItem> {
    let Some(first) = editors.first() else {
        return Vec::new();
    };
    let open = |editor: &AppInfo| MenuAction::OpenInEditor {
        editor: editor.id.to_owned(),
        path: path.to_owned(),
        line,
    };
    let mut items = vec![MenuItem::new(
        ids.0,
        format!("Open in {}", first.label),
        IconName::Code,
        open(first),
    )];
    if editors.len() > 1 {
        items.push(MenuItem::submenu(
            ids.1,
            "Open With",
            IconName::Code,
            editors
                .iter()
                .map(|editor| MenuItem::plain("open-with", editor.label, open(editor)))
                .collect(),
        ));
    }
    items
}

/// The most guesses a misspelling's menu offers.
const GUESSES: usize = 6;

fn editable_menu(
    has_selection: bool,
    read_only: bool,
    guesses: Option<&[String]>,
) -> Vec<MenuSection> {
    // A misspelling under the pointer leads with what it might have been,
    // then the ways to leave it be, as Mac text fields do.
    let mut spelling = Vec::new();
    if let Some(guesses) = guesses {
        spelling.push(if guesses.is_empty() {
            vec![MenuItem::plain(
                "spell.none",
                "No Guesses Found",
                MenuAction::Spell(SpellChoice::Ignore),
            )
            .disabled(true)]
        } else {
            guesses
                .iter()
                .take(GUESSES)
                .map(|guess| MenuItem {
                    strong: true,
                    ..MenuItem::plain(
                        "spell.guess",
                        guess.clone(),
                        MenuAction::Spell(SpellChoice::Use(guess.clone())),
                    )
                })
                .collect()
        });
        spelling.push(vec![
            MenuItem::plain(
                "spell.ignore",
                "Ignore Spelling",
                MenuAction::Spell(SpellChoice::Ignore),
            ),
            MenuItem::plain(
                "spell.learn",
                "Learn Spelling",
                MenuAction::Spell(SpellChoice::Learn),
            ),
        ]);
    }
    let copy = MenuItem::new(
        "edit.copy",
        "Copy",
        IconName::Copy,
        MenuAction::Clipboard(ClipboardOp::Copy),
    )
    .shortcut("⌘C")
    .disabled(!has_selection);
    let select_all = vec![MenuItem::new(
        "edit.selectAll",
        "Select All",
        IconName::TextSelect,
        MenuAction::Clipboard(ClipboardOp::SelectAll),
    )
    .shortcut("⌘A")];
    // Text to read offers what reading can do with it.
    if read_only {
        return vec![vec![copy], select_all];
    }
    spelling.extend([
        vec![
            MenuItem::new(
                "edit.cut",
                "Cut",
                IconName::Scissors,
                MenuAction::Clipboard(ClipboardOp::Cut),
            )
            .shortcut("⌘X")
            .disabled(!has_selection),
            copy,
            MenuItem::new(
                "edit.paste",
                "Paste",
                IconName::Clipboard,
                MenuAction::Clipboard(ClipboardOp::Paste),
            )
            .shortcut("⌘V"),
        ],
        select_all,
    ]);
    spelling
}

#[allow(clippy::too_many_arguments)]
fn change_menu(
    snapshot: &RepoSnapshot,
    node: &GraphNode,
    compare_trunk: bool,
    compare_base: bool,
    land_bookmark: Option<&str>,
    shippable: bool,
) -> Vec<MenuSection> {
    let avail = action_availability(snapshot, node);
    let id = || node.id.clone();
    let intent = |intent: UiIntent| MenuAction::Intent(intent);
    let mut change = Vec::new();
    if avail.describe {
        change.push(MenuItem::new(
            "change.describe",
            if node.description.trim().is_empty() {
                "Describe Change…"
            } else {
                "Edit Description…"
            },
            IconName::Edit,
            intent(UiIntent::Describe),
        ));
    }
    change.push(MenuItem::new(
        "change.new",
        "New Child Change",
        IconName::Plus,
        MenuAction::NewChild(id()),
    ));
    if avail.edit {
        change.push(MenuItem::new(
            "change.edit",
            "Edit Change",
            IconName::AtSign,
            MenuAction::Edit(id()),
        ));
    }
    change.push(MenuItem::new(
        "change.duplicate",
        "Duplicate Change",
        IconName::Copy,
        MenuAction::Duplicate(id()),
    ));
    if !node.is_empty {
        change.push(MenuItem::new(
            "change.revert",
            "Revert Change",
            IconName::Undo,
            MenuAction::Revert(id()),
        ));
    }
    change.push(MenuItem::new(
        "change.bookmark",
        "Bookmark…",
        IconName::Bookmark,
        intent(UiIntent::Bookmark),
    ));
    if avail.rebase {
        change.push(MenuItem::new(
            "change.rebase",
            "Rebase…",
            IconName::Rebase,
            intent(UiIntent::Rebase),
        ));
    }
    if avail.split {
        change.push(MenuItem::new(
            "change.split",
            "Split Change…",
            IconName::Split,
            intent(UiIntent::Split {
                into: false,
                path: None,
            }),
        ));
    }
    if avail.squash {
        change.push(MenuItem::new(
            "change.squash",
            "Squash into Parent",
            IconName::Squash,
            intent(UiIntent::Squash),
        ));
    }
    if node.kind != NodeKind::Immutable && !node.is_empty {
        change.push(MenuItem::new(
            "change.absorb",
            "Absorb into Ancestors",
            IconName::Squash,
            MenuAction::Absorb(id()),
        ));
    }
    if avail.abandon {
        change.push(
            MenuItem::new(
                "change.abandon",
                "Abandon Change",
                IconName::Trash,
                intent(UiIntent::Abandon),
            )
            .danger(),
        );
    }

    let compare = |mode: Option<CompareMode>| intent(UiIntent::Compare(mode));
    let mut compare_children = vec![MenuItem::plain(
        "change.compare.parent",
        "Against Parent",
        compare(Some(CompareMode::Parent)),
    )];
    if compare_trunk {
        let trunk = if snapshot.trunk_bookmark.is_empty() {
            "trunk"
        } else {
            &snapshot.trunk_bookmark
        };
        compare_children.push(MenuItem::plain(
            "change.compare.trunk",
            format!("Against {trunk}"),
            compare(Some(CompareMode::Trunk)),
        ));
    }
    if compare_base {
        compare_children.push(MenuItem::plain(
            "change.compare.base",
            "Against Stack Base",
            compare(Some(CompareMode::Base)),
        ));
    }
    compare_children.push(MenuItem::plain(
        "change.compare.pick",
        "Against Any Change…",
        compare(None),
    ));
    let view = vec![MenuItem::submenu(
        "change.compare",
        "Compare",
        IconName::Compare,
        compare_children,
    )];

    let mut publish = Vec::new();
    if let Some(bookmark) = land_bookmark {
        publish.push(MenuItem::new(
            "change.land",
            format!("Land {bookmark}…"),
            IconName::Publish,
            intent(UiIntent::Land {
                bookmark: bookmark.to_owned(),
            }),
        ));
    }
    if shippable {
        publish.push(MenuItem::new(
            "change.ship",
            "Ship to Trunk…",
            IconName::Publish,
            intent(UiIntent::Ship { head: id() }),
        ));
    }

    let mut copies = vec![
        copy_item(
            "change.copyChangeId",
            "Copy Change ID",
            node.change_id.clone(),
        ),
        copy_item(
            "change.copyCommitId",
            "Copy Commit ID",
            node.commit_id.clone(),
        ),
    ];
    if !node.description.trim().is_empty() {
        copies.push(copy_item(
            "change.copyDescription",
            "Copy Description",
            node.description.clone(),
        ));
    }
    [change, view, publish, copies]
        .into_iter()
        .filter(|section| !section.is_empty())
        .collect()
}

fn diff_menu(
    snapshot: &RepoSnapshot,
    file: &FileDiff,
    change_id: &str,
    mutable: bool,
    line: Option<(Option<u32>, Option<u32>)>,
    resolve_tool: Option<&str>,
    editors: &[AppInfo],
) -> Vec<MenuSection> {
    let abs_path = format!("{}/{}", snapshot.repo_path, file.path);
    let mut sections = Vec::new();
    if file.status != FileStatus::Removed {
        let jump = line.and_then(|(old, new)| new.or(old));
        let mut open = editor_items(
            editors,
            ("diff.openEditor", "diff.openWith"),
            &abs_path,
            jump,
        );
        if !open.is_empty() {
            open.push(MenuItem::new(
                "diff.reveal",
                "Reveal in Finder",
                IconName::Folder,
                MenuAction::RevealInFinder(abs_path.clone()),
            ));
            sections.push(open);
        }
    }
    if let Some(tool) = resolve_tool {
        sections.push(vec![MenuItem::new(
            "diff.resolve",
            format!("Resolve in {}…", merge_tool_label(tool)),
            IconName::Conflicts,
            MenuAction::ResolveConflict {
                change_id: change_id.to_owned(),
                path: file.path.clone(),
            },
        )]);
    }
    let hunks: &[DiffHunk] = match &file.content {
        FileDiffContent::Text { hunks, .. } => hunks,
        _ => &[],
    };
    if file.status != FileStatus::Removed
        && matches!(
            file.content,
            FileDiffContent::Text { .. } | FileDiffContent::Omitted
        )
    {
        sections.push(vec![MenuItem::new(
            "diff.annotate",
            "Annotate File",
            IconName::Eye,
            MenuAction::AnnotateFile {
                change_id: change_id.to_owned(),
                path: file.path.clone(),
            },
        )]);
    }
    let mut copies = vec![
        copy_item("diff.copyPath", "Copy Path", file.path.clone()),
        copy_item("diff.copyAbsPath", "Copy Absolute Path", abs_path),
    ];
    let clicked_hunk = line.and_then(|line| hunk_at(hunks, line));
    if !hunks.is_empty() {
        if let Some(hunk) = clicked_hunk {
            copies.push(copy_item(
                "diff.copyHunk",
                "Copy Hunk as Patch",
                unified_patch(file, Some(hunk)),
            ));
        }
        copies.push(copy_item(
            "diff.copyFilePatch",
            "Copy File as Patch",
            unified_patch(file, None),
        ));
    }
    sections.push(copies);

    let node = find_node(snapshot, change_id);
    if let (true, Some(node)) = (mutable, node) {
        let mut mutate = Vec::new();
        if action_availability(snapshot, node).split {
            mutate.push(MenuItem::new(
                "diff.moveFile",
                "Move File to Another Change…",
                IconName::Split,
                MenuAction::Intent(UiIntent::Split {
                    into: true,
                    path: Some(file.path.clone()),
                }),
            ));
        }
        if let (Some(hunk), Some(line), true) = (clicked_hunk, line, can_select_hunks(file)) {
            let restore = |hunk: SplitHunk| MenuAction::RestoreHunk {
                change_id: change_id.to_owned(),
                path: file.path.clone(),
                hunk,
            };
            if let Some(index) = line_index_at(hunk, line).filter(|_| can_select_lines(hunk)) {
                mutate.push(
                    MenuItem::new(
                        "diff.restoreLine",
                        "Discard This Line",
                        IconName::Undo,
                        restore(SplitHunk {
                            lines: Some(vec![index]),
                            ..hunk_coords(hunk)
                        }),
                    )
                    .danger(),
                );
            }
            mutate.push(
                MenuItem::new(
                    "diff.restoreHunk",
                    "Discard This Hunk",
                    IconName::Undo,
                    restore(hunk_coords(hunk)),
                )
                .danger(),
            );
        }
        mutate.push(
            MenuItem::new(
                "diff.restoreFile",
                "Discard Changes to File",
                IconName::Undo,
                MenuAction::RestoreFile {
                    change_id: change_id.to_owned(),
                    path: file.path.clone(),
                },
            )
            .danger(),
        );
        sections.push(mutate);
    }
    sections
}

fn bookmark_menu(
    bookmark: &BookmarkState,
    pr: Option<&PrSummary>,
    has_remotes: bool,
) -> Vec<MenuSection> {
    let mut sections = Vec::new();
    if let Some(pr) = pr {
        sections.push(vec![
            MenuItem::new(
                "bookmark.openPr",
                format!("Open PR #{} in Browser", pr.number),
                IconName::Branch,
                MenuAction::OpenUrl(pr.url.clone()),
            ),
            copy_item("bookmark.copyPrUrl", "Copy PR URL", pr.url.clone()),
        ]);
    }
    let mut ops = Vec::new();
    if bookmark.is_local && has_remotes {
        ops.push(MenuItem::new(
            "bookmark.push",
            "Push Bookmark",
            IconName::Publish,
            MenuAction::PushBookmark(bookmark.name.clone()),
        ));
    }
    if let (false, Some(remote)) = (bookmark.is_local, &bookmark.remote) {
        let (name, remote) = (bookmark.name.clone(), remote.clone());
        ops.push(if bookmark.tracked {
            MenuItem::new(
                "bookmark.untrack",
                format!("Untrack {name}@{remote}"),
                IconName::Close,
                MenuAction::UntrackBookmark { name, remote },
            )
        } else {
            MenuItem::new(
                "bookmark.track",
                format!("Track {name}@{remote}"),
                IconName::Eye,
                MenuAction::TrackBookmark { name, remote },
            )
        });
    }
    ops.push(MenuItem::new(
        "bookmark.manage",
        "Manage in Bookmarks View",
        IconName::Bookmark,
        MenuAction::Section(Section::Bookmarks),
    ));
    sections.push(ops);
    sections.push(vec![copy_item(
        "bookmark.copyName",
        "Copy Bookmark Name",
        bookmark.name.clone(),
    )]);
    if bookmark.is_local && !bookmark.is_trunk {
        sections.push(vec![MenuItem::new(
            "bookmark.delete",
            "Delete Bookmark",
            IconName::Trash,
            MenuAction::DeleteBookmark(bookmark.name.clone()),
        )
        .danger()]);
    }
    sections
}

fn pr_menu(pr: &PrSummary, remote: Option<&str>, review_bookmark_taken: bool) -> Vec<MenuSection> {
    let mut sections = vec![vec![
        MenuItem::new(
            "pr.open",
            format!("Open PR #{} in Browser", pr.number),
            IconName::Branch,
            MenuAction::OpenUrl(pr.url.clone()),
        ),
        copy_item("pr.copyUrl", "Copy PR URL", pr.url.clone()),
        copy_item("pr.copyNumber", "Copy PR Number", pr.number.to_string()),
    ]];
    let mut acts = Vec::new();
    if can_rerun(pr) {
        acts.push(MenuItem::new(
            "pr.rerunCi",
            "Re-run Failed CI",
            IconName::Refresh,
            MenuAction::RerunCi(pr.number),
        ));
    }
    if let (Some(remote), PrState::Open, false) = (remote, pr.state, review_bookmark_taken) {
        let bookmark = review_bookmark(pr.number);
        acts.push(MenuItem::new(
            "pr.fetch",
            format!("Fetch Head as {bookmark}"),
            IconName::ArrowDown,
            MenuAction::FetchPr {
                remote: remote.to_owned(),
                number: pr.number,
                bookmark,
            },
        ));
    }
    if !acts.is_empty() {
        sections.push(acts);
    }
    sections
}

fn operation_menu(op_id: &str, is_current: bool) -> Vec<MenuSection> {
    let mut acts = Vec::new();
    if !is_current {
        acts.push(MenuItem::new(
            "op.restore",
            "Restore Repo to This Operation",
            IconName::Restore,
            MenuAction::RestoreOperation(op_id.to_owned()),
        ));
    }
    acts.push(MenuItem::new(
        "op.revert",
        if is_current {
            "Undo This Operation"
        } else {
            "Back Out This Operation"
        },
        IconName::Undo,
        MenuAction::RevertOperation(op_id.to_owned()),
    ));
    vec![
        acts,
        vec![copy_item("op.copyId", "Copy Operation ID", op_id)],
    ]
}

fn conflict_menu(
    repo_path: &str,
    change_id: Option<&str>,
    path: &str,
    is_working_copy: bool,
    resolve_tool: Option<&str>,
    editors: &[AppInfo],
) -> Vec<MenuSection> {
    let mut sections = Vec::new();
    if let (Some(tool), Some(change_id)) = (resolve_tool, change_id) {
        sections.push(vec![MenuItem::new(
            "conflict.resolve",
            format!("Resolve in {}…", merge_tool_label(tool)),
            IconName::Conflicts,
            MenuAction::ResolveConflict {
                change_id: change_id.to_owned(),
                path: path.to_owned(),
            },
        )]);
    }
    // Only the working copy's conflicts are files on disk to open.
    let mut open = if is_working_copy {
        editor_items(
            editors,
            ("conflict.openEditor", "conflict.openWith"),
            &format!("{repo_path}/{path}"),
            None,
        )
    } else {
        Vec::new()
    };
    open.push(copy_item("conflict.copyPath", "Copy Path", path));
    sections.push(open);
    sections
}

fn surface_menu(ctx: &MenuContext, with_repo_section: bool) -> Vec<MenuSection> {
    let mut surface = vec![MenuItem::new(
        "app.palette",
        "Command Palette…",
        IconName::Command,
        MenuAction::Palette,
    )
    .shortcut("⌘K")];
    surface.push(match ctx.snapshot {
        Some(_) => MenuItem::new(
            "app.refresh",
            "Refresh",
            IconName::Refresh,
            MenuAction::Refresh,
        )
        .shortcut("⌘R"),
        None => MenuItem::new(
            "app.openRepo",
            "Open Repository…",
            IconName::Folder,
            MenuAction::ChooseRepo,
        )
        .shortcut("⌘O"),
    });
    if ctx.can_undo {
        surface.push(MenuItem::new(
            "app.undo",
            "Undo Last Operation",
            IconName::Undo,
            MenuAction::Undo,
        ));
    }
    let mut sections = vec![surface];
    if let (Some(snapshot), true) = (ctx.snapshot, with_repo_section) {
        let path = snapshot.repo_path.clone();
        let mut repo = vec![
            copy_item("app.copyRepoPath", "Copy Repository Path", path.clone()),
            MenuItem::new(
                "app.reveal",
                "Reveal in Finder",
                IconName::Folder,
                MenuAction::RevealInFinder(path.clone()),
            ),
            MenuItem::new(
                "app.terminal",
                "Open in Terminal",
                IconName::Terminal,
                MenuAction::OpenInTerminal(path.clone()),
            ),
        ];
        if let Some(editor) = ctx.editors.first() {
            repo.push(MenuItem::new(
                "app.openRepoEditor",
                format!("Open Repo in {}", editor.label),
                IconName::Code,
                MenuAction::OpenInEditor {
                    editor: editor.id.to_owned(),
                    path,
                    line: None,
                },
            ));
        }
        sections.push(repo);
    }
    sections
}

fn breadcrumb_menu(ctx: &MenuContext) -> Vec<MenuSection> {
    let mut items = Vec::new();
    if ctx.can_undo {
        items.push(MenuItem::new(
            "breadcrumb.undo",
            "Undo Last Operation",
            IconName::Undo,
            MenuAction::Undo,
        ));
    }
    items.push(MenuItem::new(
        "breadcrumb.ops",
        "Show in Operations Timeline",
        IconName::Operations,
        MenuAction::Section(Section::Operations),
    ));
    vec![items]
}

/// The menu for a right-click: a Copy for any selected text first, then
/// what the target itself offers, then (on bare surfaces) the app-level
/// items.
pub fn build_menu(ctx: &MenuContext) -> Vec<MenuSection> {
    if let MenuTarget::Editable {
        has_selection,
        read_only,
        guesses,
    } = ctx.target
    {
        return editable_menu(has_selection, read_only, guesses);
    }
    let mut sections = Vec::new();
    if !ctx.selection.trim().is_empty() {
        sections.push(vec![MenuItem::new(
            "copy.selection",
            "Copy",
            IconName::Copy,
            MenuAction::CopyText(ctx.selection.to_owned()),
        )
        .shortcut("⌘C")]);
    }
    match (&ctx.target, ctx.snapshot) {
        (
            MenuTarget::Change {
                node,
                compare_trunk,
                compare_base,
                land_bookmark,
                shippable,
            },
            Some(snapshot),
        ) => sections.extend(change_menu(
            snapshot,
            node,
            *compare_trunk,
            *compare_base,
            land_bookmark.as_deref(),
            *shippable,
        )),
        (
            MenuTarget::Diff {
                file,
                change_id,
                mutable,
                line,
                resolve_tool,
            },
            Some(snapshot),
        ) => sections.extend(diff_menu(
            snapshot,
            file,
            change_id,
            *mutable,
            *line,
            *resolve_tool,
            ctx.editors,
        )),
        (
            MenuTarget::Bookmark {
                bookmark,
                pr,
                has_remotes,
            },
            _,
        ) => sections.extend(bookmark_menu(bookmark, *pr, *has_remotes)),
        (
            MenuTarget::Pr {
                pr,
                remote,
                review_bookmark_taken,
            },
            _,
        ) => sections.extend(pr_menu(pr, *remote, *review_bookmark_taken)),
        (MenuTarget::Operation { op_id, is_current }, _) => {
            sections.extend(operation_menu(op_id, *is_current))
        }
        (
            MenuTarget::Conflict {
                change_id,
                path,
                is_working_copy,
                resolve_tool,
            },
            Some(snapshot),
        ) => sections.extend(conflict_menu(
            &snapshot.repo_path,
            *change_id,
            path,
            *is_working_copy,
            *resolve_tool,
            ctx.editors,
        )),
        (MenuTarget::Breadcrumb, _) => sections.extend(breadcrumb_menu(ctx)),
        (MenuTarget::Surface, _) => sections.extend(surface_menu(ctx, true)),
        _ => {}
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::snapshot::{DiffLine, DiffSegment};
    use jiji_core::{MockBackend, RepoBackend};

    fn snapshot() -> RepoSnapshot {
        let dir = std::env::temp_dir().join(format!("jiji-menu-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".jj"));
        MockBackend::default().open(&dir).unwrap()
    }

    fn ids(sections: &[MenuSection]) -> Vec<&'static str> {
        sections.iter().flatten().map(|item| item.id).collect()
    }

    fn line(kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            segments: vec![DiffSegment {
                text: text.into(),
                changed: false,
            }],
            pair: None,
        }
    }

    fn text_file() -> FileDiff {
        use DiffLineKind::*;
        let hunk = |start: u32| DiffHunk {
            old_start: start,
            new_start: start,
            context: None,
            lines: vec![
                line(Context, "keep"),
                line(Removed, "old"),
                line(Added, "new"),
                line(Added, "extra"),
            ],
        };
        FileDiff {
            path: "src/a.rs".into(),
            status: FileStatus::Modified,
            renamed_from: None,
            has_conflict: false,
            content: FileDiffContent::Text {
                hunks: vec![hunk(10), hunk(40)],
                truncated: false,
            },
        }
    }

    #[test]
    fn immutable_changes_offer_no_rewrites() {
        let snapshot = snapshot();
        let base = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Immutable)
            .unwrap();
        let menu = build_menu(&MenuContext {
            snapshot: Some(&snapshot),
            selection: "",
            target: MenuTarget::Change {
                node: base,
                compare_trunk: false,
                compare_base: false,
                land_bookmark: None,
                shippable: false,
            },
            editors: &[],
            can_undo: false,
        });
        let ids = ids(&menu);
        assert!(ids.contains(&"change.new") && ids.contains(&"change.bookmark"));
        for rewrite in [
            "change.describe",
            "change.rebase",
            "change.abandon",
            "change.absorb",
        ] {
            assert!(
                !ids.contains(&rewrite),
                "{rewrite} offered on an immutable change"
            );
        }
        assert!(ids.contains(&"change.copyChangeId"));
    }

    #[test]
    fn a_mutable_change_gets_its_verbs_and_a_selection_copies_first() {
        let snapshot = snapshot();
        let node = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Mutable && !n.is_empty)
            .unwrap();
        let menu = build_menu(&MenuContext {
            snapshot: Some(&snapshot),
            selection: "picked text",
            target: MenuTarget::Change {
                node,
                compare_trunk: true,
                compare_base: false,
                land_bookmark: Some("feat".into()),
                shippable: true,
            },
            editors: &[],
            can_undo: false,
        });
        assert_eq!(menu[0][0].id, "copy.selection");
        let ids = ids(&menu);
        for id in [
            "change.describe",
            "change.rebase",
            "change.abandon",
            "change.land",
            "change.ship",
        ] {
            assert!(ids.contains(&id), "{id} missing");
        }
        let compare = menu
            .iter()
            .flatten()
            .find(|item| item.id == "change.compare")
            .unwrap();
        let children: Vec<&str> = compare.children[0].iter().map(|i| i.id).collect();
        assert_eq!(
            children,
            [
                "change.compare.parent",
                "change.compare.trunk",
                "change.compare.pick"
            ]
        );
    }

    #[test]
    fn a_changed_line_can_be_discarded_alone() {
        let snapshot = snapshot();
        let node = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Mutable && !n.is_empty)
            .unwrap();
        let file = text_file();
        let menu = |line| {
            build_menu(&MenuContext {
                snapshot: Some(&snapshot),
                selection: "",
                target: MenuTarget::Diff {
                    file: &file,
                    change_id: &node.id,
                    mutable: true,
                    line,
                    resolve_tool: None,
                },
                editors: &[],
                can_undo: false,
            })
        };
        // The added line 11 of the first hunk.
        let on_added = menu(Some((None, Some(11))));
        let restore = on_added
            .iter()
            .flatten()
            .find(|item| item.id == "diff.restoreLine")
            .unwrap();
        assert_eq!(
            restore.action,
            Some(MenuAction::RestoreHunk {
                change_id: node.id.clone(),
                path: "src/a.rs".into(),
                hunk: SplitHunk {
                    old_start: 10,
                    new_start: 10,
                    old_lines: 2,
                    new_lines: 3,
                    lines: Some(vec![2]),
                },
            })
        );
        // A context line picks no single line, but still its hunk.
        let on_context = ids(&menu(Some((Some(10), Some(10)))));
        assert!(!on_context.contains(&"diff.restoreLine"));
        assert!(on_context.contains(&"diff.restoreHunk") && on_context.contains(&"diff.copyHunk"));
        // A header click has no line: file-level items only.
        let on_header = ids(&menu(None));
        assert!(
            !on_header.contains(&"diff.restoreHunk") && on_header.contains(&"diff.restoreFile")
        );
    }

    #[test]
    fn patches_serialize_with_git_headers() {
        let file = text_file();
        let FileDiffContent::Text { hunks, .. } = &file.content else {
            unreachable!()
        };
        let one = unified_patch(&file, Some(&hunks[1]));
        assert_eq!(
            one,
            "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -40,2 +40,3 @@\n keep\n-old\n+new\n+extra\n"
        );
        assert_eq!(unified_patch(&file, None).matches("@@ -").count(), 2);
        let mut added = text_file();
        added.status = FileStatus::Added;
        assert!(unified_patch(&added, None).starts_with("--- /dev/null\n+++ b/src/a.rs\n"));
        assert_eq!(line_index_at(&hunks[0], (Some(11), None)), Some(1));
        assert_eq!(hunk_at(hunks, (None, Some(41))).unwrap().old_start, 40);
        assert!(hunk_at(hunks, (None, Some(99))).is_none());
    }

    #[test]
    fn text_fields_get_the_clipboard_menu() {
        let menu = build_menu(&MenuContext {
            snapshot: None,
            selection: "ignored",
            target: MenuTarget::Editable {
                has_selection: false,
                read_only: false,
                guesses: None,
            },
            editors: &[],
            can_undo: false,
        });
        assert_eq!(
            ids(&menu),
            ["edit.cut", "edit.copy", "edit.paste", "edit.selectAll"]
        );
        assert!(menu[0][0].disabled && menu[0][1].disabled && !menu[0][2].disabled);
    }

    #[test]
    fn the_bare_surface_offers_app_and_repo_items() {
        let snapshot = snapshot();
        let editors = [AppInfo {
            id: "zed",
            label: "Zed",
        }];
        let menu = build_menu(&MenuContext {
            snapshot: Some(&snapshot),
            selection: "",
            target: MenuTarget::Surface,
            editors: &editors,
            can_undo: true,
        });
        assert_eq!(
            ids(&menu),
            [
                "app.palette",
                "app.refresh",
                "app.undo",
                "app.copyRepoPath",
                "app.reveal",
                "app.terminal",
                "app.openRepoEditor"
            ]
        );
    }
}
