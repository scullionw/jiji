//! The context menu's state: how each surface asks for one, and what its
//! items do. Right-clicks resolve innermost-first (a chip inside a row
//! answers before the row, the row before the bare surface), and every
//! surface funnels through here so the same target always offers the same
//! commands.

use std::collections::HashSet;
use std::path::Path;

use gpui::{ClipboardItem, Context, Pixels, Point};
use jiji_core::snapshot::{FileDiff, NodeKind};

use super::AppModel;
use crate::logic::conflicts::can_resolve;
use crate::logic::inspect::{find_node, resolve_compare_from, CompareMode};
use crate::logic::menu::{
    build_menu, review_bookmark, MenuAction, MenuContext, MenuSection, MenuTarget,
};
use crate::logic::pr::pr_for_bookmark;
use crate::logic::stacks::{publishable_stacks, shippable_stacks};

/// The menu on screen. `seq` tells one opening from the next, so the layer
/// rebuilds when a second right-click replaces the first.
pub struct OpenMenu {
    pub seq: u64,
    pub position: Point<Pixels>,
    pub sections: Vec<MenuSection>,
}

impl AppModel {
    /// Whether the breadcrumb's undo target is still around.
    pub fn can_undo(&self) -> bool {
        self.ui()
            .last_mutation
            .as_ref()
            .is_some_and(|crumb| crumb.outcome.operation_id.is_some())
    }

    /// Build and show the menu for `target`. Nothing opens when the target
    /// offers nothing.
    fn open_menu(
        &mut self,
        position: Point<Pixels>,
        selection: &str,
        target: MenuTarget,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot().cloned();
        let editors = self.editor_choices();
        let sections = build_menu(&MenuContext {
            snapshot: snapshot.as_deref(),
            selection,
            target,
            editors: &editors,
            can_undo: self.can_undo(),
        });
        if sections.iter().all(Vec::is_empty) {
            return;
        }
        self.menu_seq += 1;
        self.menu = Some(OpenMenu {
            seq: self.menu_seq,
            position,
            sections,
        });
        cx.notify();
    }

