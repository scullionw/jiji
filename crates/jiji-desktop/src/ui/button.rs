//! Controls: buttons, the segmented control, checkboxes, keycaps.
//!
//! Each is a stop on the keyboard's path ([`super::focus`]) and names
//! itself to assistive technology: a button by its label, or by its
//! tooltip when it shows only an icon.

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, CursorStyle, Div, ElementId, FontWeight,
    Hsla, Role, SharedString, Stateful, Toggled, Window,
};

use super::a11y::{name_and_keys, NodeExt as _};
use super::focus::{halo, ring, Tabbable as _};
use super::{icon, tooltip, IconName};
use crate::theme::{theme, Theme, RADIUS_M, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(Clone, Copy, PartialEq)]
enum BtnKind {
    Secondary,
    Primary,
    Danger,
    DangerSolid,
    Ghost,
}

/// What a button tells assistive technology beyond its name.
#[derive(Default, Clone, Copy)]
struct States {
    expanded: Option<bool>,
    pressed: Option<bool>,
}

/// A button's node: its name (the label, or the tooltip's when it shows
/// only an icon), the rest of the tooltip as its longer story and the
/// shortcut the tooltip mentions, and whether it is open or held down.
fn described(
    el: Stateful<Div>,
    label: &SharedString,
    tip: Option<&SharedString>,
    states: States,
) -> Stateful<Div> {
    let (tip_name, keys) = tip.map_or(("", None), |tip| name_and_keys(tip));
    let name = if label.is_empty() {
        SharedString::from(tip_name.to_owned())
    } else {
        label.clone()
    };
    let mut el = el.role(Role::Button);
    if !tip_name.is_empty() && tip_name != name.as_ref() {
        el = el.aria_description(tip_name.to_owned());
    }
    if let Some(keys) = keys {
        el = el.aria_keyshortcuts(keys.to_owned());
    }
    if let Some(expanded) = states.expanded {
        el = el.aria_expanded(expanded);
    }
    if let Some(pressed) = states.pressed {
        el = el.aria_toggled(toggled(Some(pressed)));
    }
    el.aria_label(name)
}

/// A checked state as AccessKit has it: `None` is partly.
fn toggled(state: Option<bool>) -> Toggled {
    match state {
        Some(true) => Toggled::True,
        Some(false) => Toggled::False,
        None => Toggled::Mixed,
    }
}

