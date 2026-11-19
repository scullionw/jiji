//! The change header: what the change is and what you can do with it,
//! then where it sits and how its diff is shown — so the diff keeps the
//! pane. Panels (describe, rebase, split, …) open below as trays that push
//! the diff down rather than float over it.

mod bars;
mod panel;
mod split_tray;
mod trays;

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Entity, FocusHandle, ScrollHandle, Task,
    UniformListScrollHandle, Window,
};
use jiji_core::snapshot::{FileDiff, GraphNode, MutationOutcome, NodeKind};

use super::WorkbenchView;
use crate::app::UiIntent;
use crate::host::HostResult;
use crate::logic::inspect::{
    action_availability, descendants_of, find_node, rebase_destinations, squash_destinations,
    summary_of, CompareMode,
};
use crate::logic::split::{self, selection_ready, split_payload, split_summary, Selection};
use crate::theme::{theme, Theme, TEXT_M, TEXT_S};
use crate::ui::ScrollY as _;
use crate::ui::{v_flex, InputEvent, PopoverState, TextInput};

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
            window.focus(&self.focus, cx);
        }
    }

    fn header_node(&self) -> Option<&GraphNode> {
        find_node(&self.snapshot, self.header.node.as_deref()?)
    }

    /// The files of the diff on screen, when it is this change's own.
    fn header_files(&self) -> Option<&[FileDiff]> {
        self.diff.current().map(|diff| diff.files.as_slice())
    }

    /// File what an open describe editor holds as the change's draft. Every
    /// way the editor goes away without saving comes through here,
    /// including the view itself being dropped (see `WorkbenchView::new`).
    pub(super) fn keep_open_draft(&mut self, cx: &mut App) {
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
        window.focus(&self.focus, cx);
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
                input.focus(window, cx);
            }),
            Tray::Manage(name) => {
                let name = name.clone();
                self.header.rename.update(cx, |input, cx| {
                    input.set_text(name, cx);
                    input.focus(window, cx);
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
                window.focus(&self.header.tray_focus, cx);
            }
            _ => window.focus(&self.header.tray_focus, cx),
        }
        cx.notify();
    }

    /// The filter shows only for long lists; without it the tray itself
    /// takes the keys.
    fn focus_filter_or_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dest_candidates().len() > 6 {
            self.header
                .filter
                .update(cx, |input, cx| input.focus(window, cx));
        } else {
            window.focus(&self.header.tray_focus, cx);
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
            input.focus(window, cx);
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
            input.focus(window, cx);
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
                            window.focus(&this.focus, cx);
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
            .child(self.render_title_bar(node, &avail, window, cx))
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
                    .scroll_y()
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
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use crate::app::{AppModel, Section, UiIntent};
    use crate::shell::Shell;

    const DRAFT: &str = "Typed, then left unsaved";

    /// The app on a planted repo, with the working copy's describe editor
    /// open and `DRAFT` typed over whatever it held.
    fn editing(
        name: &str,
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<AppModel>,
        gpui::WindowHandle<Shell>,
        std::path::PathBuf,
    ) {
        let dir = crate::testing::scratch(name);
        let model = crate::testing::install(&dir, cx);
        let snapshot = jiji_core::mock::mock_snapshot(&dir.join("repo"));
        model.update(cx, |model, cx| model.plant_repo(snapshot, cx));
        let window = cx.add_window(Shell::new);
        open_editor(&model, cx);
        cx.simulate_keystrokes(window.into(), "cmd-a");
        cx.simulate_input(window.into(), DRAFT);
        assert_eq!(editor_text(window, cx).as_deref(), Some(DRAFT));
        (model, window, dir)
    }

    /// Open the describe editor the way the command palette does.
    fn open_editor(model: &gpui::Entity<AppModel>, cx: &mut TestAppContext) {
        model.update(cx, |model, cx| model.send_intent(UiIntent::Describe, cx));
        cx.run_until_parked();
    }

    /// What the describe editor holds, while it is open.
    fn editor_text(window: gpui::WindowHandle<Shell>, cx: &mut TestAppContext) -> Option<String> {
        window
            .update(cx, |shell, _, cx| {
                let workbench = shell.section::<crate::views::workbench::WorkbenchView>()?;
                let header = &workbench.read(cx).header;
                (header.tray == super::Tray::Edit).then(|| header.editor.read(cx).text().to_owned())
            })
            .unwrap()
    }

    #[gpui::test]
    fn an_unsaved_description_survives_a_visit_to_another_section(cx: &mut TestAppContext) {
        let (model, window, dir) = editing("draft-section", cx);

        // ⌘5 replaces the workbench with Operations: the editor goes with
        // the view it lived in.
        cx.simulate_keystrokes(window.into(), "cmd-5");
        model.read_with(cx, |model, _| {
            assert_eq!(model.section(), Section::Operations)
        });
        cx.simulate_keystrokes(window.into(), "cmd-1");
        assert_eq!(editor_text(window, cx), None);

        open_editor(&model, cx);
        assert_eq!(editor_text(window, cx).as_deref(), Some(DRAFT));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[gpui::test]
    fn an_unsaved_description_survives_a_visit_to_another_tab(cx: &mut TestAppContext) {
        let (model, window, dir) = editing("draft-tab", cx);

        cx.simulate_keystrokes(window.into(), "cmd-t");
        model.read_with(cx, |model, _| assert_eq!(model.tabs.len(), 2));
        cx.simulate_keystrokes(window.into(), "ctrl-tab");
        assert_eq!(editor_text(window, cx), None);

        open_editor(&model, cx);
        assert_eq!(editor_text(window, cx).as_deref(), Some(DRAFT));
        let _ = std::fs::remove_dir_all(dir);
    }
}
