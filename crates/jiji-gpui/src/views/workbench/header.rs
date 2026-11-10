//! The change header: what the change is and what you can do with it,
//! then where it sits and how its diff is shown — so the diff keeps the
//! pane. Panels (describe, rebase, split, …) open below as trays that push
//! the diff down rather than float over it.

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, App, Context, Corner, Div, Entity, FocusHandle,
    FontWeight, Hsla, ScrollHandle, Stateful, Task, UniformListScrollHandle, Window,
};
use jiji_core::snapshot::{
    BookmarkState, FileDiff, GraphNode, MutationOutcome, NodeKind, RepoSnapshot,
};

use super::{TrayConfirm, TrayDown, TrayEnter, TrayUp, WorkbenchView};
use crate::app::UiIntent;
use crate::host::HostResult;
use crate::logic::inspect::{
    action_availability, bookmarks_at, children_of, combined_description, descendants_of,
    divergent_siblings, find_node, move_direction, rebase_destinations, resolve_compare_from,
    split_path, squash_destinations, stack_position, summary_of, sync_label, ChangeActions,
    CompareMode, MoveDirection, Tone,
};
use crate::logic::pr::{pr_badge, pr_for_bookmark, PrTone};
use crate::logic::split::{
    self, can_select_hunks, can_select_lines, changed_line_indices, file_hunks, hunk_label,
    hunk_preview, hunk_stats, line_text, movable, selection_ready, split_payload, split_summary,
    splittable, HunkPick, Selection, SplitPick,
};
use crate::shell::Dismiss;
use crate::theme::{theme, Theme, RADIUS_M, RADIUS_S, TEXT_L, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::{
    btn, floating, from_now, h_flex, icon, icon_btn, input::field, popover, ref_label, rich,
    scrollbar, segmented, tooltip, truncated, v_flex, IconName, InputEvent, PopoverState,
    SwallowClicks, TextInput,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Confirm {
    Squash,
    Abandon,
}

/// The one tray open under the header, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Tray {
    None,
    Edit,
    Compare,
    Confirm(Confirm),
    Rebase,
    Split,
    Bookmark,
    /// Rename or delete the named bookmark.
    Manage(String),
}

pub(super) struct HeaderState {
    /// The change the state belongs to; everything resets when it moves.
    node: Option<String>,
    pub(super) tray: Tray,
    body_open: bool,
    /// A mutation from this header is in flight.
    acting: bool,
    error: Option<String>,
    editor: Entity<TextInput>,
    draft_restored: bool,
    /// The (repo, change, description) the open editor started from.
    editing_from: Option<(String, String, String)>,
    bookmark_name: Entity<TextInput>,
    rename: Entity<TextInput>,
    rebase_alone: bool,
    /// The picked destination of a rebase or a move-into.
    pub(super) dest: Option<String>,
    pub(super) dest_hover: Option<String>,
    /// Filters whichever destination list is open.
    filter: Entity<TextInput>,
    split: Selection,
    split_hunks_open: HashSet<String>,
    split_lines_open: HashSet<String>,
    split_desc: Entity<TextInput>,
    pub(super) split_into: bool,
    diverge_confirm: Option<String>,
    diverge_error: Option<String>,
    more: PopoverState,
    files: PopoverState,
    /// A CI re-run asked from this header: PR number, in flight, outcome.
    rerun: Option<(u64, bool, Option<String>)>,
    tray_focus: FocusHandle,
    dest_scroll: UniformListScrollHandle,
    split_scroll: ScrollHandle,
    files_scroll: ScrollHandle,
}

impl HeaderState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<WorkbenchView>) -> Self {
        Self {
            node: None,
            tray: Tray::None,
            body_open: false,
            acting: false,
            error: None,
            editor: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multiline(4, 12)
                    .placeholder("Summary line")
            }),
            draft_restored: false,
            editing_from: None,
            bookmark_name: cx.new(|cx| TextInput::new(window, cx).placeholder("new-bookmark-name")),
            rename: cx.new(|cx| TextInput::new(window, cx)),
            rebase_alone: false,
            dest: None,
            dest_hover: None,
            filter: cx.new(|cx| {
                TextInput::new(window, cx).placeholder("filter by id, title, or bookmark")
            }),
            split: Selection::new(),
            split_hunks_open: HashSet::new(),
            split_lines_open: HashSet::new(),
            split_desc: cx.new(|cx| {
                TextInput::new(window, cx)
                    .multiline(2, 6)
                    .placeholder("Summary line for the carved-out change")
            }),
            split_into: false,
            diverge_confirm: None,
            diverge_error: None,
            more: PopoverState::default(),
            files: PopoverState::default(),
            rerun: None,
            tray_focus: cx.focus_handle(),
            dest_scroll: UniformListScrollHandle::new(),
            split_scroll: ScrollHandle::new(),
            files_scroll: ScrollHandle::new(),
        }
    }

    pub(super) fn inputs(&self) -> [&Entity<TextInput>; 5] {
        [
            &self.editor,
            &self.bookmark_name,
            &self.rename,
            &self.filter,
            &self.split_desc,
        ]
    }
}

fn kind_glyph(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::WorkingCopy => "@",
        NodeKind::Mutable => "○",
        NodeKind::Immutable => "◆",
    }
}

fn short(id: &str, n: usize) -> String {
    id.chars().take(n).collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

fn title_or_none(description: &str) -> &str {
    let title = summary_of(description);
    if title.is_empty() {
        "no description"
    } else {
        title
    }
}

fn matches_filter(node: &GraphNode, query: &str) -> bool {
    query.is_empty()
        || node.id.to_lowercase().starts_with(query)
        || node.description.to_lowercase().contains(query)
        || node
            .bookmarks
            .iter()
            .any(|name| name.to_lowercase().contains(query))
}

fn tone_color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Ok => theme.ok,
        Tone::Warn => theme.warn,
        Tone::Danger => theme.danger,
        Tone::Muted => theme.text3,
    }
}

impl WorkbenchView {
    // ── State ──

    /// Follow the selection: another change starts with a clean header,
    /// and an editor left open keeps its text as a draft.
    pub(super) fn sync_header(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (range, node) = self.subject(cx);
        let node = node.filter(|_| range.is_none());
        if self.header.node == node {
            return;
        }
        self.keep_open_draft(cx);
        let had_tray = self.header.tray != Tray::None;
        let header = &mut self.header;
        header.node = node;
        header.tray = Tray::None;
        header.body_open = false;
        header.acting = false;
        header.error = None;
        header.rebase_alone = false;
        header.dest = None;
        header.dest_hover = None;
        header.split.clear();
        header.split_hunks_open.clear();
        header.split_lines_open.clear();
        header.split_into = false;
        header.diverge_confirm = None;
        header.diverge_error = None;
        header.more.close();
        header.files.close();
        if had_tray {
            window.focus(&self.focus);
        }
    }

    fn header_node(&self) -> Option<&GraphNode> {
        find_node(&self.snapshot, self.header.node.as_deref()?)
    }

    /// The files of the diff on screen, when it is this change's own.
    fn header_files(&self) -> Option<&[FileDiff]> {
        self.diff.current().map(|diff| diff.files.as_slice())
    }

    fn keep_open_draft(&mut self, cx: &mut Context<Self>) {
        if self.header.tray != Tray::Edit {
            return;
        }
        if let Some((repo, id, base)) = self.header.editing_from.take() {
            let text = self.header.editor.read(cx).text().to_owned();
            self.app
                .update(cx, |model, _| model.keep_draft(&repo, &id, &base, &text));
        }
    }