    /// Dismiss the menu; false when none was open.
    pub fn close_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let was_open = self.menu.take().is_some();
        if was_open {
            cx.notify();
        }
        was_open
    }

    /// The stacks Publish can land, as (head bookmark, title). Landing
    /// needs a PR behind a bookmark and a verified forge connection (the
    /// plan the routes open asks GitHub), so this is empty until then.
    pub fn landable_stacks(&self) -> Vec<(String, String)> {
        let Some(snapshot) = self.snapshot() else {
            return Vec::new();
        };
        if !self.forge.connection_ready() {
            return Vec::new();
        }
        let branches: HashSet<&str> = self
            .forge
            .prs
            .iter()
            .flat_map(|prs| prs.by_branch.keys().map(String::as_str))
            .collect();
        publishable_stacks(snapshot, &branches)
            .into_iter()
            .filter_map(|stack| Some((stack.head_bookmark()?.to_owned(), stack.title)))
            .collect()
    }

    /// The stacks that can ship straight to trunk, as (head change, head
    /// title, change count). Shipping needs only a detected GitHub remote:
    /// the push authenticates like any git push.
    pub fn shippable_stacks(&self) -> Vec<(String, String, usize)> {
        let has_repo = self
            .forge
            .status
            .as_ref()
            .is_some_and(|status| status.repo.is_some());
        match self.snapshot() {
            Some(snapshot) if has_repo => shippable_stacks(snapshot)
                .into_iter()
                .map(|stack| (stack.head_change, stack.head_title, stack.change_count))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A graph row, a focus card, the change header. The click selects the
    /// change first — intents act on the selection.
    pub fn open_change_menu(&mut self, id: &str, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        let Some(node) = find_node(&snapshot, id) else {
            return;
        };
        self.jump_to_change(id, cx);
        let land_bookmark = self
            .landable_stacks()
            .into_iter()
            .map(|(bookmark, _)| bookmark)
            .find(|bookmark| node.bookmarks.iter().any(|name| name == bookmark));
        let shippable = self
            .shippable_stacks()
            .iter()
            .any(|(head, ..)| *head == node.id);
        let target = MenuTarget::Change {
            node,
            compare_trunk: resolve_compare_from(&snapshot, id, &CompareMode::Trunk).is_some(),
            compare_base: resolve_compare_from(&snapshot, id, &CompareMode::Base).is_some(),
            land_bookmark,
            shippable,
        };
        self.open_menu(position, "", target, cx);
    }

    /// A bookmark chip or a Bookmarks-view row.
    pub fn open_bookmark_menu(
        &mut self,
        name: &str,
        remote: Option<&str>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        let Some(bookmark) = snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == name && b.remote.as_deref() == remote)
        else {
            return;
        };
        let prs = self.forge.prs.clone();
        let target = MenuTarget::Bookmark {
            bookmark,
            pr: pr_for_bookmark(bookmark, prs.as_deref()),
            has_remotes: !snapshot.git_remotes.is_empty(),
        };
        self.open_menu(position, "", target, cx);
    }

    /// A PR badge anywhere: graph chips, the header chip, Publish rows.
    pub fn open_pr_menu(&mut self, number: u64, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        let Some(prs) = self.forge.prs.clone() else {
            return;
        };
        let Some(pr) = prs
            .report
            .prs
            .iter()
            .chain(prs.by_branch.values())
            .find(|pr| pr.number == number)
        else {
            return;
        };
        let remote = self
            .forge
            .status
            .as_ref()
            .and_then(|status| status.repo.as_ref())
            .map(|repo| repo.remote.clone());
        let review = review_bookmark(number);
        let target = MenuTarget::Pr {
            pr,
            remote: remote.as_deref(),
            review_bookmark_taken: snapshot
                .bookmarks
                .iter()
                .any(|b| b.is_local && b.name == review),
        };
        self.open_menu(position, "", target, cx);
    }

    /// A row of the operations timeline.
    pub fn open_operation_menu(
        &mut self,
        op_id: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        let Some(op) = snapshot.operations.iter().find(|op| op.id == op_id) else {
            return;
        };
        let target = MenuTarget::Operation {
            op_id,
            is_current: op.is_current,
        };
        self.open_menu(position, "", target, cx);
    }

    /// A conflicted path in the Conflicts inbox.
    pub fn open_conflict_menu(
        &mut self,
        change_id: Option<&str>,
        path: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        let node = change_id.and_then(|id| find_node(&snapshot, id));
        let target = MenuTarget::Conflict {
            change_id,
            path,
            is_working_copy: node.is_some_and(|node| node.kind == NodeKind::WorkingCopy),
            resolve_tool: can_resolve(&snapshot, change_id)
                .then_some(snapshot.resolve_tool.as_deref())
                .flatten(),
        };
        self.open_menu(position, "", target, cx);
    }

    /// Inside a diff: the file names the path, the surface the change, the
    /// row (when the click hit one) the line. A historical surface (the
    /// evolution pane) shows old versions' diffs: reading verbs stay, but
    /// mutations and resolve would act on the change's current content
    /// with stale coordinates — `readonly` turns them off.
    #[allow(clippy::too_many_arguments)]
    pub fn open_diff_menu(
        &mut self,
        file: &FileDiff,
        change_id: &str,
        readonly: bool,
        line: Option<(Option<u32>, Option<u32>)>,
        selection: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        let Some(node) = find_node(&snapshot, change_id) else {
            return;
        };
        let resolve = !readonly && file.has_conflict && can_resolve(&snapshot, Some(change_id));
        let target = MenuTarget::Diff {
            file,
            change_id,
            mutable: !readonly && node.kind != NodeKind::Immutable,
            line,
            resolve_tool: resolve
                .then_some(snapshot.resolve_tool.as_deref())
                .flatten(),
        };
        self.open_menu(position, selection, target, cx);
    }

    /// A text field, or text to read; `guesses` when the pointer is on a
    /// misspelling.
    pub fn open_editable_menu(
        &mut self,
        has_selection: bool,
        read_only: bool,
        guesses: Option<Vec<String>>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let target = MenuTarget::Editable {
            has_selection,
            read_only,
            guesses: guesses.as_deref(),
        };
        self.open_menu(position, "", target, cx);
    }

    /// The status bar's breadcrumb.
    pub fn open_breadcrumb_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.open_menu(position, "", MenuTarget::Breadcrumb, cx);
    }

    /// Anything that is nothing in particular. Text selected there still
    /// copies.
    pub fn open_surface_menu(
        &mut self,
        position: Point<Pixels>,
        selection: &str,
        cx: &mut Context<Self>,
    ) {
        self.open_menu(position, selection, MenuTarget::Surface, cx);
    }

    /// Run one command. Shared by the context menu and the palette; the
    /// clipboard operations on a text field are the caller's (they need
    /// the field).
    pub fn run_action(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        match action {
            MenuAction::Clipboard(_) | MenuAction::Spell(_) => {}
            MenuAction::CopyText(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            MenuAction::NewChild(id) => {
                self.mutate_quiet(cx, move |backend, path| backend.new_change(path, &id))
            }
            MenuAction::Edit(id) => {
                self.mutate_quiet(cx, move |backend, path| backend.edit_change(path, &id))
            }
            MenuAction::Duplicate(id) => {
                self.mutate_quiet(cx, move |backend, path| backend.duplicate_change(path, &id))
            }
            MenuAction::Revert(id) => {
                self.mutate_quiet(cx, move |backend, path| backend.revert_change(path, &id))
            }
            MenuAction::Absorb(id) => {
                self.mutate_quiet(cx, move |backend, path| backend.absorb_change(path, &id))
            }
            MenuAction::Intent(intent) => self.send_intent(intent, cx),
            MenuAction::Palette => self.toggle_palette(cx),
            MenuAction::Refresh => self.refresh_snapshot(cx),
            MenuAction::ChooseRepo => self.choose_repo(cx),
            MenuAction::Undo => self.undo_last_mutation(cx),
            MenuAction::Section(section) => self.go_to_section(section, cx),
            MenuAction::OpenUrl(url) => cx.open_url(&url),
            MenuAction::OpenInEditor { editor, path, line } => {
                self.open_in_editor(&editor, path, line, cx)
            }
            MenuAction::RevealInFinder(path) => cx.reveal_path(Path::new(&path)),
            MenuAction::OpenInTerminal(path) => self.open_in_terminal(&path, cx),
            MenuAction::ResolveConflict { change_id, path } => {
                let repo = self.repo_path();
                let task = self.resolve_conflict(change_id, path, cx);
                self.report(repo, task, cx);
            }
            MenuAction::AnnotateFile { change_id, path } => self.open_annotate(change_id, path, cx),
            MenuAction::RestoreFile { change_id, path } => self
                .mutate_quiet(cx, move |backend, repo| {
                    backend.restore_file(repo, &change_id, &path, None)
                }),
            MenuAction::RestoreHunk {
                change_id,
                path,
                hunk,
            } => self.mutate_quiet(cx, move |backend, repo| {
                backend.restore_file(repo, &change_id, &path, Some(&[hunk]))
            }),
            MenuAction::RestoreOperation(op) => self.mutate_quiet(cx, move |backend, path| {
                backend.restore_operation(path, &op)
            }),
            MenuAction::RevertOperation(op) => {
                self.mutate_quiet(cx, move |backend, path| backend.revert_operation(path, &op))
            }
            MenuAction::PushBookmark(name) => self.mutate_quiet(cx, move |backend, path| {
                backend.push_bookmarks(path, &[name], None)
            }),
            MenuAction::TrackBookmark { name, remote } => self
                .mutate_quiet(cx, move |backend, path| {
                    backend.track_bookmark(path, &name, &remote)
                }),
            MenuAction::UntrackBookmark { name, remote } => self
                .mutate_quiet(cx, move |backend, path| {
                    backend.untrack_bookmark(path, &name, &remote)
                }),
            MenuAction::DeleteBookmark(name) => self.mutate_quiet(cx, move |backend, path| {
                backend.delete_bookmark(path, &name)
            }),
            MenuAction::RerunCi(number) => {
                let Some(repo) = self.repo_path() else {
                    return;
                };
                let asked = self.bg(cx, {
                    let repo = repo.clone();
                    move |host| host.rerun_failed_ci(&repo, number)
                });
                let task = cx.spawn(async move |this, cx| {
                    asked.await?;
                    let _ = this.update(cx, |this, cx| this.refresh_forge_prs(cx));
                    Ok(())
                });
                self.report(Some(repo), task, cx);
            }
            MenuAction::FetchPr {
                remote,
                number,
                bookmark,
            } => self.mutate_quiet(cx, move |backend, path| {
                backend.fetch_pr_head(path, &remote, number, &bookmark)
            }),
        }
    }
}
