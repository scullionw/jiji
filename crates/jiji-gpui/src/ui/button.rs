//! Controls: buttons, the segmented control, checkboxes, keycaps.

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, CursorStyle, Div, ElementId, FontWeight,
    Hsla, SharedString, Stateful, Window,
};

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
        let mut el = div()
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
                (_, true) => crate::theme::TEXT_XS,
                _ => TEXT_S,
            }))
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap();
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
            return el.opacity(0.5).cursor(CursorStyle::Arrow);
        }
        el = el.cursor_pointer().hover(move |style| {
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

/// A square icon-only control.
#[derive(IntoElement)]
pub struct IconBtn {
    id: ElementId,
    icon: IconName,
    size: f32,
    icon_size: f32,
    on: bool,
    disabled: bool,
    color: Option<Hsla>,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

pub fn icon_btn(id: impl Into<ElementId>, icon: IconName) -> IconBtn {
    IconBtn {
        id: id.into(),
        icon,
        size: 26.,
        icon_size: 15.,
        on: false,
        disabled: false,
        color: None,
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
        let mut el = div()
            .id(self.id)
            .group(group.clone())
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(self.size))
            .rounded(px(RADIUS_M));
        if self.on {
            el = el.bg(t.bg_hover);
        }
        let mut glyph = icon(self.icon, self.icon_size, rest);
        if self.disabled {
            return el.opacity(0.4).child(glyph);
        }
        glyph = glyph.group_hover(group, move |style| style.text_color(hover));
        el = el
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

/// A segmented control: a recessed track, the active segment raised.
pub fn segmented<T: Clone + PartialEq + 'static>(
    id: &'static str,
    options: Vec<(T, SharedString, Option<IconName>)>,
    active: &T,
    cx: &App,
    on_pick: impl Fn(&T, &mut Window, &mut App) + 'static,
) -> Div {
    let t = theme(cx);
    let on_pick = std::rc::Rc::new(on_pick);
    let mut track = div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .gap(px(1.))
        .p(px(1.))
        .bg(t.bg_well)
        .border_1()
        .border_color(t.border2)
        .rounded(px(RADIUS_M));
    for (ix, (value, label, glyph)) in options.into_iter().enumerate() {
        let is_active = value == *active;
        let on_pick = on_pick.clone();
        let fg = if is_active { t.text1 } else { t.text3 };
        let mut segment = div()
            .id((id, ix))
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

/// A checkbox: checked, unchecked, or partially selected.
pub fn checkbox(
    id: impl Into<ElementId>,
    state: Option<bool>,
    cx: &App,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let t = theme(cx);
    let filled = state != Some(false);
    let mut el = div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(14.))
        .rounded(px(3.))
        .border_1()
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