    /// Whether Esc has a tray (or menu) to close here.
    pub(super) fn dismiss_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.header.more.is_open() || self.header.files.is_open() {
            self.header.more.close();
            self.header.files.close();
            cx.notify();
            return true;
        }
        if self.header.diverge_confirm.take().is_some() {
            cx.notify();
            return true;
        }
        if self.header.tray == Tray::None {
            return false;
        }
        self.close_tray(window, cx);
        true
    }

    pub(super) fn close_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_open_draft(cx);
        self.header.tray = Tray::None;
        self.header.error = None;
        self.header.dest_hover = None;
        window.focus(&self.focus);
        cx.notify();
    }

    /// Open a tray (or close it when it is the one already open). Trays
    /// are exclusive: opening one closes whatever else was.
    fn toggle_tray(&mut self, tray: Tray, window: &mut Window, cx: &mut Context<Self>) {
        if self.header.tray == tray {
            self.close_tray(window, cx);
            return;
        }
        self.keep_open_draft(cx);
        let header = &mut self.header;
        header.tray = tray.clone();
        header.error = None;
        header.more.close();
        header.files.close();
        header.dest = None;
        header.dest_hover = None;
        header.rebase_alone = false;
        header.split.clear();
        header.split_hunks_open.clear();
        header.split_lines_open.clear();
        header.filter.update(cx, |input, cx| input.set_text("", cx));
        self.header.dest_scroll = UniformListScrollHandle::new();
        match &tray {
            Tray::Bookmark => self.header.bookmark_name.update(cx, |input, cx| {
                input.set_text("", cx);
                input.focus(window);
            }),
            Tray::Manage(name) => {
                let name = name.clone();
                self.header.rename.update(cx, |input, cx| {
                    input.set_text(name, cx);
                    input.focus(window);
                });
            }
            Tray::Rebase | Tray::Compare => self.focus_filter_or_tray(window, cx),
            Tray::Split => {
                self.header
                    .split_desc
                    .update(cx, |input, cx| input.set_text("", cx));
                // The split panel reads the change's own diff.
                if self.compare != CompareMode::Parent {
                    self.set_compare(CompareMode::Parent, cx);
                }
                window.focus(&self.header.tray_focus);
            }
            _ => window.focus(&self.header.tray_focus),
        }
        cx.notify();
    }

    /// The filter shows only for long lists; without it the tray itself
    /// takes the keys.
    fn focus_filter_or_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dest_candidates().len() > 6 {
            self.header
                .filter
                .update(cx, |input, _| input.focus(window));
        } else {
            window.focus(&self.header.tray_focus);
        }
    }

    pub(super) fn open_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.header_node() else {
            return;
        };
        let (id, description) = (node.id.clone(), node.description.clone());
        let repo = self.snapshot.repo_path.clone();
        let kept = self
            .app
            .read(cx)
            .draft_for(&repo, &id, &description)
            .map(str::to_owned);
        self.keep_open_draft(cx);
        self.header.draft_restored = kept.is_some();
        let text = kept.unwrap_or_else(|| description.clone());
        self.header.editing_from = Some((repo, id, description));
        self.header.tray = Tray::Edit;
        self.header.error = None;
        self.header.more.close();
        self.header.editor.update(cx, |input, cx| {
            input.set_text(text, cx);
            input.set_disabled(false, cx);
            input.focus(window);
        });
        cx.notify();
    }

    fn discard_editor_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((repo, id, base)) = self.header.editing_from.clone() else {
            return;
        };
        self.app
            .update(cx, |model, _| model.discard_draft(&repo, &id));
        self.header.draft_restored = false;
        self.header.editor.update(cx, |input, cx| {
            input.set_text(base, cx);
            input.focus(window);
        });
        cx.notify();
    }

    /// The shape every header mutation shares: one at a time, its error
    /// inline in the tray that asked (which stays open), the tray closing
    /// on success.
    fn run_panel(
        &mut self,
        task: Task<HostResult<MutationOutcome>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.header.acting = true;
        self.header.error = None;
        let for_node = self.header.node.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.header.acting = false;
                match result {
                    Ok(_) => {
                        // The mutation may already have moved the selection
                        // (and reset this header) — only close what is ours.
                        if this.header.node == for_node && this.header.tray != Tray::None {
                            this.header.editing_from = None;
                            this.header.tray = Tray::None;
                            window.focus(&this.focus);
                        }
                    }
                    Err(error) => this.header.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.header.acting {
            return;
        }
        let Some((repo, id, _)) = self.header.editing_from.clone() else {
            return;
        };
        let text = self.header.editor.read(cx).text().to_owned();
        let change = id.clone();
        let task = self.app.update(cx, |model, cx| {
            model.mutate(cx, move |backend, path| {
                backend.describe(path, &change, &text)
            })
        });
        let app = self.app.clone();
        let saved = cx.spawn(async move |_, cx| {
            let outcome = task.await?;
            let _ = app.update(cx, |model, _| model.discard_draft(&repo, &id));
            Ok(outcome)
        });
        self.run_panel(saved, window, cx);
    }

    fn mutate_node(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        action: impl FnOnce(
                &dyn jiji_core::RepoBackend,
                &std::path::Path,
                &str,
            ) -> Result<MutationOutcome, jiji_core::BackendError>
            + Send
            + 'static,
    ) {
        if self.header.acting {
            return;
        }
        let Some(id) = self.header.node.clone() else {
            return;
        };
        let task = self.app.update(cx, |model, cx| {
            model.mutate(cx, move |backend, path| action(backend, path, &id))
        });
        self.run_panel(task, window, cx);
    }

    /// The candidates of whichever destination list is open, filtered.
    fn dest_candidates(&self) -> Vec<&GraphNode> {
        let Some(id) = self.header.node.as_deref() else {
            return Vec::new();
        };
        match self.header.tray {
            Tray::Rebase => rebase_destinations(&self.snapshot, id, !self.header.rebase_alone),
            Tray::Split => squash_destinations(&self.snapshot, id),
            Tray::Compare => self.snapshot.nodes.iter().filter(|n| n.id != id).collect(),
            _ => Vec::new(),
        }
    }

    fn visible_dests(&self, cx: &App) -> Vec<String> {
        let query = self.header.filter.read(cx).text().trim().to_lowercase();
        self.dest_candidates()
            .into_iter()
            .filter(|node| matches_filter(node, &query))
            .map(|node| node.id.clone())
            .collect()
    }

    fn move_dest(&mut self, delta: isize, cx: &mut Context<Self>) {
        let picking = match self.header.tray {
            Tray::Rebase => true,
            Tray::Split => self.header.split_into,
            _ => false,
        };
        if !picking {
            return;
        }
        let list = self.visible_dests(cx);
        if list.is_empty() {
            return;
        }
        let index = self
            .header
            .dest
            .as_ref()
            .and_then(|dest| list.iter().position(|id| id == dest));
        let next = match index {
            None if delta > 0 => 0,
            None => list.len() - 1,
            Some(index) => (index as isize + delta).clamp(0, list.len() as isize - 1) as usize,
        };
        self.header.dest = Some(list[next].clone());
        self.header
            .dest_scroll
            .scroll_to_item(next, gpui::ScrollStrategy::Center);
        cx.notify();
    }

    /// Confirm the open tray. `command` is ⌘↵; plain ↵ only confirms the
    /// trays whose list is the whole decision.
    fn tray_submit(&mut self, command: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.header.acting {
            return;
        }
        match self.header.tray.clone() {
            Tray::Edit if command => self.save_description(window, cx),
            Tray::Confirm(confirm) if command => self.run_confirmed(confirm, window, cx),
            Tray::Rebase => self.submit_rebase(window, cx),
            Tray::Split if command || self.header.split_into => self.submit_split(window, cx),
            _ => {}
        }
    }

    fn run_confirmed(&mut self, confirm: Confirm, window: &mut Window, cx: &mut Context<Self>) {
        match confirm {
            Confirm::Squash => self.mutate_node(window, cx, |backend, path, id| {
                backend.squash_change(path, id)
            }),
            Confirm::Abandon => self.mutate_node(window, cx, |backend, path, id| {
                backend.abandon_change(path, id)
            }),
        }
    }

    fn submit_rebase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dest) = self.header.dest.clone() else {
            return;
        };
        let alone = self.header.rebase_alone;
        self.mutate_node(window, cx, move |backend, path, id| {
            if alone {
                backend.move_change(path, id, &dest)
            } else {
                backend.rebase_change(path, id, &dest)
            }
        });
    }

    fn split_ready(&self) -> bool {
        let summary = split_summary(self.header_files(), &self.header.split);
        selection_ready(
            &summary,
            self.header
                .split_into
                .then_some(self.header.dest.as_deref()),
        )
    }

    fn submit_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.split_ready() {
            return;
        }
        let Some(files) = self.header_files() else {
            return;
        };
        let selection = split_payload(files, &self.header.split);
        let dest = self.header.dest.clone().filter(|_| self.header.split_into);
        let description = self.header.split_desc.read(cx).text().to_owned();
        self.mutate_node(window, cx, move |backend, path, id| match &dest {
            Some(dest) => backend.squash_into(path, id, &selection, dest),
            None => backend.split_change(path, id, &selection, &description),
        });
    }

    fn submit_create_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.header.bookmark_name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.mutate_node(window, cx, move |backend, path, id| {
            backend.create_bookmark(path, &name, id)
        });
    }

    fn submit_rename_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Tray::Manage(name) = self.header.tray.clone() else {
            return;
        };
        let next = self.header.rename.read(cx).text().trim().to_owned();
        if next.is_empty() || next == name {
            return;
        }
        self.mutate_node(window, cx, move |backend, path, _| {
            backend.rename_bookmark(path, &name, &next)
        });
    }

    pub(super) fn on_header_input(
        &mut self,
        which: usize,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if *event == InputEvent::Submit {
            match which {
                0 => self.save_description(window, cx),
                1 => self.submit_create_bookmark(window, cx),
                2 => self.submit_rename_bookmark(window, cx),
                3 => self.tray_submit(false, window, cx),
                _ => self.tray_submit(true, window, cx),
            }
        }
        cx.notify();
    }

    /// Intents the header owns. Returns the intent back when it is not
    /// one of them.
    pub(super) fn take_header_intent(
        &mut self,
        intent: UiIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.header_node() else {
            return;
        };
        let avail = action_availability(&self.snapshot, node);
        let id = node.id.clone();
        match intent {
            UiIntent::Describe if avail.describe && self.header.tray != Tray::Edit => {
                self.open_editor(window, cx)
            }
            UiIntent::Bookmark if self.header.tray != Tray::Bookmark => {
                self.toggle_tray(Tray::Bookmark, window, cx)
            }
            UiIntent::Rebase if avail.rebase && self.header.tray != Tray::Rebase => {
                self.toggle_tray(Tray::Rebase, window, cx)
            }
            UiIntent::Split { into, path } if avail.split && self.header.tray != Tray::Split => {
                self.toggle_tray(Tray::Split, window, cx);
                self.header.split_into = into;
                if let Some(path) = path {
                    split::toggle_file(&mut self.header.split, &path);
                }
                if into {
                    self.focus_filter_or_tray(window, cx);
                }
            }
            UiIntent::Squash
                if avail.squash && self.header.tray != Tray::Confirm(Confirm::Squash) =>
            {
                self.toggle_tray(Tray::Confirm(Confirm::Squash), window, cx)
            }
            UiIntent::Abandon
                if avail.abandon && self.header.tray != Tray::Confirm(Confirm::Abandon) =>
            {
                self.toggle_tray(Tray::Confirm(Confirm::Abandon), window, cx)
            }
            UiIntent::Compare(None) if self.header.tray != Tray::Compare => {
                self.toggle_tray(Tray::Compare, window, cx)
            }
            UiIntent::Evolog => self
                .app
                .update(cx, |model, cx| model.open_evolution(id, cx)),
            _ => {}
        }
        cx.notify();
    }

    /// The rows a pending rewrite would touch, and the picked (or
    /// hovered) destination — what the graph washes while a plan panel is
    /// scrubbing.
    pub(super) fn rewrite_preview(&self) -> Option<(HashSet<String>, Option<String>)> {
        let node = self.header_node()?;
        let with_descendants = |id: &str| -> Vec<String> {
            std::iter::once(id.to_owned())
                .chain(
                    descendants_of(&self.snapshot, id)
                        .into_iter()
                        .map(|d| d.id.clone()),
                )
                .collect()
        };
        let header = &self.header;
        let picked = || header.dest_hover.clone().or(header.dest.clone());
        match &header.tray {
            Tray::Rebase => {
                let moving = if header.rebase_alone {
                    vec![node.id.clone()]
                } else {
                    with_descendants(&node.id)
                };
                Some((moving.into_iter().collect(), picked()))
            }
            Tray::Confirm(Confirm::Squash) => {
                let parent = match node.parents.as_slice() {
                    [only] => find_node(&self.snapshot, only)?,
                    _ => return None,
                };
                Some((with_descendants(&parent.id).into_iter().collect(), None))
            }
            Tray::Confirm(Confirm::Abandon) => {
                Some((with_descendants(&node.id).into_iter().collect(), None))
            }
            Tray::Split => {
                let mut ids = with_descendants(&node.id);
                let dest = header.split_into.then(picked).flatten();
                if let Some(dest) = &dest {
                    ids.extend(with_descendants(dest));
                }
                Some((ids.into_iter().collect(), dest))
            }
            _ => None,
        }
    }

    fn rerun_ci(&mut self, number: u64, cx: &mut Context<Self>) {
        if self.header.rerun.as_ref().is_some_and(|(_, busy, _)| *busy) {
            return;
        }
        let Some(path) = self.app.read(cx).repo_path() else {
            return;
        };
        self.header.rerun = Some((number, true, None));
        cx.notify();
        let asked = self
            .app
            .read(cx)
            .bg(cx, move |host| host.rerun_failed_ci(&path, number));
        cx.spawn(async move |this, cx| {
            let result = asked.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(report) => {
                        let mut parts = Vec::new();
                        if !report.rerun.is_empty() {
                            parts.push(format!(
                                "Re-running failed jobs in {}.",
                                report.rerun.join(", ")
                            ));
                        }
                        if !report.refused.is_empty() {
                            parts.push(format!("GitHub refused: {}.", report.refused.join("; ")));
                        }
                        let note = if parts.is_empty() {
                            "No failed GitHub Actions runs on this PR's head — the failing \
                             check may come from another CI system."
                                .to_owned()
                        } else {
                            parts.join(" ")
                        };
                        this.header.rerun = Some((number, false, Some(note)));
                        this.app.update(cx, |model, cx| model.refresh_forge_prs(cx));
                    }
                    Err(error) => {
                        this.header.rerun = None;
                        this.app
                            .update(cx, |model, cx| model.set_error(error.message, cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn abandon_copy(&mut self, sibling: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.header.acting {
            return;
        }
        let Some(node) = self.header_node() else {
            return;
        };
        let (kept_id, kept_change) = (node.id.clone(), node.change_id.clone());
        let task = self.app.update(cx, |model, cx| {
            model.mutate(cx, move |backend, path| {
                backend.abandon_change(path, &sibling)
            })
        });
        self.header.acting = true;
        self.header.diverge_error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.header.acting = false;
                match result {
                    Ok(_) => {
                        this.header.diverge_confirm = None;
                        // The survivor may go back to its change id once it
                        // is the only copy.
                        this.app.update(cx, |model, cx| {
                            let survivor = model.snapshot().and_then(|snapshot| {
                                find_node(snapshot, &kept_id)
                                    .or_else(|| find_node(snapshot, &kept_change))
                                    .map(|node| node.id.clone())
                            });
                            if let Some(id) = survivor {
                                model.jump_to_change(&id, cx);
                            }
                        });
                    }
                    Err(error) => this.header.diverge_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn jump_to(&mut self, id: String, cx: &mut Context<Self>) {
        self.app
            .update(cx, |model, cx| model.jump_to_change(&id, cx));
    }

    // ── Rendering: shared pieces ──

    fn tray(&self, danger: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme(cx);
        div()
            .id("tray")
            .key_context("Tray")
            .track_focus(&self.header.tray_focus)
            .flex()
            .flex_col()
            .px(px(16.))
            .py(px(12.))
            .border_t_1()
            .border_color(t.border2)
            .bg(if danger {
                Theme::blend(t.danger, 0.06, t.bg_well)
            } else {
                t.bg_well
            })
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.close_tray(window, cx)))
            .on_action(
                cx.listener(|this, _: &TrayConfirm, window, cx| this.tray_submit(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &TrayEnter, window, cx| this.tray_submit(false, window, cx)),
            )
            .on_action(cx.listener(|this, _: &TrayUp, _, cx| this.move_dest(-1, cx)))
            .on_action(cx.listener(|this, _: &TrayDown, _, cx| this.move_dest(1, cx)))
    }

    /// A tray's opening line: the verb and its subject in full ink, then
    /// quiet context.
    fn tray_title(&self, lead: &str, context: String, cx: &App) -> Div {
        let t = theme(cx);
        h_flex()
            .min_w_0()
            .gap(px(8.))
            .items_baseline()
            .text_size(px(TEXT_M))
            .font_weight(FontWeight::SEMIBOLD)
            .child(div().flex_shrink_0().child(rich(lead, cx)))
            .child(
                div()
                    .min_w_0()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(t.text3)
                    .child(truncated(context)),
            )
    }

    fn label(&self, text: &'static str, cx: &App) -> Div {
        div()
            .mt(px(10.))
            .mb(px(4.))
            .text_size(px(TEXT_XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme(cx).text3)
            .child(text)
    }

    /// What will happen, one consequence per line.
    fn consequences(&self, items: Vec<(String, bool)>, cx: &App) -> Div {
        let t = theme(cx);
        let mut list = v_flex()
            .mt(px(8.))
            .gap(px(2.))
            .text_size(px(TEXT_S))
            .line_height(gpui::relative(1.5));
        for (text, blocked) in items {
            list = list.child(
                h_flex()
                    .items_start()
                    .gap(px(6.))
                    .text_color(if blocked { t.warn } else { t.text2 })
                    .child(div().flex_shrink_0().child("•"))
                    .child(div().flex_1().min_w_0().child(rich(&text, cx))),
            );
        }
        list
    }

    /// A tray's closing row: a hint, the error if one, then the buttons.
    fn confirm_row(&self, hint: String, buttons: Vec<AnyElement>, cx: &App) -> Div {
        let t = theme(cx);
        let mut row = h_flex().mt(px(12.)).gap(px(8.)).child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .child(hint),
        );
        if let Some(error) = &self.header.error {
            row = row.child(
                div()
                    .id("tray-error")
                    .min_w_0()
                    .max_w(px(360.))
                    .text_size(px(TEXT_XS))
                    .text_color(t.danger)
                    .tooltip(tooltip(error.clone()))
                    .child(truncated(
                        error.lines().next().unwrap_or_default().to_owned(),
                    )),
            );
        }
        row.children(buttons)
    }

    fn cancel_button(&self, label: &'static str, cx: &mut Context<Self>) -> AnyElement {
        btn("tray-cancel", label)
            .disabled(self.header.acting)
            .on_click(cx.listener(|this, _, window, cx| this.close_tray(window, cx)))
            .into_any_element()
    }

    /// One row of a destination list: marker, short id, title, bookmarks.
    fn dest_row(
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        key: usize,
        selected: bool,
        theme: &Theme,
        cx: &App,
    ) -> Stateful<Div> {
        let glyph_color = match node.kind {
            NodeKind::WorkingCopy => theme.working_copy,
            NodeKind::Mutable => theme.text2,
            NodeKind::Immutable => theme.text3,
        };
        let mut row = h_flex()
            .id(("dest", key))
            .w_full()
            .h(px(26.))
            .gap(px(8.))
            .px(px(8.))
            .rounded(px(RADIUS_S))
            .text_size(px(TEXT_S))
            .cursor_pointer()
            .child(
                div()
                    .w(px(12.))
                    .flex_shrink_0()
                    .font_family(theme.mono.clone())
                    .text_color(glyph_color)
                    .child(kind_glyph(node.kind)),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .font_family(theme.mono.clone())
                    .text_color(theme.text3)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text1)
                            .child(short(&node.id, 2)),
                    )
                    .child(node.id.chars().skip(2).take(2).collect::<String>()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.text2)
                    .child(truncated(title_or_none(&node.description).to_owned())),
            );
        for name in &node.bookmarks {
            let trunk = *name == snapshot.trunk_bookmark;
            let mut chip = ref_label(None, name.clone(), cx).max_w(px(140.));
            if trunk {
                chip = chip.bg(theme.accent_dim).text_color(theme.accent_strong);
            }
            row = row.child(chip);
        }
        if selected {
            row.bg(theme.accent_dim)
        } else {
            let hover = theme.bg_hover;
            row.hover(move |style| style.bg(hover))
        }
    }

    /// The scrolling destination list of the open tray.
    fn dest_list(&mut self, empty: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let ids = Rc::new(self.visible_dests(cx));
        let frame = div()
            .mt(px(2.))
            .p(px(2.))
            .rounded(px(RADIUS_M))
            .border_1()
            .border_color(t.border2)
            .bg(t.bg1);
        if ids.is_empty() {
            return frame
                .child(
                    div()
                        .px(px(8.))
                        .py(px(6.))
                        .text_size(px(TEXT_S))
                        .text_color(t.text3)
                        .child(empty),
                )
                .into_any_element();
        }
        let compare = self.header.tray == Tray::Compare;
        let height = (ids.len() as f32 * 26.).min(200.);
        let list = uniform_list(
            "dest-list",
            ids.len(),
            cx.processor({
                let ids = ids.clone();
                move |this, range: Range<usize>, _, cx| {
                    let t = theme(cx);
                    let picked = if compare {
                        match &this.compare {
                            CompareMode::Change(id) => Some(id.clone()),
                            _ => None,
                        }
                    } else {
                        this.header.dest.clone()
                    };
                    let mut rows = Vec::with_capacity(range.len());
                    for ix in range {
                        let id = ids[ix].clone();
                        let Some(node) = find_node(&this.snapshot, &id) else {
                            rows.push(div().h(px(26.)).into_any_element());
                            continue;
                        };
                        let selected = picked.as_deref() == Some(id.as_str());
                        let hover_id = id.clone();
                        let row = Self::dest_row(&this.snapshot, node, ix, selected, &t, cx)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if compare {
                                    this.set_compare(CompareMode::Change(id.clone()), cx);
                                    this.close_tray(window, cx);
                                } else {
                                    this.header.dest = Some(id.clone());
                                    cx.notify();
                                }
                            }))
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                let next = hovered.then(|| hover_id.clone());
                                if (*hovered || this.header.dest_hover.as_ref() == Some(&hover_id))
                                    && this.header.dest_hover != next
                                {
                                    this.header.dest_hover = next;
                                    cx.notify();
                                }
                            }));
                        rows.push(row.into_any_element());
                    }
                    rows
                }
            }),
        )
        .track_scroll(self.header.dest_scroll.clone())
        .h(px(height));
        frame.child(list).into_any_element()
    }

    fn dest_filter(&self, window: &Window, cx: &App) -> Option<Div> {
        (self.dest_candidates().len() > 6).then(|| {
            field(&self.header.filter, window, cx)
                .w(px(260.))
                .font_family(theme(cx).mono.clone())
        })
    }

    // ── Rendering: the bars ──

    pub(super) fn render_change_header(
        &mut self,
        node_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let snapshot = self.snapshot.clone();
        let Some(node) = find_node(&snapshot, node_id) else {
            return div().into_any_element();
        };
        let avail = action_availability(&snapshot, node);
        let editing = self.header.tray == Tray::Edit;
        let mut header = v_flex()
            .flex_shrink_0()
            .border_b_1()
            .border_color(t.border2)
            .child(self.render_title_bar(node, &avail, cx))
            .child(self.render_meta_bar(&snapshot, node, cx));
        if self.header.tray == Tray::None {
            if let Some(error) = &self.header.error {
                header = header.child(
                    div()
                        .px(px(16.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(t.border2)
                        .text_size(px(TEXT_S))
                        .text_color(t.danger)
                        .bg(Theme::fade(t.danger, 0.07))
                        .child(error.clone()),
                );
            }
        }
        if editing {
            header = header.child(self.render_editor(window, cx));
        }
        let body = node
            .description
            .split_once('\n')
            .map(|(_, rest)| rest.trim())
            .filter(|rest| !rest.is_empty());
        if let (true, Some(body), false) = (self.header.body_open, body, editing) {
            header = header.child(
                div()
                    .id("description-body")
                    .px(px(16.))
                    .pt(px(8.))
                    .pb(px(12.))
                    .max_h(px(200.))
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(t.border2)
                    .text_size(px(TEXT_M))
                    .line_height(gpui::relative(1.55))
                    .text_color(t.text2)
                    .child(body.to_owned()),
            );
        }
        if node.is_divergent {
            header = header.children(self.render_divergence(&snapshot, node, cx));
        }
        if let Some(note) = self.compare_note(&snapshot, node) {
            header = header.child(
                div()
                    .px(px(16.))
                    .py(px(5.))
                    .border_t_1()
                    .border_color(t.border2)
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .child(note),
            );
        }
        let tray = match self.header.tray.clone() {
            Tray::Compare => Some(self.render_compare_tray(&snapshot, node, window, cx)),
            Tray::Confirm(confirm) => self.render_confirm_tray(&snapshot, node, confirm, cx),
            Tray::Rebase => Some(self.render_rebase_tray(&snapshot, node, window, cx)),
            Tray::Split => Some(self.render_split_tray(&snapshot, node, window, cx)),
            Tray::Bookmark => Some(self.render_bookmark_tray(&snapshot, node, window, cx)),
            Tray::Manage(name) => self.render_manage_tray(&snapshot, node, &name, window, cx),
            Tray::None | Tray::Edit => None,
        };
        header.children(tray).into_any_element()
    }

    fn render_title_bar(
        &mut self,
        node: &GraphNode,
        avail: &ChangeActions,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let editing = self.header.tray == Tray::Edit;
        let acting = self.header.acting;
        let title = summary_of(&node.description).to_owned();
        let has_body = node
            .description
            .split_once('\n')
            .is_some_and(|(_, rest)| !rest.trim().is_empty());
        let has_draft = self
            .app
            .read(cx)
            .draft_for(&self.snapshot.repo_path, &node.id, &node.description)
            .is_some();
        let mut bar = h_flex()
            .h(px(TOOLBAR_H))
            .min_w_0()
            .gap(px(8.))
            .pl(px(16.))
            .pr(px(8.))
            .border_b_1()
            .border_color(t.border2);
        if title.is_empty() {
            let mut undescribed = h_flex()
                .min_w_0()
                .gap(px(8.))
                .text_size(px(TEXT_L))
                .italic()
                .text_color(t.text3)
                .child("No description yet");
            if avail.describe {
                undescribed = undescribed.child(
                    h_flex()
                        .id("describe")
                        .flex_shrink_0()
                        .gap(px(5.))
                        .h(px(22.))
                        .px(px(7.))
                        .rounded(px(RADIUS_S))
                        .not_italic()
                        .text_size(px(TEXT_S))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.warn)
                        .bg(t.warn_dim)
                        .cursor_pointer()
                        .hover(|style| style.bg(Theme::fade(t.warn, 0.22)))
                        .on_click(cx.listener(|this, _, window, cx| this.open_editor(window, cx)))
                        .child(icon(IconName::Edit, 11., t.warn))
                        .child(if has_draft {
                            "Resume draft"
                        } else {
                            "Describe"
                        }),
                );
            } else {
                undescribed = undescribed.child(
                    div()
                        .not_italic()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_S))
                        .child("jj describe"),
                );
            }
            bar = bar.child(undescribed);
        } else {
            bar = bar.child(
                div()
                    .id("change-title")
                    .min_w_0()
                    .text_size(px(TEXT_L))
                    .font_weight(FontWeight::SEMIBOLD)
                    .tooltip(tooltip(title.clone()))
                    .child(truncated(title)),
            );
            if has_body {
                let open = self.header.body_open;
                bar = bar.child(
                    icon_btn(
                        "disclose-body",
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                    )
                    .size(22., 13.)
                    .tooltip(if open {
                        "Hide full description"
                    } else {
                        "Show full description"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.header.body_open = !this.header.body_open;
                        cx.notify();
                    })),
                );
            }
            if avail.describe && !editing {
                bar = bar.child(
                    icon_btn("edit-description", IconName::Edit)
                        .size(22., 12.)
                        .color(if has_draft { t.warn } else { t.text3 })
                        .tooltip(if has_draft {
                            "Edit description (unsaved draft)"
                        } else {
                            "Edit description"
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.open_editor(window, cx))),
                );
            }
        }
        bar = bar.child(div().flex_1());
        if !editing {
            let action =
                |id: &'static str, label: &'static str, glyph: IconName, tip: &'static str| {
                    btn(id, label)
                        .ghost()
                        .icon(glyph)
                        .disabled(acting)
                        .tooltip(tip)
                };
            let mut actions = h_flex().flex_shrink_0().gap(px(1.)).child(
                action(
                    "action-new",
                    "New",
                    IconName::Plus,
                    "Start a new change on top of this one (jj new)",
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.mutate_node(window, cx, |backend, path, id| backend.new_change(path, id))
                })),
            );
            if avail.edit {
                actions = actions.child(
                    action(
                        "action-edit",
                        "Edit",
                        IconName::AtSign,
                        "Make this the working copy (jj edit)",
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.mutate_node(window, cx, |backend, path, id| {
                            backend.edit_change(path, id)
                        })
                    })),
                );
            }
            actions = actions
                .child(
                    action(
                        "action-bookmark",
                        "Bookmark",
                        IconName::Bookmark,
                        "Create or move a bookmark onto this change (jj bookmark)",
                    )
                    .on(self.header.tray == Tray::Bookmark)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_tray(Tray::Bookmark, window, cx)
                    })),
                )
                .child(self.render_more_menu(avail, cx));
            bar = bar.child(actions);
        }
        bar.child(
            icon_btn("close-change", IconName::Close)
                .size(24., 13.)
                .tooltip("Close (esc)")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.app.update(cx, |model, cx| model.select_node(None, cx));
                })),
        )
    }

    fn render_more_menu(
        &mut self,
        avail: &ChangeActions,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let open = self.header.more.is_open();
        let acting = self.header.acting;
        let mut anchor = div().relative().child(
            btn("action-more", "More")
                .ghost()
                .on(open)
                .disabled(acting)
                .tooltip("More change actions")
                .trailing(icon(IconName::ChevronDown, 12., t.text3))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.header.more.toggle();
                    cx.notify();
                })),
        );
        if !open {
            return anchor;
        }
        let item = |id: &'static str,
                    glyph: IconName,
                    label: &'static str,
                    command: &'static str,
                    armed: bool,
                    danger: bool| {
            let fg = if danger { t.danger } else { t.text1 };
            let hover = if danger { t.danger_dim } else { t.bg_hover };
            let mut row = h_flex()
                .id(id)
                .h(px(28.))
                .gap(px(8.))
                .px(px(8.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_M))
                .text_color(fg)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(icon(glyph, 14., if danger { t.danger } else { t.text3 }))
                .child(div().flex_1().child(label))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_XS))
                        .text_color(t.text3)
                        .child(command),
                );
            if armed {
                row = row.bg(hover);
            }
            row
        };
        let tray = self.header.tray.clone();
        let evolution_open = self
            .app
            .read(cx)
            .ui()
            .evolution
            .as_deref()
            .is_some_and(|id| Some(id) == self.header.node.as_deref());
        let mut panel = popover(cx)
            .w(px(248.))
            .p(px(4.))
            .flex()
            .flex_col()
            .swallow_clicks()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.header.more.close();
                cx.notify();
            }));
        let open_tray = |tray: Tray| {
            move |this: &mut Self,
                  _: &gpui::ClickEvent,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
                this.header.more.close();
                this.toggle_tray(tray.clone(), window, cx);
            }
        };
        if avail.rebase {
            panel = panel.child(
                item(
                    "more-rebase",
                    IconName::Rebase,
                    "Rebase…",
                    "jj rebase",
                    tray == Tray::Rebase,
                    false,
                )
                .tooltip(tooltip(
                    "Move this change onto a different parent (jj rebase)",
                ))
                .on_click(cx.listener(open_tray(Tray::Rebase))),
            );
        }
        if avail.split {
            panel = panel.child(
                item("more-split", IconName::Split, "Split…", "jj split", tray == Tray::Split, false)
                    .tooltip(tooltip(
                        "Split this change in two, or move files into another change (jj split / jj squash --into)",
                    ))
                    .on_click(cx.listener(open_tray(Tray::Split))),
            );
        }
        if avail.squash {
            panel = panel.child(
                item(
                    "more-squash",
                    IconName::Squash,
                    "Squash into parent…",
                    "jj squash",
                    tray == Tray::Confirm(Confirm::Squash),
                    false,
                )
                .tooltip(tooltip(
                    "Move this change's content into its parent (jj squash)",
                ))
                .on_click(cx.listener(open_tray(Tray::Confirm(Confirm::Squash)))),
            );
        }
        panel = panel.child(
            item("more-evolog", IconName::Operations, "Evolution", "jj evolog", evolution_open, false)
                .tooltip(tooltip(
                    "How this change was rewritten over time (jj evolog) — a scrubbable timeline of its versions",
                ))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.header.more.close();
                    let Some(id) = this.header.node.clone() else {
                        return;
                    };
                    this.app.update(cx, |model, cx| {
                        if evolution_open {
                            model.close_evolution(cx)
                        } else {
                            model.open_evolution(id, cx)
                        }
                    });
                    cx.notify();
                })),
        );
        if avail.abandon {
            panel = panel.child(crate::ui::separator(cx)).child(
                item(
                    "more-abandon",
                    IconName::Trash,
                    "Abandon…",
                    "jj abandon",
                    tray == Tray::Confirm(Confirm::Abandon),
                    true,
                )
                .tooltip(tooltip("Abandon this change (jj abandon)"))
                .on_click(cx.listener(open_tray(Tray::Confirm(Confirm::Abandon)))),
            );
        }
        anchor = anchor.child(
            div()
                .absolute()
                .top(px(30.))
                .right_0()
                .child(floating(Corner::TopRight, panel)),
        );
        anchor
    }

    fn compare_label(&self, snapshot: &RepoSnapshot, from: Option<&str>) -> String {
        match (from, &self.compare) {
            (None, _) => "vs parent".into(),
            (Some(_), CompareMode::Trunk) => format!(
                "vs {}",
                if snapshot.trunk_bookmark.is_empty() {
                    "trunk"
                } else {
                    &snapshot.trunk_bookmark
                }
            ),
            (Some(_), CompareMode::Base) => "vs stack base".into(),
            (Some(from), _) => format!("vs {}", short(from, 4)),
        }
    }

    fn compare_note(&self, snapshot: &RepoSnapshot, node: &GraphNode) -> Option<String> {
        let from = resolve_compare_from(snapshot, &node.id, &self.compare)?;
        match move_direction(snapshot, &from, &node.id)? {
            MoveDirection::Backwards => Some(format!(
                "Comparing against a descendant — the diff reads in reverse: what going back to {} would undo.",
                short(&node.id, 4)
            )),
            MoveDirection::Sideways => Some(
                "Comparing across branches — everything that differs between the two trees.".into(),
            ),
            MoveDirection::Forward => None,
        }
    }

    fn render_meta_bar(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let mono = t.mono.clone();
        let mut facts = h_flex()
            .flex_1()
            .min_w_0()
            .flex_wrap()
            .gap_x(px(8.))
            .gap_y(px(4.));
        if node.is_divergent {
            facts = facts
                .child(
                    h_flex()
                        .id("fact-ids")
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .text_color(t.danger)
                        .tooltip(tooltip(
                            "Divergent change: several visible commits share this change id",
                        ))
                        .child(node.change_id.clone())
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("??")),
                )
                .child(
                    div()
                        .id("fact-commit")
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .tooltip(tooltip("This copy's commit id"))
                        .child(node.commit_id.clone()),
                );
        } else {
            facts = facts
                .child(
                    h_flex()
                        .id("fact-ids")
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .text_color(t.text2)
                        .tooltip(tooltip("Change id"))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.text1)
                                .child(short(&node.id, 2)),
                        )
                        .child(node.id.chars().skip(2).collect::<String>()),
                )
                .child(
                    div()
                        .id("fact-commit")
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .tooltip(tooltip(format!("Commit id {}", node.commit_id)))
                        .child(short(&node.commit_id, 8)),
                );
        }
        match node.kind {
            NodeKind::WorkingCopy => {
                facts = facts.child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.working_copy)
                        .child("working copy"),
                )
            }
            NodeKind::Immutable => {
                facts = facts.child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::MEDIUM)
                        .child("immutable"),
                )
            }
            NodeKind::Mutable => {}
        }
        facts = facts
            .child(div().max_w(px(160.)).child(truncated(node.author.clone())))
            .child(
                div()
                    .id("fact-age")
                    .flex_shrink_0()
                    .tooltip(tooltip(crate::ui::absolute_time(&node.timestamp)))
                    .child(from_now(&node.timestamp)),
            );
        if node.is_empty {
            facts = facts.child(div().flex_shrink_0().child("empty"));
        }
        if node.has_conflict {
            facts = facts.child(
                h_flex()
                    .flex_shrink_0()
                    .gap(px(4.))
                    .h(px(17.))
                    .px(px(5.))
                    .rounded(px(RADIUS_S))
                    .bg(t.danger_dim)
                    .text_size(px(TEXT_XS))
                    .text_color(t.danger)
                    .child(icon(IconName::Conflicts, 11., t.danger))
                    .child("conflicts"),
            );
        }
        let prs = self.app.read(cx).forge.prs.clone();
        for (ix, mark) in bookmarks_at(snapshot, &node.id).into_iter().enumerate() {
            facts = facts.children(self.bookmark_facts(mark, ix, prs.as_deref(), cx));
        }
        if let Some(position) = stack_position(snapshot, &node.id) {
            let (at, of) = (position.index + 1, position.workstream.node_ids.len());
            let title = position.workstream.title.clone();
            facts = facts.child(
                div()
                    .id("fact-stack")
                    .min_w_0()
                    .max_w(px(280.))
                    .tooltip(tooltip(format!("{at} of {of} in \u{201c}{title}\u{201d}")))
                    .child(truncated(format!("{at}/{of} in {title}"))),
            );
        }
        let relation = |key: usize, id: &str, role: &'static str, glyph: IconName| {
            let description = find_node(snapshot, id)
                .map(|n| summary_of(&n.description).to_owned())
                .filter(|title| !title.is_empty());
            let tip = match description {
                Some(description) => format!("Go to {role} {id} — {description}"),
                None => format!("Go to {role} {id}"),
            };
            let target = id.to_owned();
            h_flex()
                .id(("relation", key))
                .flex_shrink_0()
                .gap(px(2.))
                .h(px(18.))
                .px(px(4.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_XS))
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover))
                .tooltip(tooltip(tip))
                .on_click(cx.listener(move |this, _, _, cx| this.jump_to(target.clone(), cx)))
                .child(icon(glyph, 10., t.text3))
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_color(t.text2)
                        .child(short(id, 4)),
                )
        };
        let mut key = 0;
        for child in children_of(snapshot, &node.id) {
            facts = facts.child(relation(key, &child.id, "child", IconName::ArrowUp));
            key += 1;
        }
        for parent in &node.parents {
            facts = facts.child(relation(key, parent, "parent", IconName::ArrowDown));
            key += 1;
        }

        h_flex()
            .min_h(px(30.))
            .min_w_0()
            .gap(px(8.))
            .pl(px(16.))
            .pr(px(8.))
            .py(px(3.))
            .text_size(px(TEXT_S))
            .text_color(t.text3)
            .child(facts)
            .child(self.render_view_controls(snapshot, node, cx))
    }

    /// A bookmark on this change: its chip (click to rename or delete a
    /// local one), then its pull request and the CI re-run when one
    /// applies.
    fn bookmark_facts(
        &mut self,
        mark: &BookmarkState,
        key: usize,
        prs: Option<&jiji_forge::RepoPrState>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = theme(cx);
        let (sync_text, tone) = sync_label(mark.sync);
        let full = match &mark.remote {
            Some(remote) => format!("{}@{remote}", mark.name),
            None => mark.name.clone(),
        };
        let manageable = mark.is_local && !mark.is_trunk;
        let armed = self.header.tray == Tray::Manage(mark.name.clone());
        let mut chip = h_flex()
            .id(("bookmark-chip", key))
            .flex_shrink_0()
            .gap(px(5.))
            .h(px(17.))
            .px(px(5.))
            .rounded(px(RADIUS_S))
            .font_family(t.mono.clone())
            .text_size(px(TEXT_XS))
            .on_mouse_down(
                gpui::MouseButton::Right,
                crate::views::menu::bookmark_menu(mark.name.clone(), mark.remote.clone()),
            )
            .child(div().max_w(px(180.)).child(truncated(mark.name.clone())))
            .child(
                div()
                    .font_family(crate::theme::FONT_UI)
                    .text_color(tone_color(&t, tone))
                    .child(sync_text),
            );
        chip = if mark.is_trunk {
            chip.bg(t.accent_dim).text_color(t.accent_strong)
        } else {
            chip.bg(if armed { t.bg_hover } else { t.ref_bg })
                .text_color(t.text2)
        };
        chip = if manageable {
            let name = mark.name.clone();
            chip.cursor_pointer()
                .hover(|style| style.bg(t.bg_hover))
                .tooltip(tooltip(format!("{full} — {sync_text} · rename or delete")))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.toggle_tray(Tray::Manage(name.clone()), window, cx)
                }))
        } else {
            chip.tooltip(tooltip(format!("{full} — {sync_text}")))
        };
        let mut out = vec![chip.into_any_element()];
        let Some(pr) = pr_for_bookmark(mark, prs) else {
            return out;
        };
        // The bookmark's pull request: a one-click route to GitHub, wearing
        // the same state the graph badge shows.
        let badge = pr_badge(pr);
        let (fg, bg) = match badge.tone {
            PrTone::Open => (t.ok, t.ok_dim),
            PrTone::Draft => (t.text3, t.ref_bg),
            PrTone::Merged => (t.accent_strong, t.accent_dim),
            PrTone::Closed => (t.danger, t.danger_dim),
        };
        let url = pr.url.clone();
        let mut pr_chip = h_flex()
            .id(("pr-chip", key))
            .flex_shrink_0()
            .gap(px(4.))
            .h(px(17.))
            .px(px(5.))
            .rounded(px(RADIUS_S))
            .bg(bg)
            .text_size(px(TEXT_XS))
            .text_color(fg)
            .cursor_pointer()
            .tooltip(tooltip(format!("{} · open on GitHub", badge.title)))
            .on_click(move |_, _, cx| cx.open_url(&url))
            .on_mouse_down(
                gpui::MouseButton::Right,
                crate::views::menu::pr_menu(pr.number),
            )
            .child(div().font_family(t.mono.clone()).child(badge.label.clone()))
            .child(badge.tone.word());
        for glyph in [badge.review, badge.checks].into_iter().flatten() {
            pr_chip = pr_chip.child(
                div()
                    .text_color(tone_color(&t, glyph.tone))
                    .child(glyph.glyph),
            );
        }
        out.push(pr_chip.child("↗").into_any_element());
        if pr.state == jiji_forge::PrState::Open && pr.checks == jiji_forge::ChecksRollup::Failing {
            // A failing PR offers the way back to green: ask GitHub
            // Actions to re-run only the failed jobs.
            let number = pr.number;
            let mine = self
                .header
                .rerun
                .as_ref()
                .filter(|(asked, _, _)| *asked == number);
            let busy = mine.is_some_and(|(_, busy, _)| *busy);
            let label = match mine {
                Some((_, true, _)) => "Re-running…",
                Some(_) => "CI re-run requested",
                None => "Re-run failed CI",
            };
            let tip = mine
                .and_then(|(_, _, note)| note.clone())
                .unwrap_or_else(|| format!("Re-run the failed GitHub Actions jobs on #{number}"));
            let mut rerun = div()
                .id(("rerun-ci", key))
                .flex_shrink_0()
                .h(px(17.))
                .px(px(5.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_XS))
                .text_color(t.accent_strong)
                .tooltip(tooltip(tip))
                .child(label);
            if !busy {
                rerun = rerun
                    .cursor_pointer()
                    .hover(|style| style.bg(t.accent_dim))
                    .on_click(cx.listener(move |this, _, _, cx| this.rerun_ci(number, cx)));
            }
            out.push(rerun.into_any_element());
        }
        out
    }

    fn render_view_controls(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let from = resolve_compare_from(snapshot, &node.id, &self.compare);
        let comparing = from.is_some();
        let tip = match &from {
            Some(from) => format!(
                "Comparing against {from} \u{201c}{}\u{201d} — click to change",
                find_node(snapshot, from).map_or("no description", |n| title_or_none(&n.description))
            ),
            None => "Compare against trunk, the stack base, or any change — or ⌘-click a row in the graph"
                .to_owned(),
        };
        let fg = if comparing { t.accent_strong } else { t.text2 };
        let mut compare = h_flex().flex_shrink_0().child({
            let mut chip = h_flex()
                .id("compare-chip")
                .gap(px(5.))
                .h(px(22.))
                .px(px(7.))
                .rounded(px(RADIUS_S))
                .text_color(fg)
                .cursor_pointer()
                .tooltip(tooltip(tip))
                .on_click(
                    cx.listener(|this, _, window, cx| this.toggle_tray(Tray::Compare, window, cx)),
                )
                .child(icon(
                    IconName::Compare,
                    12.,
                    if comparing { fg } else { t.text3 },
                ))
                .child(self.compare_label(snapshot, from.as_deref()));
            chip = if comparing {
                chip.bg(t.accent_dim)
            } else {
                if self.header.tray == Tray::Compare {
                    chip = chip.bg(t.bg_hover);
                }
                chip.hover(|style| style.bg(t.bg_hover).text_color(t.text1))
            };
            chip
        });
        if comparing {
            compare = compare.child(
                div()
                    .id("compare-reset")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(18.))
                    .h(px(22.))
                    .rounded_r(px(RADIUS_S))
                    .bg(t.accent_dim)
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::fade(t.accent, 0.3)))
                    .tooltip(tooltip("Back to the parent diff"))
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_compare(CompareMode::Parent, cx)),
                    )
                    .child(icon(IconName::Close, 10., t.accent_strong)),
            );
        }
        h_flex()
            .flex_shrink_0()
            .gap(px(4.))
            .min_h(px(24.))
            .child(compare)
            .children(self.render_files_menu(cx))
            .child(self.tree_toggle(cx))
            .child(self.layout_toggle(cx))
    }

    /// "N files +a −r": the jump-to-a-file menu.
    fn render_files_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx);
        let files = self.surface.read(cx).files();
        self.diff.current()?;
        let count = files.len();
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), file| (a + file.added, r + file.removed));
        let open = self.header.files.is_open();
        let stat = |sign: &str, n: u32, color: Hsla| {
            (n > 0).then(|| {
                div()
                    .font_family(t.mono.clone())
                    .text_color(color)
                    .child(format!("{sign}{n}"))
            })
        };
        let mut button = h_flex()
            .id("files-button")
            .gap(px(6.))
            .h(px(22.))
            .px(px(7.))
            .rounded(px(RADIUS_S))
            .text_color(t.text2)
            .child(plural(count, "file", "files"))
            .children(stat("+", added, t.add_fg))
            .children(stat("−", removed, t.del_fg));
        if count == 0 {
            button = button.opacity(0.6);
        } else {
            if open {
                button = button.bg(t.bg_hover);
            }
            button = button
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
                .tooltip(tooltip("Jump to a file"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.header.files.toggle();
                    cx.notify();
                }));
        }
        let mut anchor = div().relative().child(button);
        if open {
            let mut list = v_flex().p(px(4.));
            for (index, file) in files.iter().enumerate() {
                let (glyph, hue) = match file.status {
                    jiji_core::snapshot::FileStatus::Added => ("A", t.add_fg),
                    jiji_core::snapshot::FileStatus::Modified => ("M", t.warn),
                    jiji_core::snapshot::FileStatus::Removed => ("D", t.del_fg),
                    _ => ("R", t.accent_strong),
                };
                let (dir, name) = split_path(&file.path);
                list = list.child(
                    h_flex()
                        .id(("jump-file", index))
                        .w_full()
                        .h(px(26.))
                        .gap(px(8.))
                        .px(px(8.))
                        .rounded(px(RADIUS_S))
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_S))
                        .cursor_pointer()
                        .hover(|style| style.bg(t.bg_hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.header.files.close();
                            this.jump_to_file(index, cx);
                            cx.notify();
                        }))
                        .child(
                            div()
                                .w(px(12.))
                                .flex_shrink_0()
                                .text_size(px(TEXT_XS))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(hue)
                                .child(glyph),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .min_w_0()
                                        .text_color(t.text3)
                                        .child(truncated(dir.to_owned())),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(t.text1)
                                        .child(name.to_owned()),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .gap(px(6.))
                                .text_size(px(TEXT_XS))
                                .children(stat("+", file.added, t.add_fg))
                                .children(stat("−", file.removed, t.del_fg)),
                        ),
                );
            }
            let panel = popover(cx)
                .relative()
                .w(px(420.))
                .swallow_clicks()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.header.files.close();
                    cx.notify();
                }))
                .child(
                    div()
                        .id("files-menu")
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .track_scroll(&self.header.files_scroll)
                        .child(list),
                )
                .child(scrollbar("files-menu-scrollbar", &self.header.files_scroll));
            anchor = anchor.child(
                div()
                    .absolute()
                    .top(px(26.))
                    .right_0()
                    .child(floating(Corner::TopRight, panel)),
            );
        }
        Some(anchor.into_any_element())
    }

    // ── Rendering: the trays ──

    fn render_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let saving = self.header.acting;
        let mut hint = h_flex()
            .flex_1()
            .min_w_0()
            .gap(px(4.))
            .text_size(px(TEXT_XS))
            .text_color(t.text3);
        if self.header.draft_restored {
            hint = hint.child("Unsaved draft restored ·").child(
                div()
                    .id("discard-draft")
                    .text_color(t.accent_strong)
                    .cursor_pointer()
                    .hover(|style| style.text_color(t.text1))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.discard_editor_draft(window, cx)),
                    )
                    .child("Discard"),
            );
        } else {
            hint = hint.child("First line becomes the summary · ⌘↵ to save");
        }
        let mut row = h_flex().mt(px(8.)).gap(px(8.)).child(hint);
        if let Some(error) = &self.header.error {
            row = row.child(
                div()
                    .min_w_0()
                    .max_w(px(360.))
                    .text_size(px(TEXT_XS))
                    .text_color(t.danger)
                    .child(truncated(error.clone())),
            );
        }
        row = row
            .child(
                btn("editor-cancel", "Cancel")
                    .disabled(saving)
                    .tooltip("Close, keeping the draft for later (esc)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_tray(window, cx))),
            )
            .child(
                btn("editor-save", if saving { "Saving…" } else { "Save" })
                    .primary()
                    .disabled(saving)
                    .on_click(cx.listener(|this, _, window, cx| this.save_description(window, cx))),
            );
        let focused = self.header.editor.read(cx).is_focused(window);
        self.tray(false, cx)
            .child(
                div()
                    .flex()
                    .min_h(px(84.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(RADIUS_M))
                    .border_1()
                    .border_color(if focused { t.accent } else { t.border1 })
                    .bg(t.bg1)
                    .text_size(px(TEXT_M))
                    .line_height(px(20.))
                    .when(saving, |el| el.opacity(0.6))
                    .child(self.header.editor.clone()),
            )
            .child(row)
            .into_any_element()
    }

    fn render_divergence(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = theme(cx);
        let siblings = divergent_siblings(snapshot, node);
        let acting = self.header.acting;
        let mut note = h_flex()
            .flex_wrap()
            .gap_x(px(8.))
            .gap_y(px(4.))
            .px(px(16.))
            .py(px(6.))
            .border_t_1()
            .border_color(t.border2)
            .bg(Theme::fade(t.danger, 0.06))
            .text_size(px(TEXT_S))
            .text_color(t.text2)
            .child(div().child(rich(
                &format!(
                    "Divergent change — {} visible commits share **{}**, so each copy goes by its commit id. Keep one and abandon the other{} to resolve.",
                    siblings.len() + 1,
                    node.change_id,
                    if siblings.len() == 1 { "" } else { "s" }
                ),
                cx,
            )));
        let small = |id: gpui::ElementId, label: &'static str, color: Hsla| {
            div()
                .id(id)
                .h(px(18.))
                .px(px(5.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_XS))
                .text_color(color)
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover))
                .child(label)
        };
        for (ix, sibling) in siblings.iter().enumerate() {
            let commit = short(&sibling.commit_id, 8);
            let (jump_id, compare_id, confirm_id) =
                (sibling.id.clone(), sibling.id.clone(), sibling.id.clone());
            let comparing = self.compare == CompareMode::Change(sibling.id.clone());
            let mut group = h_flex().gap(px(2.)).child(
                div()
                    .id(("copy", ix))
                    .h(px(18.))
                    .px(px(5.))
                    .rounded(px(RADIUS_S))
                    .bg(t.ref_bg)
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_XS))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.bg_hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.jump_to(jump_id.clone(), cx)))
                    .child(commit.clone()),
            );
            group = group.child(
                small(
                    ("copy-compare", ix).into(),
                    "Compare",
                    if comparing { t.accent_strong } else { t.text2 },
                )
                .tooltip(tooltip(format!(
                    "Everything that differs between copy {commit} and this one"
                )))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_compare(CompareMode::Change(compare_id.clone()), cx)
                })),
            );
            if sibling.kind != NodeKind::Immutable && !acting {
                group = group.child(
                    small(("copy-abandon", ix).into(), "Abandon", t.danger)
                        .tooltip(tooltip(format!("Abandon copy {commit} and keep this one")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.header.diverge_confirm =
                                if this.header.diverge_confirm.as_ref() == Some(&confirm_id) {
                                    None
                                } else {
                                    Some(confirm_id.clone())
                                };
                            cx.notify();
                        })),
                );
            }
            note = note.child(group);
        }
        if let Some(error) = &self.header.diverge_error {
            note = note.child(
                div()
                    .min_w_0()
                    .text_color(t.danger)
                    .child(truncated(error.clone())),
            );
        }
        let mut out = vec![note.into_any_element()];
        let confirming = self.header.diverge_confirm.clone();
        if let Some(sibling) = siblings
            .iter()
            .find(|sibling| Some(&sibling.id) == confirming.as_ref())
        {
            let id = sibling.id.clone();
            let text = format!(
                "Abandon copy **{}** \u{201c}{}\u{201d} and keep this one. Bookmarks on it are deleted and its descendants rebase onto its parents{}. Undoable from the op log.",
                short(&sibling.commit_id, 8),
                title_or_none(&sibling.description),
                if siblings.len() == 1 {
                    format!("; {} stops being divergent", node.change_id)
                } else {
                    String::new()
                }
            );
            out.push(
                h_flex()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(t.border2)
                    .bg(t.bg_well)
                    .text_size(px(TEXT_S))
                    .text_color(t.text2)
                    .child(div().flex_1().min_w_0().child(rich(&text, cx)))
                    .child(
                        btn("copy-cancel", "Cancel")
                            .disabled(acting)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.header.diverge_confirm = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        btn(
                            "copy-confirm",
                            if acting {
                                "Abandoning…"
                            } else {
                                "Abandon copy"
                            },
                        )
                        .danger_solid()
                        .disabled(acting)
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.abandon_copy(id.clone(), window, cx),
                        )),
                    )
                    .into_any_element(),
            );
        }
        out
    }

    fn render_compare_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let from = resolve_compare_from(snapshot, &node.id, &self.compare);
        let trunk_from = resolve_compare_from(snapshot, &node.id, &CompareMode::Trunk);
        let base_from = resolve_compare_from(snapshot, &node.id, &CompareMode::Base);
        let preset = |id: &'static str,
                      glyph: &'static str,
                      glyph_color: Hsla,
                      name: &'static str,
                      note: String,
                      selected: bool| {
            let mut row = h_flex()
                .id(id)
                .w_full()
                .h(px(26.))
                .gap(px(8.))
                .px(px(8.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_S))
                .cursor_pointer()
                .child(
                    div()
                        .w(px(12.))
                        .flex_shrink_0()
                        .font_family(t.mono.clone())
                        .text_color(glyph_color)
                        .child(glyph),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.text1)
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(t.text3)
                        .child(truncated(note)),
                );
            row = if selected {
                row.bg(t.accent_dim)
            } else {
                row.hover(|style| style.bg(t.bg_hover))
            };
            row
        };
        let pick = |mode: CompareMode| {
            move |this: &mut Self,
                  _: &gpui::ClickEvent,
                  window: &mut Window,
                  cx: &mut Context<Self>| {
                this.set_compare(mode.clone(), cx);
                this.close_tray(window, cx);
            }
        };
        let mut presets = v_flex()
            .mt(px(8.))
            .p(px(2.))
            .rounded(px(RADIUS_M))
            .border_1()
            .border_color(t.border2)
            .bg(t.bg1)
            .child(
                preset(
                    "compare-parent",
                    "·",
                    t.text3,
                    "Parent",
                    "what this change itself did".into(),
                    from.is_none(),
                )
                .on_click(cx.listener(pick(CompareMode::Parent))),
            );
        if let Some(trunk) = &trunk_from {
            presets = presets.child(
                preset(
                    "compare-trunk",
                    "◆",
                    t.text3,
                    "Trunk",
                    format!("everything between {} and here", short(trunk, 4)),
                    !matches!(self.compare, CompareMode::Change(_)) && from.as_ref() == Some(trunk),
                )
                .child(
                    ref_label(None, snapshot.trunk_bookmark.clone(), cx)
                        .bg(t.accent_dim)
                        .text_color(t.accent_strong),
                )
                .on_click(cx.listener(pick(CompareMode::Trunk))),
            );
        }
        if let Some(base) = base_from
            .as_ref()
            .filter(|base| Some(*base) != trunk_from.as_ref())
        {
            let kind = find_node(snapshot, base).map_or(NodeKind::Mutable, |n| n.kind);
            presets = presets.child(
                preset(
                    "compare-base",
                    kind_glyph(kind),
                    t.text2,
                    "Stack base",
                    format!(
                        "everything this stack changes up to here, from {}",
                        short(base, 4)
                    ),
                    self.compare == CompareMode::Base && from.as_ref() == Some(base),
                )
                .on_click(cx.listener(pick(CompareMode::Base))),
            );
        }
        let filter = self.dest_filter(window, cx);
        let list = self.dest_list("No matching change", cx);
        let close = self.cancel_button("Close", cx);
        self.tray(false, cx)
            .child(self.tray_title(
                &format!("Compare **{}**", short(&node.id, 4)),
                "pick what the diff is measured against — read-only, nothing is rewritten".into(),
                cx,
            ))
            .child(presets)
            .child(self.label("Or against any change", cx))
            .children(filter.map(|filter| filter.mb(px(4.))))
            .child(list)
            .child(self.confirm_row(
                "⌘-click a graph row also compares against it · presets follow the selection; a picked change ends when the selection moves".into(),
                vec![close],
                cx,
            ))
            .into_any_element()
    }

    fn working_copy_line(
        &self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        own: &'static str,
    ) -> Option<String> {
        if node.id == snapshot.working_copy {
            Some(own.to_owned())
        } else if descendants_of(snapshot, &node.id)
            .iter()
            .any(|d| d.id == snapshot.working_copy)
        {
            Some("The working copy follows the rebase.".to_owned())
        } else {
            None
        }
    }

    fn render_confirm_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        confirm: Confirm,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = theme(cx);
        let acting = self.header.acting;
        let marks = bookmarks_at(snapshot, &node.id);
        let names = marks
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let descendants = descendants_of(snapshot, &node.id).len();
        let mut items: Vec<(String, bool)> = Vec::new();
        let rebase_line = |onto: &str| {
            format!(
                "{descendants} descendant change{} onto {onto}.",
                if descendants == 1 {
                    " rebases"
                } else {
                    "s rebase"
                }
            )
        };
        let (title, go, danger) = match confirm {
            Confirm::Squash => {
                let parent = match node.parents.as_slice() {
                    [only] => find_node(snapshot, only)?,
                    _ => return None,
                };
                items.push((
                    "Everything this change touches moves into the parent; the change itself is abandoned."
                        .into(),
                    false,
                ));
                if !marks.is_empty() {
                    items.push((
                        format!(
                            "Bookmark{} {names} move{} to the parent.",
                            if marks.len() == 1 { "" } else { "s" },
                            if marks.len() == 1 { "s" } else { "" }
                        ),
                        false,
                    ));
                }
                if descendants > 0 {
                    items.push((rebase_line("the parent"), false));
                }
                items.extend(
                    self.working_copy_line(
                        snapshot,
                        node,
                        "The working copy restarts as a new empty change on the parent.",
                    )
                    .map(|line| (line, false)),
                );
                (
                    self.tray_title(
                        &format!(
                            "Squash **{}** into its parent **{}**",
                            short(&node.id, 4),
                            short(&parent.id, 4)
                        ),
                        format!("\u{201c}{}\u{201d}", title_or_none(&parent.description)),
                        cx,
                    ),
                    btn("confirm-go", if acting { "Squashing…" } else { "Squash" }).primary(),
                    false,
                )
            }
            Confirm::Abandon => {
                items.push((
                    "Its changes disappear from the graph — the operation log can still restore them."
                        .into(),
                    false,
                ));
                if !marks.is_empty() {
                    items.push((
                        format!(
                            "Bookmark{} {names} will be deleted.",
                            if marks.len() == 1 { "" } else { "s" }
                        ),
                        false,
                    ));
                }
                if descendants > 0 {
                    items.push((rebase_line("its parent"), false));
                }
                items.extend(
                    self.working_copy_line(
                        snapshot,
                        node,
                        "The working copy restarts as a new empty change on its parent.",
                    )
                    .map(|line| (line, false)),
                );
                let siblings = divergent_siblings(snapshot, node);
                match siblings.as_slice() {
                    [] => {}
                    [only] => items.push((
                        format!(
                            "Only commit **{}** remains for **{}** — the divergence resolves.",
                            short(&only.commit_id, 8),
                            node.change_id
                        ),
                        false,
                    )),
                    many => items.push((
                        format!(
                            "{} other copies of **{}** stay divergent.",
                            many.len(),
                            node.change_id
                        ),
                        false,
                    )),
                }
                (
                    self.tray_title(
                        &format!("Abandon **{}**", short(&node.id, 4)),
                        format!("\u{201c}{}\u{201d}", title_or_none(&node.description)),
                        cx,
                    ),
                    btn(
                        "confirm-go",
                        if acting {
                            "Abandoning…"
                        } else {
                            "Abandon change"
                        },
                    )
                    .danger_solid(),
                    true,
                )
            }
        };
        let mut tray = self
            .tray(danger, cx)
            .child(title)
            .child(self.consequences(items, cx));
        if confirm == Confirm::Squash {
            let parent = find_node(snapshot, &node.parents[0])?;
            let combined = combined_description(&parent.description, &node.description);
            let empty = combined.is_empty();
            tray = tray.child(self.label("Resulting description", cx)).child(
                div()
                    .id("squash-description")
                    .px(px(12.))
                    .py(px(8.))
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .rounded(px(RADIUS_M))
                    .border_1()
                    .border_color(t.border2)
                    .bg(t.bg1)
                    .text_size(px(TEXT_S))
                    .line_height(gpui::relative(1.5))
                    .text_color(if empty { t.text3 } else { t.text2 })
                    .when(empty, |el| el.italic())
                    .child(if empty {
                        "(no description — describe it afterwards)".to_owned()
                    } else {
                        combined
                    }),
            );
        }
        let buttons = vec![
            self.cancel_button("Cancel", cx),
            go.disabled(acting)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.run_confirmed(confirm, window, cx)),
                )
                .into_any_element(),
        ];
        Some(
            tray.child(self.confirm_row("⌘↵ to confirm".into(), buttons, cx))
                .into_any_element(),
        )
    }

    fn render_rebase_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let acting = self.header.acting;
        let alone = self.header.rebase_alone;
        let descendants = descendants_of(snapshot, &node.id);
        let count = descendants.len();
        let wc_above = descendants.iter().any(|d| d.id == snapshot.working_copy);
        let dest = self
            .header
            .dest
            .as_deref()
            .and_then(|id| find_node(snapshot, id));
        let mut controls = h_flex().mt(px(8.)).gap(px(8.)).flex_wrap();
        if count > 0 {
            let this = cx.entity().downgrade();
            let id = node.id.clone();
            controls = controls.child(segmented(
                "rebase-mode",
                vec![
                    (
                        false,
                        format!("With {}", plural(count, "descendant", "descendants")).into(),
                        None,
                    ),
                    (true, "Only this change".into(), None),
                ],
                &alone,
                cx,
                move |alone, _, cx| {
                    let alone = *alone;
                    let _ = this.update(cx, |this, cx| {
                        this.header.rebase_alone = alone;
                        // A destination the other mode excludes cannot stay
                        // picked.
                        let still = this.header.dest.as_ref().is_some_and(|dest| {
                            rebase_destinations(&this.snapshot, &id, !alone)
                                .iter()
                                .any(|candidate| &candidate.id == dest)
                        });
                        if !still {
                            this.header.dest = None;
                        }
                        cx.notify();
                    });
                },
            ));
        }
        controls = controls.children(self.dest_filter(window, cx));

        let mut items: Vec<(String, bool)> = Vec::new();
        if let Some(dest) = dest {
            let onto = format!(
                "**{}** ~~\u{201c}{}\u{201d}~~",
                short(&dest.id, 4),
                title_or_none(&dest.description)
            );
            if alone {
                items.push((format!("Only this change moves onto {onto}."), false));
                if count > 0 {
                    let parents = node
                        .parents
                        .iter()
                        .map(|p| short(p, 4))
                        .collect::<Vec<_>>()
                        .join(", ");
                    items.push((
                        format!(
                            "{count} descendant change{} behind, reparented onto **{parents}**.",
                            if count == 1 { " stays" } else { "s stay" }
                        ),
                        false,
                    ));
                }
            } else if count > 0 {
                items.push((
                    format!(
                        "This change and its {} move onto {onto}.",
                        plural(count, "descendant", "descendants")
                    ),
                    false,
                ));
            } else {
                items.push((format!("This change moves onto {onto}."), false));
            }
            if node.parents.len() > 1 {
                items.push((
                    format!(
                        "Its {} parents are replaced by the destination — the merge is dissolved.",
                        node.parents.len()
                    ),
                    false,
                ));
            }
            if node.id == snapshot.working_copy {
                items.push(("The working copy moves with it.".into(), false));
            } else if wc_above && !alone {
                items.push(("The working copy follows the rebase.".into(), false));
            } else if wc_above {
                items.push((
                    "The working copy stays behind with the reparented descendants.".into(),
                    false,
                ));
            }
            items.push((
                "Changes that no longer apply cleanly become conflicts instead of stopping the rebase; the operation log can undo it."
                    .into(),
                false,
            ));
        }
        let has_dest = dest.is_some();
        let list = self.dest_list("No matching destination", cx);
        let buttons = vec![
            self.cancel_button("Cancel", cx),
            btn(
                "confirm-go",
                if acting {
                    "Rebasing…"
                } else if alone {
                    "Move here"
                } else {
                    "Rebase here"
                },
            )
            .primary()
            .disabled(acting || !has_dest)
            .on_click(cx.listener(|this, _, window, cx| this.submit_rebase(window, cx)))
            .into_any_element(),
        ];
        let mut tray = self
            .tray(false, cx)
            .child(self.tray_title(
                &format!("Rebase **{}**", short(&node.id, 4)),
                format!("\u{201c}{}\u{201d}", title_or_none(&node.description)),
                cx,
            ))
            .child(controls)
            .child(self.label("Destination — the new parent", cx))
            .child(list);
        if !items.is_empty() {
            tray = tray.child(self.consequences(items, cx));
        }
        tray.child(self.confirm_row(
            if has_dest {
                "↵ to confirm".into()
            } else {
                "Pick the destination this change moves onto — ↑↓ or click".into()
            },
            buttons,
            cx,
        ))
        .into_any_element()
    }

    fn render_bookmark_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let acting = self.header.acting;
        let empty = self.header.bookmark_name.read(cx).text().trim().is_empty();
        let mut tray = self
            .tray(false, cx)
            .child(self.tray_title(
                &format!("Bookmark **{}**", short(&node.id, 4)),
                format!("\u{201c}{}\u{201d}", title_or_none(&node.description)),
                cx,
            ))
            .child(
                h_flex()
                    .mt(px(8.))
                    .gap(px(8.))
                    .child(
                        field(&self.header.bookmark_name, window, cx)
                            .w(px(280.))
                            .font_family(t.mono.clone()),
                    )
                    .child(
                        btn(
                            "bookmark-create",
                            if acting { "Working…" } else { "Create" },
                        )
                        .primary()
                        .disabled(acting || empty)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.submit_create_bookmark(window, cx)
                        })),
                    ),
            );
        let movable: Vec<&BookmarkState> = snapshot
            .bookmarks
            .iter()
            .filter(|b| b.is_local && b.target != node.id)
            .collect();
        if !movable.is_empty() {
            let mut list = v_flex()
                .max_h(px(160.))
                .id("move-bookmarks")
                .overflow_y_scroll();
            for (ix, mark) in movable.into_iter().enumerate() {
                let mut notes = Vec::new();
                match move_direction(snapshot, &mark.target, &node.id) {
                    Some(MoveDirection::Backwards) => notes.push("moves backwards".to_owned()),
                    Some(MoveDirection::Sideways) => notes.push("moves sideways".to_owned()),
                    _ => {}
                }
                if let Some(remote) = &mark.remote {
                    notes.push(format!("{remote} updates on push"));
                }
                let mut note = format!("from {}", short(&mark.target, 4));
                for extra in notes {
                    note.push_str(" · ");
                    note.push_str(&extra);
                }
                let name = mark.name.clone();
                let mut row = h_flex()
                    .id(("move-bookmark", ix))
                    .w_full()
                    .h(px(26.))
                    .gap(px(8.))
                    .px(px(8.))
                    .rounded(px(RADIUS_S))
                    .text_size(px(TEXT_S))
                    .text_color(t.text2)
                    .child(icon(IconName::Bookmark, 10., t.text3))
                    .child(
                        div()
                            .min_w_0()
                            .font_family(t.mono.clone())
                            .child(truncated(mark.name.clone())),
                    );
                if mark.is_trunk {
                    row = row.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(TEXT_XS))
                            .text_color(t.accent_strong)
                            .child("trunk"),
                    );
                }
                row = row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(TEXT_XS))
                        .text_color(t.text3)
                        .child(truncated(note)),
                );
                if acting {
                    row = row.opacity(0.6);
                } else {
                    row = row
                        .cursor_pointer()
                        .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let name = name.clone();
                            this.mutate_node(window, cx, move |backend, path, id| {
                                backend.move_bookmark(path, &name, id)
                            })
                        }));
                }
                list = list.child(row);
            }
            tray = tray
                .child(self.label("Or move an existing bookmark here", cx))
                .child(list);
        }
        let cancel = self.cancel_button("Cancel", cx);
        tray.child(self.confirm_row(
            "Bookmarks never rewrite changes; the operation log can restore them".into(),
            vec![cancel],
            cx,
        ))
        .into_any_element()
    }

    fn render_manage_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = theme(cx);
        let mark = bookmarks_at(snapshot, &node.id)
            .into_iter()
            .find(|mark| mark.name == name)?;
        let acting = self.header.acting;
        let draft = self.header.rename.read(cx).text().trim().to_owned();
        let context = match &mark.remote {
            Some(remote) => format!("tracked on {remote} — {}", sync_label(mark.sync).0),
            None => "local-only bookmark".to_owned(),
        };
        let hint = match &mark.remote {
            Some(remote) => format!(
                "Renaming starts a local-only bookmark; deleting removes it from {remote} on the next push"
            ),
            None => "Only exists locally; the operation log can restore it".to_owned(),
        };
        let delete_name = mark.name.clone();
        let buttons = vec![
            btn("bookmark-delete", "Delete")
                .danger()
                .icon(IconName::Trash)
                .disabled(acting)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let name = delete_name.clone();
                    this.mutate_node(window, cx, move |backend, path, _| {
                        backend.delete_bookmark(path, &name)
                    })
                }))
                .into_any_element(),
            self.cancel_button("Close", cx),
        ];
        Some(
            self.tray(false, cx)
                .child(self.tray_title(&format!("**{}**", mark.name), context, cx))
                .child(
                    h_flex()
                        .mt(px(8.))
                        .gap(px(8.))
                        .child(
                            field(&self.header.rename, window, cx)
                                .w(px(280.))
                                .font_family(t.mono.clone()),
                        )
                        .child(
                            btn(
                                "bookmark-rename",
                                if acting { "Working…" } else { "Rename" },
                            )
                            .primary()
                            .disabled(acting || draft.is_empty() || draft == mark.name)
                            .on_click(cx.listener(
                                |this, _, window, cx| this.submit_rename_bookmark(window, cx),
                            )),
                        ),
                )
                .child(self.confirm_row(hint, buttons, cx))
                .into_any_element(),
        )
    }

    fn render_split_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let acting = self.header.acting;
        let into = self.header.split_into;
        let title = self.tray_title(
            &format!("Split **{}**", short(&node.id, 4)),
            format!("\u{201c}{}\u{201d}", title_or_none(&node.description)),
            cx,
        );
        let files: Option<Vec<FileDiff>> = self.header_files().map(<[FileDiff]>::to_vec);
        let quiet = |text: &'static str| {
            div()
                .mt(px(8.))
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child(text)
        };
        let Some(files) = files else {
            let cancel = self.cancel_button("Cancel", cx);
            return self
                .tray(false, cx)
                .child(title)
                .child(quiet("Loading the file list…"))
                .child(self.confirm_row(String::new(), vec![cancel], cx))
                .into_any_element();
        };
        if !movable(&files) {
            let cancel = self.cancel_button("Cancel", cx);
            return self
                .tray(false, cx)
                .child(title)
                .child(quiet(
                    "This change touches no files — there is nothing to split or move.",
                ))
                .child(self.confirm_row(String::new(), vec![cancel], cx))
                .into_any_element();
        }
        let summary = split_summary(Some(&files), &self.header.split);
        let ready = self.split_ready();
        let marks = bookmarks_at(snapshot, &node.id);
        let names = marks
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let descendants = descendants_of(snapshot, &node.id);
        let wc_above = descendants.iter().any(|d| d.id == snapshot.working_copy);
        let node_title = title_or_none(&node.description).to_owned();

        // "the 2 checked files", "checked 1 hunk across 1 file", …
        let kept = {
            let n = |count: usize, word: &str| plural(count, word, &format!("{word}s"));
            if summary.partial == 0 {
                n(summary.whole, "checked file")
            } else {
                let mut sub = Vec::new();
                if summary.hunks > 0 {
                    sub.push(n(summary.hunks, "hunk"));
                }
                if summary.lines > 0 {
                    sub.push(n(summary.lines, "line"));
                }
                let sub = sub.join(" and ");
                if summary.whole == 0 {
                    format!("checked {sub} across {}", n(summary.partial, "file"))
                } else {
                    format!(
                        "{} plus {sub} from {} more",
                        n(summary.whole, "checked file"),
                        summary.partial
                    )
                }
            }
        };
        let kept_singular = if summary.partial == 0 {
            summary.whole == 1
        } else {
            summary.whole == 0 && summary.hunks + summary.lines == 1
        };
        let unchecked_note = if summary.partial > 0 {
            " — including the other hunks of partially checked files —"
        } else {
            ""
        };

        let this = cx.entity().downgrade();
        let mut controls = h_flex().mt(px(8.)).gap(px(8.)).flex_wrap().child(segmented(
            "split-mode",
            vec![
                (false, "Into a new change".into(), None),
                (true, "Into an existing change".into(), None),
            ],
            &into,
            cx,
            move |into, window, cx| {
                let into = *into;
                let _ = this.update(cx, |this, cx| {
                    this.header.split_into = into;
                    this.header.error = None;
                    this.header.dest = None;
                    if into {
                        this.focus_filter_or_tray(window, cx);
                    }
                    cx.notify();
                });
            },
        ));
        if into {
            controls = controls.children(self.dest_filter(window, cx));
        }
        let mut tray = self.tray(false, cx).child(title).child(controls);

        if !into && !splittable(&files) {
            tray = tray.child(quiet(
                "This change touches only one file, and its diff offers nothing to carve apart — nothing to split. “Into an existing change” can still move it somewhere else.",
            ));
        } else {
            tray = tray
                .child(self.label(
                    if into {
                        "Check the files — or single hunks — to move into the destination"
                    } else {
                        "Check the files — or single hunks — to carve off into their own change"
                    },
                    cx,
                ))
                .child(self.split_file_list(&files, cx));
            if !into {
                let focused = self.header.split_desc.read(cx).is_focused(window);
                tray = tray
                    .child(self.label("Description for the split-off change", cx))
                    .child(
                        div()
                            .flex()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(RADIUS_M))
                            .border_1()
                            .border_color(if focused { t.accent } else { t.border1 })
                            .bg(t.bg1)
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_S))
                            .line_height(px(18.))
                            .child(self.header.split_desc.clone()),
                    );
                if summary.whole + summary.partial > 0 {
                    let mut items = vec![(
                        format!(
                            "The {kept} become{} the first change — it keeps the id **{}** and the description above.",
                            if kept_singular { "s" } else { "" },
                            short(&node.id, 4)
                        ),
                        false,
                    )];
                    if summary.all_covered {
                        items.push((
                            "Every file is fully checked — leave a file or a hunk for the new change on top."
                                .into(),
                            true,
                        ));
                    } else {
                        items.push((
                            format!(
                                "Everything unchecked{unchecked_note} moves to a new change directly on top, keeping ~~\u{201c}{node_title}\u{201d}~~."
                            ),
                            false,
                        ));
                    }
                    if !marks.is_empty() {
                        items.push((
                            format!(
                                "Bookmark{} {names} follow{} the new top change.",
                                if marks.len() == 1 { "" } else { "s" },
                                if marks.len() == 1 { "s" } else { "" }
                            ),
                            false,
                        ));
                    }
                    if node.id == snapshot.working_copy {
                        items.push((
                            "The working copy follows the new top change — nothing moves on disk."
                                .into(),
                            false,
                        ));
                    } else if wc_above {
                        items.push(("The working copy follows the rebase.".into(), false));
                    }
                    if !descendants.is_empty() {
                        items.push((
                            format!(
                                "{} descendant change{} onto the new top change.",
                                descendants.len(),
                                if descendants.len() == 1 {
                                    " rebases"
                                } else {
                                    "s rebase"
                                }
                            ),
                            false,
                        ));
                    }
                    tray = tray.child(self.consequences(items, cx));
                }
            } else {
                let list = self.dest_list("No matching destination", cx);
                tray = tray
                    .child(self.label("Destination — the change the selection moves into", cx))
                    .child(list);
                let dest = self
                    .header
                    .dest
                    .as_deref()
                    .and_then(|id| find_node(snapshot, id));
                if let (Some(dest), true) = (dest, summary.whole + summary.partial > 0) {
                    let direction = move_direction(snapshot, &node.id, &dest.id);
                    let where_ = match direction {
                        Some(MoveDirection::Backwards) => " — an earlier change beneath this one",
                        Some(MoveDirection::Forward) => " — a later change building on this one",
                        Some(MoveDirection::Sideways) => " — a change on another branch",
                        None => "",
                    };
                    let mut items = vec![(
                        format!(
                            "The {kept} move{} into **{}** ~~\u{201c}{}\u{201d}~~{where_}.",
                            if kept_singular { "s" } else { "" },
                            short(&dest.id, 4),
                            title_or_none(&dest.description)
                        ),
                        false,
                    )];
                    if summary.all_covered {
                        let bookmarks = if marks.is_empty() {
                            String::new()
                        } else {
                            format!(
                                ", and bookmark{} {names} move{} to its parent",
                                if marks.len() == 1 { "" } else { "s" },
                                if marks.len() == 1 { "s" } else { "" }
                            )
                        };
                        items.push((
                            format!(
                                "That is everything in **{}** — the emptied change is abandoned{bookmarks}.",
                                short(&node.id, 4)
                            ),
                            false,
                        ));
                        if node.id == snapshot.working_copy {
                            items.push((
                                "The working copy respawns as a new empty change on its parent."
                                    .into(),
                                false,
                            ));
                        }
                    } else {
                        items.push((
                            format!(
                                "Everything unchecked{unchecked_note} stays here as ~~\u{201c}{node_title}\u{201d}~~."
                            ),
                            false,
                        ));
                        if node.id == snapshot.working_copy {
                            items.push((
                                if direction == Some(MoveDirection::Backwards) {
                                    "The working copy keeps building on the moved changes — nothing moves on disk."
                                } else {
                                    "The moved changes leave the working copy's files on disk."
                                }
                                .into(),
                                false,
                            ));
                        }
                    }
                    if dest.id == snapshot.working_copy {
                        items.push((
                            "The working copy takes the changes; its files update on disk.".into(),
                            false,
                        ));
                    } else if node.id != snapshot.working_copy && wc_above {
                        items.push(("The working copy follows the rebase.".into(), false));
                    }
                    items.push((
                        "Changes that no longer apply cleanly become conflicts instead of blocking the move; the operation log can undo it."
                            .into(),
                        false,
                    ));
                    tray = tray.child(self.consequences(items, cx));
                }
            }
        }
        let hint = if into {
            if ready {
                "↵ to confirm"
            } else if summary.whole + summary.partial == 0 {
                "Check the files or hunks to move"
            } else {
                "Pick the change they move into — ↑↓ or click"
            }
        } else if ready {
            "⌘↵ to confirm"
        } else {
            "Check files or hunks for the first change — leaving something on each side"
        };
        let buttons = vec![
            self.cancel_button("Cancel", cx),
            btn(
                "confirm-go",
                match (acting, into) {
                    (true, true) => "Moving…",
                    (true, false) => "Splitting…",
                    (false, true) => "Move here",
                    (false, false) => "Split",
                },
            )
            .primary()
            .disabled(acting || !ready)
            .on_click(cx.listener(|this, _, window, cx| this.submit_split(window, cx)))
            .into_any_element(),
        ];
        tray.child(self.confirm_row(hint.into(), buttons, cx))
            .into_any_element()
    }

    /// The split panel's checklist: files, their hunks on demand, a
    /// hunk's changed lines on demand.
    fn split_file_list(&mut self, files: &[FileDiff], cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let acting = self.header.acting;
        let check = |state: Option<bool>| {
            // Checked, partly checked, or empty.
            let mut cell = div()
                .w(px(14.))
                .h(px(14.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .border_1();
            cell = match state {
                Some(false) => cell.border_color(t.border1).bg(t.bg_well),
                Some(true) => cell.border_color(t.accent).bg(t.accent).child(icon(
                    IconName::Check,
                    10.,
                    t.accent_contrast,
                )),
                None => cell.border_color(t.accent).bg(t.accent).child(icon(
                    IconName::Minus,
                    10.,
                    t.accent_contrast,
                )),
            };
            cell
        };
        let row = |id: gpui::ElementId, indent: f32, on: bool| {
            let mut row = h_flex()
                .id(id)
                .flex_1()
                .min_w_0()
                .h(px(24.))
                .gap(px(8.))
                .pl(px(8. + indent))
                .pr(px(8.))
                .rounded(px(RADIUS_S))
                .text_size(px(TEXT_S));
            if !acting {
                row = row.cursor_pointer();
            }
            if on {
                row.bg(t.accent_dim)
            } else {
                row.hover(|style| style.bg(t.bg_hover))
            }
        };
        let expander = |id: gpui::ElementId, label: String, open: bool| {
            h_flex()
                .id(id)
                .flex_shrink_0()
                .gap(px(4.))
                .h(px(20.))
                .px(px(6.))
                .rounded(px(RADIUS_S))
                .font_family(t.mono.clone())
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
                .child(label)
                .child(icon(
                    if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    10.,
                    t.text3,
                ))
        };
        let stat = |sign: &str, n: u32, color: Hsla| {
            (n > 0).then(|| div().text_color(color).child(format!("{sign}{n}")))
        };
        let mut list = v_flex();
        let mut key = 0usize;
        for file in files {
            let pick = self.header.split.get(&file.path);
            let checked = pick == Some(&SplitPick::All);
            let partial = matches!(pick, Some(SplitPick::Hunks(_)));
            let hunky = can_select_hunks(file);
            let hunks_open = hunky && self.header.split_hunks_open.contains(&file.path);
            let (dir, name) = split_path(&file.path);
            let (glyph, hue) = match file.status {
                jiji_core::snapshot::FileStatus::Added => ("A", t.add_fg),
                jiji_core::snapshot::FileStatus::Modified => ("M", t.warn),
                jiji_core::snapshot::FileStatus::Removed => ("D", t.del_fg),
                _ => ("R", t.accent_strong),
            };
            let path = file.path.clone();
            key += 1;
            let mut line = h_flex().gap(px(4.)).child(
                row(("split-file", key).into(), 0., checked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.header.acting {
                            split::toggle_file(&mut this.header.split, &path);
                            cx.notify();
                        }
                    }))
                    .child(check(if checked {
                        Some(true)
                    } else if partial {
                        None
                    } else {
                        Some(false)
                    }))
                    .child(
                        div()
                            .w(px(12.))
                            .flex_shrink_0()
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hue)
                            .child(glyph),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .font_family(t.mono.clone())
                            .child(
                                div()
                                    .min_w_0()
                                    .text_color(t.text3)
                                    .child(truncated(dir.to_owned())),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(t.text1)
                                    .child(name.to_owned()),
                            ),
                    ),
            );
            if hunky {
                let path = file.path.clone();
                line = line.child(
                    expander(
                        ("split-expand", key).into(),
                        format!("{} hunks", file_hunks(file).len()),
                        hunks_open,
                    )
                    .tooltip(tooltip(if hunks_open {
                        "Hide this file's hunks"
                    } else {
                        "Pick single hunks of this file"
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.header.split_hunks_open.remove(&path) {
                            this.header.split_hunks_open.insert(path.clone());
                        }
                        cx.notify();
                    })),
                );
            }
            list = list.child(line);
            if !hunks_open {
                continue;
            }
            let hunks = file_hunks(file);
            for (index, hunk) in hunks.iter().enumerate() {
                let hunk_pick = match pick {
                    Some(SplitPick::Hunks(map)) => map.get(&index),
                    _ => None,
                };
                let on = checked || hunk_pick == Some(&HunkPick::All);
                let half = matches!(hunk_pick, Some(HunkPick::Lines(_)));
                let (added, removed) = hunk_stats(hunk);
                let line_key = format!("{}@{index}", file.path);
                let liney = can_select_lines(hunk);
                let lines_open = liney && self.header.split_lines_open.contains(&line_key);
                let (path, count) = (file.path.clone(), hunks.len());
                key += 1;
                let mut line = h_flex().gap(px(4.)).child(
                    row(("split-hunk", key).into(), 22., on)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.header.acting {
                                split::toggle_hunk(&mut this.header.split, &path, index, count);
                                cx.notify();
                            }
                        }))
                        .child(check(if on {
                            Some(true)
                        } else if half {
                            None
                        } else {
                            Some(false)
                        }))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .text_color(t.text2)
                                .child(hunk_label(hunk)),
                        )
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .gap(px(6.))
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .children(stat("+", added, t.add_fg))
                                .children(stat("−", removed, t.del_fg)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .text_color(t.text3)
                                .child(truncated(hunk_preview(hunk))),
                        ),
                );
                if liney {
                    let toggle_key = line_key.clone();
                    line = line.child(
                        expander(
                            ("split-expand-lines", key).into(),
                            format!("{} lines", changed_line_indices(hunk).len()),
                            lines_open,
                        )
                        .tooltip(tooltip(if lines_open {
                            "Hide this hunk's lines"
                        } else {
                            "Pick single lines of this hunk"
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.header.split_lines_open.remove(&toggle_key) {
                                this.header.split_lines_open.insert(toggle_key.clone());
                            }
                            cx.notify();
                        })),
                    );
                }
                list = list.child(line);
                if !lines_open {
                    continue;
                }
                for line_index in changed_line_indices(hunk) {
                    let source = &hunk.lines[line_index as usize];
                    let line_on = on
                        || matches!(hunk_pick, Some(HunkPick::Lines(set)) if set.contains(&line_index));
                    let added = source.kind == jiji_core::snapshot::DiffLineKind::Added;
                    let target = file.clone();
                    key += 1;
                    list = list.child(
                        h_flex().child(
                            row(("split-line", key).into(), 44., line_on)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.header.acting {
                                        split::toggle_line(
                                            &mut this.header.split,
                                            &target,
                                            index,
                                            line_index,
                                        );
                                        cx.notify();
                                    }
                                }))
                                .child(check(Some(line_on)))
                                .child(
                                    div()
                                        .w(px(10.))
                                        .flex_shrink_0()
                                        .font_family(t.mono.clone())
                                        .text_color(if added { t.add_fg } else { t.del_fg })
                                        .child(if added { "+" } else { "−" }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .font_family(t.mono.clone())
                                        .text_size(px(TEXT_XS))
                                        .text_color(t.text2)
                                        .child(truncated(line_text(source))),
                                ),
                        ),
                    );
                }
            }
        }
        div()
            .relative()
            .rounded(px(RADIUS_M))
            .border_1()
            .border_color(t.border2)
            .bg(t.bg1)
            .child(
                div()
                    .id("split-files")
                    .max_h(px(220.))
                    .p(px(2.))
                    .overflow_y_scroll()
                    .track_scroll(&self.header.split_scroll)
                    .child(list),
            )
            .child(scrollbar(
                "split-files-scrollbar",
                &self.header.split_scroll,
            ))
            .into_any_element()
    }
}
