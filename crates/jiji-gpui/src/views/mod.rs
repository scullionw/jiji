//! The section views and the overlays above them.

mod bookmarks;
mod conflicts;
pub mod menu;
mod operations;
pub mod palette;
pub mod publish;
pub mod search;
mod welcome;
pub mod workbench;

use gpui::{
    actions, div, prelude::*, AnyView, App, Context, FocusHandle, Focusable, KeyBinding, Render,
    Window,
};

use crate::app::Section;
use crate::ui::{empty_state, IconName};

actions!(views, [ConfirmPlan]);

/// Keys every section's plan panels share.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-enter", ConfirmPlan, Some("PlanPanel"))]);
}

/// Build the view for a section; `None` is the welcome screen (no repo on
/// this tab yet). Views are rebuilt on every tab or section change, so no
/// view-local state leaks from one repo's view into another's.
pub fn build(
    section: Option<Section>,
    window: &mut Window,
    cx: &mut App,
) -> (AnyView, Option<FocusHandle>) {
    match section {
        None => (
            cx.new(|cx| welcome::WelcomeView::new(window, cx)).into(),
            None,
        ),
        Some(Section::Workbench) => {
            let view = cx.new(|cx| workbench::WorkbenchView::new(window, cx));
            let focus = view.focus_handle(cx);
            (view.into(), Some(focus))
        }
        Some(Section::Conflicts) => {
            let view = cx.new(conflicts::ConflictsView::new);
            let focus = view.focus_handle(cx);
            (view.into(), Some(focus))
        }
        Some(Section::Publish) => {
            let view = cx.new(|cx| publish::PublishView::new(window, cx));
            let focus = view.focus_handle(cx);
            (view.into(), Some(focus))
        }
        Some(Section::Bookmarks) => {
            let view = cx.new(|cx| bookmarks::BookmarksView::new(window, cx));
            let focus = view.focus_handle(cx);
            (view.into(), Some(focus))
        }
        Some(Section::Operations) => {
            let view = cx.new(operations::OperationsView::new);
            let focus = view.focus_handle(cx);
            (view.into(), Some(focus))
        }
        Some(section) => (cx.new(|_| Placeholder { section }).into(), None),
    }
}

/// A section that has not arrived yet.
struct Placeholder {
    section: Section,
}

impl Render for Placeholder {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(empty_state(
            IconName::Workspaces,
            self.section.label(),
            "Switching between this repository's workspaces, and recovering a stale one, \
             arrive here in a later release.",
            Some("jj workspace list".into()),
            cx,
        ))
    }
}
