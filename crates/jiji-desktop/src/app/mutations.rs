//! Changing the repository: every mutation takes the same road, so the
//! snapshot, the breadcrumb and the selection always follow it.

use std::path::Path;
use std::time::Instant;

use gpui::{Context, Task, WeakEntity};
use jiji_core::snapshot::MutationOutcome;
use jiji_core::{BackendError, RepoBackend};

use super::{no_repo_open, AppEvent, AppModel, Breadcrumb};
use crate::host::HostResult;
use crate::logic::inspect::{find_node, stack_position};

impl AppModel {
    /// Every mutation flows through this shape: backend action → refreshed
    /// snapshot → breadcrumb → selection follows the outcome's target (the
    /// new working copy after `new`, the parent after abandon or squash —
    /// without this the selection would dangle on a removed node). Errors
    /// come back to the caller so the initiating surface can show them
    /// inline (its editor or confirm panel stays open).
    pub fn mutate(
        &mut self,
        cx: &mut Context<Self>,
        action: impl FnOnce(&dyn RepoBackend, &Path) -> Result<MutationOutcome, BackendError>
            + Send
            + 'static,
    ) -> Task<HostResult<MutationOutcome>> {
        // The mutation targets the repo on screen when it starts; if
        // another tab takes over mid-flight, results land on that repo's
        // tab instead of the one now showing.
        let Some(path) = self.repo_path() else {
            return Task::ready(Err(no_repo_open()));
        };
        let ran = self.bg(cx, {
            let path = path.clone();
            move |host| host.mutate(&path, action)
        });
        cx.spawn(async move |this, cx| {
            let outcome = ran.await?;
            let _ = this.update(cx, |this, cx| this.after_mutation(&path, &outcome, cx));
            Ok(outcome)
        })
    }

    pub(super) fn after_mutation(
        &mut self,
        path: &str,
        outcome: &MutationOutcome,
        cx: &mut Context<Self>,
    ) {
        // The refreshed snapshot also arrives as a host event; pulling it
        // here makes the flow self-contained and deterministic.
        if let Some(snapshot) = self.host.snapshot_for(path) {
            self.apply_snapshot(snapshot, cx);
        }
        let Some(tab) = self.tab_for_path(path) else {
            return;
        };
        if outcome.operation_id.is_some() {
            tab.ui.last_mutation = Some(Breadcrumb {
                outcome: outcome.clone(),
                at: Instant::now(),
            });
        }
        if let (Some(target), Some(snapshot)) = (&outcome.target_change, tab.snapshot.clone()) {
            if find_node(&snapshot, target).is_some() {
                tab.ui.selected_node = Some(target.clone());
                tab.ui.selected_range = None;
                if let Some(position) = stack_position(&snapshot, target) {
                    tab.ui.focused_workstream = Some(position.workstream.id.clone());
                }
                Self::settle_selection(tab);
            }
        }
        cx.emit(AppEvent::Selection);
        cx.notify();
    }

    /// Run a mutation whose surface has nowhere to render an error inline
    /// (palette rows, menu items, drag and drop): failures land in the
    /// status bar's error slot.
    pub fn mutate_quiet(
        &mut self,
        cx: &mut Context<Self>,
        action: impl FnOnce(&dyn RepoBackend, &Path) -> Result<MutationOutcome, BackendError>
            + Send
            + 'static,
    ) {
        let path = self.repo_path();
        let task = self.mutate(cx, action);
        self.report(path, task, cx);
    }

    /// Send a task's failure to the status bar of the tab it started on.
    pub fn report<T: 'static>(
        &mut self,
        path: Option<String>,
        task: Task<HostResult<T>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |this, cx| match &path {
                    Some(path) => this.set_error_for(path, error.message, cx),
                    None => this.set_error(error.message, cx),
                });
            }
        })
        .detach();
    }

    /// Hand one conflicted file to the external merge tool and wait for
    /// the tool's window to close. `resolving_conflict` stays set the
    /// whole time so every Resolve affordance shows the waiting state.
    pub fn resolve_conflict(
        &mut self,
        change_id: String,
        file_path: String,
        cx: &mut Context<Self>,
    ) -> Task<HostResult<MutationOutcome>> {
        let repo = self.repo_path();
        self.tab_mut().ui.resolving_conflict = Some((change_id.clone(), file_path.clone()));
        cx.notify();
        let task = self.mutate(cx, move |backend, path| {
            backend.resolve_conflict(path, &change_id, &file_path)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                // The waiting state belongs to the repo's tab, wherever
                // that tab is by now.
                if let Some(tab) = repo.as_deref().and_then(|repo| this.tab_for_path(repo)) {
                    tab.ui.resolving_conflict = None;
                }
                cx.notify();
            });
            result
        })
    }

    /// The breadcrumb's Undo: revert the operation the last mutation
    /// recorded. The undo itself records an operation and becomes the new
    /// breadcrumb, so its own Undo is redo.
    pub fn undo_last_mutation(&mut self, cx: &mut Context<Self>) {
        let Some(op_id) = self
            .ui()
            .last_mutation
            .as_ref()
            .and_then(|crumb| crumb.outcome.operation_id.clone())
        else {
            return;
        };
        self.mutate_quiet(cx, move |backend, path| {
            backend.revert_operation(path, &op_id)
        });
    }

    pub fn dismiss_breadcrumb(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().ui.last_mutation = None;
        cx.notify();
    }

    pub fn dismiss_error(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().ui.error = None;
        cx.notify();
    }
}
