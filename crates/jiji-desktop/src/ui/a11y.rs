//! What assistive technology is told. GPUI reports an element to AccessKit
//! only when it has both an id and a role; everything else on screen is
//! paint. So controls name themselves, lists and trees state their shape
//! and what is selected or open, dialogs say what they are for, and what
//! changes on its own — a search's count, the status line, a failed
//! action — is a live region, read out as it changes.

use gpui::accesskit::{Live, Node};
use gpui::{div, prelude::*, Div, ElementId, Role, SharedString, Stateful};

pub trait NodeExt: StatefulInteractiveElement + Sized {
    /// Set on the element's node what the element builder has no setter
    /// for (live regions, disabled, modal, multiselect).
    fn a11y(self, f: impl FnOnce(&mut Node) + 'static) -> Self {
        self.a11y_synthetic_children(move |builder| f(builder.parent_node()))
    }

    /// A live region: its value is read out whenever it changes.
    fn live(self, live: Live) -> Self {
        self.a11y(move |node| node.set_live(live))
    }

    /// Something failed, read out at once.
    fn alert(self, text: impl Into<SharedString>) -> Self {
        self.role(Role::Alert)
            .aria_value(text)
            .live(Live::Assertive)
    }

    /// How something stands (a count, progress), read out when the reader
    /// is free.
    fn status(self, text: impl Into<SharedString>) -> Self {
        self.role(Role::Status).aria_value(text).live(Live::Polite)
    }
}

impl<E: StatefulInteractiveElement> NodeExt for E {}

/// A live region with nothing on screen, for a list whose keys stay in a
/// text field (the palette, history search) or that never takes them (the
/// context menu): what the highlight moved to is read out, since focus,
/// and so the reader, stays where it was.
pub fn spoken(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    div().id(id).absolute().size_0().status(text)
}

/// A tooltip's text as the name it gives a control and the shortcut it
/// mentions: "New tab (⌘T)" is "New tab", pressed with ⌘T.
pub fn name_and_keys(tip: &str) -> (&str, Option<&str>) {
    let Some(open) = tip.rfind(" (").filter(|_| tip.ends_with(')')) else {
        return (tip, None);
    };
    let keys = &tip[open + 2..tip.len() - 1];
    let is_keys =
        keys == "esc" || (keys.starts_with(['⌘', '⌥', '⇧', '⌃', '↩']) && !keys.contains(' '));
    if is_keys {
        (&tip[..open], Some(keys))
    } else {
        (tip, None)
    }
}

#[cfg(test)]
mod tests {
    use super::name_and_keys;

    #[test]
    fn a_tooltip_names_its_control_apart_from_its_shortcut() {
        assert_eq!(name_and_keys("New tab (⌘T)"), ("New tab", Some("⌘T")));
        assert_eq!(
            name_and_keys("Hide graph panel (⌘⇧B)"),
            ("Hide graph panel", Some("⌘⇧B"))
        );
        assert_eq!(name_and_keys("Close (esc)"), ("Close", Some("esc")));
        assert_eq!(
            name_and_keys("Previous match (⇧↩)"),
            ("Previous match", Some("⇧↩"))
        );
        assert_eq!(name_and_keys("Settings"), ("Settings", None));
        // A parenthesis that is not a shortcut stays part of the name.
        assert_eq!(
            name_and_keys("Squash into parent (jj squash)"),
            ("Squash into parent (jj squash)", None)
        );
    }
}