/// A button. Secondary is the default: one step up from the pane on a
/// hairline.
#[derive(IntoElement)]
pub struct Btn {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    trailing: Option<AnyElement>,
    kind: BtnKind,
    large: bool,
    small: bool,
    armed: bool,
    disabled: bool,
    on: bool,
    states: States,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

pub fn btn(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Btn {
    Btn {
        id: id.into(),
        label: label.into(),
        icon: None,
        trailing: None,
        kind: BtnKind::Secondary,
        large: false,
        small: false,
        armed: false,
        disabled: false,
        on: false,
        states: States::default(),
        tooltip: None,
        on_click: None,
    }
}

impl Btn {
    pub fn primary(mut self) -> Self {
        self.kind = BtnKind::Primary;
        self
    }

    pub fn danger(mut self) -> Self {
        self.kind = BtnKind::Danger;
        self
    }

    pub fn danger_solid(mut self) -> Self {
        self.kind = BtnKind::DangerSolid;
        self
    }

    pub fn ghost(mut self) -> Self {
        self.kind = BtnKind::Ghost;
        self
    }

    pub fn large(mut self) -> Self {
        self.large = true;
        self
    }

    /// The compact size row-level actions use.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// The button's panel is open under it.
    pub fn armed(mut self, armed: bool) -> Self {
        self.armed = armed;
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Content after the label (a keycap, a chevron).
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Held down: a ghost button whose panel is open.
    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    /// Whether the panel or tray it opens is open, for assistive
    /// technology.
    pub fn expanded(mut self, expanded: bool) -> Self {
        self.states.expanded = Some(expanded);
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Btn {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let transparent = gpui::transparent_black();
        let (bg, border, fg, icon_fg, hover_bg, hover_border, hover_fg) = match self.kind {
            BtnKind::Secondary => (
                t.btn_bg,
                t.border1,
                t.text1,
                t.text3,
                t.btn_hover,
                t.border1,
                t.text1,
            ),
            BtnKind::Primary => (
                t.accent,
                transparent,
                t.accent_contrast,
                t.accent_contrast,
                t.accent_hover,
                transparent,
                t.accent_contrast,
            ),
            BtnKind::Danger => (
                t.btn_bg,
                t.border1,
                t.danger,
                t.danger,
                t.danger_dim,
                Theme::fade(t.danger, 0.4),
                t.danger,
            ),
            BtnKind::DangerSolid => {
                let white = gpui::white();
                (
                    t.danger,
                    transparent,
                    white,
                    white,
                    Theme::blend(t.danger, 0.88, gpui::black()),
                    transparent,
                    white,
                )
            }
            BtnKind::Ghost if self.on => (
                t.bg_hover,
                transparent,
                t.text1,
                t.text1,
                t.bg_hover,
                transparent,
                t.text1,
            ),
            BtnKind::Ghost => (
                transparent,
                transparent,
                t.text2,
                t.text3,
                t.bg_hover,
                transparent,
                t.text1,
            ),
        };
        // On a filled button the accent ring would vanish into the fill: it
        // takes the label's ink, with the accent's halo outside the fill.
        let filled = matches!(self.kind, BtnKind::Primary | BtnKind::DangerSolid);
        let (ink, accent) = (if filled { fg } else { t.accent }, t.accent);
        let el = div()
            .id(self.id)
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .gap(px(if self.small { 5. } else { 6. }))
            .h(px(match (self.large, self.small) {
                (true, _) => 30.,
                (_, true) => 22.,
                _ => 26.,
            }))
            .px(px(match (self.large, self.small) {
                (true, _) => 14.,
                (_, true) => 8.,
                _ => 10.,
            }))
            .rounded(px(RADIUS_M))
            .border_1()
            .border_color(if self.armed { t.accent } else { border })
            .bg(bg)
            .text_color(if self.armed { t.accent_strong } else { fg })
            .text_size(px(match (self.large, self.small) {
                (true, _) => TEXT_M,
                (_, true) => TEXT_XS,
                _ => TEXT_S,
            }))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap();
        let mut el = described(el, &self.label, self.tooltip.as_ref(), self.states);
        if let Some(name) = self.icon {
            el = el.child(icon(name, if self.small { 11. } else { 14. }, icon_fg));
        }
        if !self.label.is_empty() {
            el = el.child(self.label);
        }
        if let Some(trailing) = self.trailing {
            el = el.child(trailing);
        }
        if self.disabled {
            return el
                .opacity(0.5)
                .cursor(CursorStyle::Arrow)
                .a11y(|node| node.set_disabled());
        }
        el = el
            .tabbable_styled(move |style| {
                let mut shadows = ring(ink);
                if filled {
                    shadows.extend(halo(accent));
                }
                style.border_color(ink).shadow(shadows)
            })
            .cursor_pointer()
            .hover(move |style| {
                style
                    .bg(hover_bg)
                    .border_color(hover_border)
                    .text_color(hover_fg)
            });
        if let Some(text) = self.tooltip {
            el = el.tooltip(tooltip(text));
        }
        if let Some(handler) = self.on_click {
            el = el.on_click(handler);
        }
        el
    }
}

/// A square icon-only control, named by its tooltip unless given a name
/// of its own.
#[derive(IntoElement)]
pub struct IconBtn {
    id: ElementId,
    label: SharedString,
    icon: IconName,
    size: f32,
    icon_size: f32,
    on: bool,
    disabled: bool,
    color: Option<Hsla>,
    states: States,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

pub fn icon_btn(id: impl Into<ElementId>, icon: IconName) -> IconBtn {
    IconBtn {
        id: id.into(),
        label: SharedString::default(),
        icon,
        size: 26.,
        icon_size: 15.,
        on: false,
        disabled: false,
        color: None,
        states: States::default(),
        tooltip: None,
        on_click: None,
    }
}

impl IconBtn {
    pub fn size(mut self, size: f32, icon_size: f32) -> Self {
        self.size = size;
        self.icon_size = icon_size;
        self
    }

    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Its name, when the tooltip says what a press does rather than what
    /// the control is (a toggle), or names too little.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    /// Whether the panel it opens is open, for assistive technology.
    pub fn expanded(mut self, expanded: bool) -> Self {
        self.states.expanded = Some(expanded);
        self
    }

    /// Whether it is switched on, when it is a toggle.
    pub fn pressed(mut self, pressed: bool) -> Self {
        self.states.pressed = Some(pressed);
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for IconBtn {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let rest = self
            .color
            .unwrap_or(if self.on { t.text1 } else { t.text3 });
        let hover = t.text1;
        let group = SharedString::from(format!("icon-btn-{:?}", self.id));
        let el = div()
            .id(self.id)
            .group(group.clone())
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(self.size))
            .rounded(px(RADIUS_M));
        let mut el = described(el, &self.label, self.tooltip.as_ref(), self.states);
        if self.on {
            el = el.bg(t.bg_hover);
        }
        let mut glyph = icon(self.icon, self.icon_size, rest);
        if self.disabled {
            return el
                .opacity(0.4)
                .a11y(|node| node.set_disabled())
                .child(glyph);
        }
        glyph = glyph.group_hover(group, move |style| style.text_color(hover));
        el = el
            .tabbable(cx)
            .cursor_pointer()
            .hover(|style| style.bg(t.bg_hover))
            .child(glyph);
        if let Some(text) = self.tooltip {
            el = el.tooltip(tooltip(text));
        }
        if let Some(handler) = self.on_click {
            el = el.on_click(handler);
        }
        el
    }
}

/// One choice of a [`segmented`] control: a label, an icon, or both. An
/// icon alone still has a name, for assistive technology.
pub struct Segment<T> {
    value: T,
    label: SharedString,
    icon: Option<IconName>,
    name: SharedString,
}

impl<T> Segment<T> {
    pub fn text(value: T, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            value,
            name: label.clone(),
            label,
            icon: None,
        }
    }

    pub fn icon(value: T, icon: IconName, name: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: SharedString::default(),
            icon: Some(icon),
            name: name.into(),
        }
    }

    /// An icon before the label.
    pub fn with_icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// A segmented control: a recessed track, the active segment raised. To
/// assistive technology, a radio group named `name`.
pub fn segmented<T: Clone + PartialEq + 'static>(
    id: &'static str,
    name: impl Into<SharedString>,
    options: Vec<Segment<T>>,
    active: &T,
    cx: &App,
    on_pick: impl Fn(&T, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let t = theme(cx);
    let on_pick = std::rc::Rc::new(on_pick);
    let mut track = div()
        .id(id)
        .role(Role::RadioGroup)
        .aria_label(name)
        .flex()
        .flex_row()
        .flex_shrink_0()
        .gap(px(1.))
        .p(px(1.))
        .bg(t.bg_well)
        .border_1()
        .border_color(t.border2)
        .rounded(px(RADIUS_M));
    for (ix, segment) in options.into_iter().enumerate() {
        let Segment {
            value,
            label,
            icon: glyph,
            name,
        } = segment;
        let is_active = value == *active;
        let on_pick = on_pick.clone();
        let fg = if is_active { t.text1 } else { t.text3 };
        let mut segment = div()
            .id((id, ix))
            .role(Role::RadioButton)
            .aria_label(name)
            .aria_toggled(toggled(Some(is_active)))
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .gap(px(5.))
            .h(px(22.))
            .px(px(8.))
            .rounded(px(RADIUS_M - 1.))
            .text_size(px(TEXT_S))
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg)
            .tabbable(cx)
            .cursor_pointer()
            .hover(|style| style.text_color(t.text1))
            .on_click(move |_, window, cx| on_pick(&value, window, cx));
        if is_active {
            segment = segment.bg(t.bg3).border_1().border_color(t.border2);
        } else {
            segment = segment.border_1().border_color(gpui::transparent_black());
        }
        if let Some(glyph) = glyph {
            segment = segment.child(icon(glyph, 13., fg));
        }
        if !label.is_empty() {
            segment = segment.child(label);
        }
        track = track.child(segment);
    }
    track
}

/// A keycap: a quiet outlined tile.
pub fn kbd(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .min_w(px(18.))
        .h(px(18.))
        .px(px(4.))
        .text_size(px(TEXT_XS))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.text3)
        .border_1()
        .border_color(t.border1)
        .rounded(px(RADIUS_S))
        .child(text.into())
}

/// A checkbox: checked, unchecked, or partially selected (`None`), named
/// `name` for assistive technology.
pub fn checkbox(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    state: Option<bool>,
    cx: &App,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let t = theme(cx);
    let filled = state != Some(false);
    let accent = t.accent;
    let mut el = div()
        .id(id)
        .role(Role::CheckBox)
        .aria_label(name)
        .aria_toggled(toggled(state))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(14.))
        .rounded(px(3.))
        .border_1()
        .tabbable_styled(move |style| style.shadow(halo(accent)))
        .cursor_pointer()
        .on_click(on_toggle);
    if filled {
        el = el.bg(t.accent).border_color(t.accent).child(icon(
            if state.is_some() {
                IconName::Check
            } else {
                IconName::Minus
            },
            10.,
            t.accent_contrast,
        ));
    } else {
        el = el.bg(t.bg_well).border_color(t.border1);
    }
    el
}

#[cfg(test)]
mod tests {
    use gpui::accesskit::{Node, Role, Toggled};
    use gpui::{prelude::*, Element, TestAppContext};

