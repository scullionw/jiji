//! Where the interface is looking: the section, the overlays, the
//! selection, and the intents one surface hands to another.

use gpui::Context;
use jiji_core::snapshot::SnapshotView;

use super::{AppEvent, AppModel, Section, UiIntent};
use crate::logic::inspect::{find_node, stack_position};

impl AppModel {
    pub fn go_to_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.snapshot().is_none() || self.section() == section {
            return;
        }
        self.tab_mut().ui.section = section;
        cx.emit(AppEvent::Navigation);
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.store.set_pref("rail.hidden", &self.sidebar_collapsed);
        cx.notify();
    }

    pub fn toggle_graph_panel(&mut self, cx: &mut Context<Self>) {
        self.graph_collapsed = !self.graph_collapsed;
        self.store
            .set_pref("graph.collapsed", &self.graph_collapsed);
        cx.notify();
    }

    pub fn toggle_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_open = !self.palette_open;
        if self.palette_open {
            self.search_open = false;
        }
        cx.notify();
    }

    pub fn close_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_open = false;
        cx.notify();
    }

    pub fn open_search(&mut self, seed: impl Into<String>, cx: &mut Context<Self>) {
        if self.snapshot().is_none() {
            return;
        }
        self.search_seed = seed.into();
        self.search_open = true;
        self.palette_open = false;
        cx.notify();
    }

    pub fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_open = false;
        cx.notify();
    }

    /// Select one change in the workbench graph (a row click). Moving the
    /// selection collapses any range and closes the reading panes.
    pub fn select_node(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        if tab.ui.selected_node == id && tab.ui.selected_range.is_none() {
            return;
        }
        tab.ui.selected_node = id;
        tab.ui.selected_range = None;
        Self::settle_selection(tab);
        cx.emit(AppEvent::Selection);
        cx.notify();
    }

    /// ⇧-click: extend from the anchor (the selected change) along its
    /// lane. Falls back to a plain select when the two share no lane.
    pub fn extend_selection(&mut self, target: String, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        let anchor = self.ui().selected_node.clone();
        let range = anchor
            .as_deref()
            .and_then(|anchor| crate::logic::inspect::extend_range(&snapshot, anchor, &target));
        match range {
            Some(range) if range.len() >= 2 => {
                self.tab_mut().ui.selected_range = Some(range);
                cx.emit(AppEvent::Selection);
                cx.notify();
            }
            Some(_) => {
                self.tab_mut().ui.selected_range = None;
                cx.emit(AppEvent::Selection);
                cx.notify();
            }
            None => self.select_node(Some(target), cx),
        }
    }

    pub fn focus_workstream(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.tab_mut().ui.focused_workstream = id;
        cx.notify();
    }

    /// Move the workbench selection to a change (the palette's "go to"),
    /// focusing its workstream like a graph-row click would.
    pub fn jump_to_change(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot().cloned() else {
            return;
        };
        if find_node(&snapshot, id).is_none() {
            return;
        }
        let navigated = self.section() != Section::Workbench;
        let tab = self.tab_mut();
        tab.ui.section = Section::Workbench;
        tab.ui.selected_node = Some(id.to_owned());
        tab.ui.selected_range = None;
        if let Some(position) = stack_position(&snapshot, id) {
            tab.ui.focused_workstream = Some(position.workstream.id.clone());
        }
        Self::settle_selection(tab);
        if navigated {
            cx.emit(AppEvent::Navigation);
        }
        cx.emit(AppEvent::Selection);
        cx.notify();
    }

    /// Select a change a search hit names, wherever it is: a change the
    /// current lens draws is a plain jump; one it does not re-renders the
    /// graph under a transient custom revset pinning that change.
    /// Deliberately not persisted — a refresh or lens switch returns to the
    /// user's own choice.
    pub fn reveal_change(&mut self, id: String, cx: &mut Context<Self>) {
        let drawn = self
            .snapshot()
            .is_some_and(|snapshot| find_node(snapshot, &id).is_some());
        if drawn {
            self.jump_to_change(&id, cx);
            return;
        }
        let path = self.repo_path();
        let applied =
            self.set_snapshot_view(SnapshotView::Custom { revset: id.clone() }, false, cx);
        let task = cx.spawn(async move |this, cx| {
            applied.await?;
            let _ = this.update(cx, |this, cx| this.jump_to_change(&id, cx));
            Ok(())
        });
        self.report(path, task, cx);
    }

    /// Open one file's line-by-line annotation over the diff surface.
    pub fn open_annotate(&mut self, change_id: String, path: String, cx: &mut Context<Self>) {
        let navigated = self.section() != Section::Workbench;
        let tab = self.tab_mut();
        tab.ui.section = Section::Workbench;
        tab.ui.evolution = None;
        tab.ui.annotate = Some((change_id, path));
        if navigated {
            cx.emit(AppEvent::Navigation);
        }
        cx.notify();
    }

    pub fn close_annotate(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().ui.annotate = None;
        cx.notify();
    }

    /// Open a change's evolution timeline over the diff surface. Shares
    /// the surface with annotate, so one replaces the other.
    pub fn open_evolution(&mut self, change_id: String, cx: &mut Context<Self>) {
        let navigated = self.section() != Section::Workbench;
        let tab = self.tab_mut();
        tab.ui.section = Section::Workbench;
        tab.ui.annotate = None;
        tab.ui.evolution = Some(change_id);
        if navigated {
            cx.emit(AppEvent::Navigation);
        }
        cx.notify();
    }

    pub fn close_evolution(&mut self, cx: &mut Context<Self>) {
        self.tab_mut().ui.evolution = None;
        cx.notify();
    }

    /// Every intent switches to the section whose surface owns it — the
    /// workbench for the change/diff panels, Publish for the land and ship
    /// plans — so the owning surface mounts (if it wasn't) and takes it.
    pub fn send_intent(&mut self, intent: UiIntent, cx: &mut Context<Self>) {
        if self.snapshot().is_none() {
            return;
        }
        let section = match intent {
            UiIntent::Land { .. } | UiIntent::Ship { .. } => Section::Publish,
            _ => Section::Workbench,
        };
        let navigated = self.section() != section;
        self.tab_mut().ui.section = section;
        self.intent = Some(intent);
        if navigated {
            cx.emit(AppEvent::Navigation);
        }
        cx.emit(AppEvent::Intent);
        cx.notify();
    }

    /// Take the pending intent when `owns` says it is this surface's.
    pub fn take_intent(&mut self, owns: impl FnOnce(&UiIntent) -> bool) -> Option<UiIntent> {
        if self.intent.as_ref().is_some_and(owns) {
            self.intent.take()
        } else {
            None
        }
    }
}