    use super::{btn, checkbox, icon_btn, segmented, Segment};
    use crate::ui::IconName;

    /// What an element tells AccessKit about itself.
    fn node(element: &impl Element) -> (Option<Role>, Node) {
        let role = element.a11y_role();
        let mut node = Node::new(role.unwrap_or(Role::Unknown));
        element.write_a11y_info(&mut node);
        (role, node)
    }

    #[gpui::test]
    fn controls_tell_assistive_technology_what_they_are(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("control-nodes");
        crate::testing::install(&dir, cx);
        let window = cx.add_empty_window();
        window.update(|window, cx| {
            // A labelled button: its label, the tooltip's story, the keys.
            let save = btn("save", "Save")
                .tooltip("Save the description (⌘↵)")
                .render(window, cx)
                .into_element();
            let (role, save) = node(&save);
            assert_eq!(role, Some(Role::Button));
            assert_eq!(save.label(), Some("Save"));
            assert_eq!(save.description(), Some("Save the description"));
            assert_eq!(save.keyboard_shortcut(), Some("⌘↵"));

            // An icon alone is named by its tooltip, and says what it opens.
            let settings = icon_btn("settings", IconName::Settings)
                .tooltip("Settings")
                .expanded(true)
                .render(window, cx)
                .into_element();
            let (_, settings) = node(&settings);
            assert_eq!(settings.label(), Some("Settings"));
            assert_eq!(settings.description(), None);
            assert_eq!(settings.is_expanded(), Some(true));

            // A toggle keeps its name while its tooltip says what a press
            // would do.
            let rows = icon_btn("density", IconName::RowsCompact)
                .label("Classic rows")
                .pressed(false)
                .tooltip("Compact rows — switch to larger, classic rows")
                .render(window, cx)
                .into_element();
            let (_, rows) = node(&rows);
            assert_eq!(rows.label(), Some("Classic rows"));
            assert_eq!(rows.toggled(), Some(Toggled::False));

            let modes = segmented(
                "mode",
                "Workbench view",
                vec![Segment::text(0, "Graph"), Segment::text(1, "Focus")],
                &0,
                cx,
                |_, _, _| {},
            );
            let (role, modes) = node(&modes);
            assert_eq!(role, Some(Role::RadioGroup));
            assert_eq!(modes.label(), Some("Workbench view"));

            let partly = checkbox("file", "src/main.rs", None, cx, |_, _, _| {});
            let (role, partly) = node(&partly);
            assert_eq!(role, Some(Role::CheckBox));
            assert_eq!(partly.label(), Some("src/main.rs"));
            assert_eq!(partly.toggled(), Some(Toggled::Mixed));
        });
        let _ = std::fs::remove_dir_all(dir);
    }
}
