//! Shared primitives. Every control in the app is one of these, so a
//! button in a popover, a panel, and a section page are the same object.
//! Views add layout; they don't restyle the material.

pub mod icon;
pub mod input;
pub mod scroll;
pub mod truncate;

use gpui::{
    div, prelude::*, px, AnyElement, AnyView, App, ClickEvent, Context, CursorStyle, Div,
    ElementId, FontWeight, Hsla, MouseButton, Render, SharedString, Stateful, Window,
};

pub use self::icon::{icon, IconName};
pub use self::input::{InputEvent, TextInput};
pub use self::scroll::scrollbar;
pub use self::truncate::truncated;
use crate::theme::{theme, Theme, RADIUS_L, RADIUS_M, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS};

pub fn h_flex() -> Div {
    div().flex().flex_row().items_center()
}

pub fn v_flex() -> Div {
    div().flex().flex_col()
}

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
                t.bg3,
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
                t.bg3,
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

/// A ref label: a bookmark, tag or remote name riding a row.
pub fn ref_label(glyph: Option<IconName>, text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    let mut el = div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .gap(px(3.))
        .h(px(17.))
        .px(px(5.))
        .rounded(px(RADIUS_S))
        .bg(t.ref_bg)
        .text_color(t.text2)
        .font_family(t.mono.clone())
        .text_size(px(TEXT_XS))
        .line_height(px(17.))
        .whitespace_nowrap();
    if let Some(glyph) = glyph {
        el = el.child(icon(glyph, 10., t.text3));
    }
    el.child(truncated(text))
}

/// Floating surfaces: menus, popovers, the palette, drag cards. Opaque,
/// one hairline, the system's only shadow.
pub fn popover(cx: &App) -> Div {
    let t = theme(cx);
    div()
        .bg(t.bg2)
        .border_1()
        .border_color(t.border1)
        .rounded(px(RADIUS_L))
        .shadow(t.popover_shadow())
        .text_color(t.text1)
        .text_size(px(TEXT_M))
        .occlude()
}

/// A group heading inside a popover or menu.
pub fn menu_label(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .pt(px(8.))
        .px(px(8.))
        .pb(px(4.))
        .text_size(px(TEXT_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.text3)
        .child(text.into())
}

/// One row of a menu: an optional icon, a label, an optional trailing hint.
pub fn menu_row(
    id: impl Into<ElementId>,
    glyph: Option<IconName>,
    label: impl Into<SharedString>,
    cx: &App,
) -> Stateful<Div> {
    let t = theme(cx);
    let mut row = div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .h(px(26.))
        .px(px(8.))
        .rounded(px(RADIUS_S))
        .text_size(px(TEXT_S))
        .text_color(t.text1)
        .cursor_pointer()
        .hover(|style| style.bg(t.bg_hover));
    if let Some(glyph) = glyph {
        row = row.child(icon(glyph, 14., t.text3));
    }
    row.child(div().flex_1().min_w_0().child(truncated(label)))
}

/// A hairline between groups of a menu or panel.
pub fn separator(cx: &App) -> Div {
    div().h(px(1.)).my(px(4.)).bg(theme(cx).border2)
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

/// A small status dot.
pub fn dot(color: Hsla, size: f32) -> Div {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded(px(size / 2.))
        .bg(color)
}

/// A quiet placeholder for a surface with nothing on it: the glyph in
/// tertiary ink, one line of what, one of why, and the jj command that
/// would change it.
pub fn empty_state(
    glyph: IconName,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    hint: Option<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    let mut el = v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .p(px(32.))
        .child(div().mb(px(8.)).child(icon(glyph, 20., t.text3)))
        .child(
            div()
                .text_size(px(TEXT_M))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text1)
                .child(title.into()),
        )
        .child(
            div()
                .max_w(px(380.))
                .text_size(px(TEXT_S))
                .line_height(gpui::relative(1.55))
                .text_color(t.text2)
                .text_center()
                .child(body.into()),
        );
    if let Some(hint) = hint {
        el = el.child(
            div()
                .mt(px(8.))
                .text_size(px(TEXT_XS))
                .font_family(t.mono.clone())
                .text_color(t.text3)
                .child(hint),
        );
    }
    el
}

/// A full-page section's opening: its name and one paragraph of what the
/// surface does. The icon echoes the rail, small and quiet.
pub fn section_header(
    glyph: IconName,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    v_flex()
        .mb(px(20.))
        .child(
            h_flex()
                .gap(px(8.))
                .text_size(px(crate::theme::TEXT_TITLE))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text1)
                .child(icon(glyph, 16., t.text3))
                .child(title.into()),
        )
        .child(
            div()
                .mt(px(4.))
                .max_w(px(760.))
                .text_size(px(TEXT_S))
                .line_height(gpui::relative(1.55))
                .text_color(t.text2)
                .child(description.into()),
        )
}

/// The plan step before a consequential action: a recessed panel that
/// states what will happen, in the workbench trays' vocabulary.
pub fn plan_panel(cx: &App) -> Div {
    let t = theme(cx);
    v_flex()
        .py(px(12.))
        .px(px(16.))
        .bg(t.bg_well)
        .border_1()
        .border_color(t.border2)
        .rounded(px(crate::theme::RADIUS_L))
}

/// A plan panel's heading: what will be done, and to what.
pub fn plan_title(
    action: impl Into<SharedString>,
    context: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    h_flex()
        .gap(px(8.))
        .min_w_0()
        .text_size(px(TEXT_M))
        .child(
            div()
                .flex_shrink_0()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text1)
                .child(action.into()),
        )
        .child(
            div()
                .min_w_0()
                .text_color(t.text3)
                .child(truncated(context.into())),
        )
}

/// What the action will do, one bullet per consequence.
pub fn consequences<T: Into<SharedString>>(lines: impl IntoIterator<Item = T>, cx: &App) -> Div {
    let t = theme(cx);
    let mut list = v_flex()
        .mt(px(8.))
        .gap(px(2.))
        .text_size(px(TEXT_S))
        .line_height(gpui::relative(1.5))
        .text_color(t.text2);
    for line in lines {
        list = list.child(
            div()
                .flex()
                .flex_row()
                .gap(px(8.))
                .child(div().flex_shrink_0().text_color(t.text3).child("•"))
                .child(div().flex_1().min_w_0().child(line.into())),
        );
    }
    list
}

/// A plan panel's closing row: the confirm hint, an inline error when the
/// action was refused, then the buttons.
pub fn plan_actions(
    hint: impl Into<SharedString>,
    error: Option<&str>,
    buttons: impl IntoIterator<Item = Btn>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    let mut row = h_flex().gap(px(8.)).mt(px(12.)).child(
        div()
            .flex_1()
            .min_w_0()
            .text_size(px(crate::theme::TEXT_XS))
            .text_color(t.text3)
            .child(hint.into()),
    );
    if let Some(error) = error {
        row = row.child(
            div()
                .min_w_0()
                .max_w(px(340.))
                .text_size(px(crate::theme::TEXT_XS))
                .text_color(t.danger)
                .child(truncated(error.to_owned())),
        );
    }
    row.children(buttons)
}

/// A sentence with inline emphasis: `**strong**` reads in full ink and
/// weight (ids, names), `~~quiet~~` recedes (quoted titles). The text
/// wraps like any paragraph.
pub fn rich(markup: &str, cx: &App) -> gpui::StyledText {
    let t = theme(cx);
    let mut text = String::with_capacity(markup.len());
    let mut highlights = Vec::new();
    let mut strong: Option<usize> = None;
    let mut quiet: Option<usize> = None;
    let mut rest = markup;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            match strong.take() {
                Some(start) => highlights.push((
                    start..text.len(),
                    gpui::HighlightStyle {
                        color: Some(t.text1),
                        font_weight: Some(FontWeight::SEMIBOLD),
                        ..Default::default()
                    },
                )),
                None => strong = Some(text.len()),
            }
            rest = after;
        } else if let Some(after) = rest.strip_prefix("~~") {
            match quiet.take() {
                Some(start) => highlights.push((
                    start..text.len(),
                    gpui::HighlightStyle {
                        color: Some(t.text3),
                        ..Default::default()
                    },
                )),
                None => quiet = Some(text.len()),
            }
            rest = after;
        } else {
            let ch = rest.chars().next().unwrap();
            text.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    highlights.sort_by_key(|(range, _)| range.start);
    gpui::StyledText::new(text).with_highlights(highlights)
}

/// A floating surface pinned to where this element sits in its parent:
/// drawn above everything else and kept inside the window. Put it in an
/// absolutely positioned wrapper next to the trigger.
pub fn floating(corner: gpui::Corner, content: impl IntoElement) -> gpui::Deferred {
    gpui::deferred(
        gpui::anchored()
            .anchor(corner)
            .snap_to_window_with_margin(px(8.))
            .child(content),
    )
    .with_priority(1)
}

/// Open/closed state for a popover whose trigger toggles it. A press on
/// the trigger while the popover is open first dismisses it (the press
/// lands outside the popover); the toggle that follows must not reopen it.
#[derive(Default)]
pub struct PopoverState {
    open: bool,
    closed_at: Option<std::time::Instant>,
}

impl PopoverState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            self.closed_at = Some(std::time::Instant::now());
        }
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else if self
            .closed_at
            .is_none_or(|at| at.elapsed() > std::time::Duration::from_millis(250))
        {
            self.open = true;
        }
    }
}

struct TooltipView {
    text: SharedString,
}

impl Render for TooltipView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        // Offset off the pointer so the tip never sits under it.
        div().pt(px(14.)).pl(px(6.)).child(
            div()
                .bg(t.bg2)
                .border_1()
                .border_color(t.border1)
                .rounded(px(RADIUS_S))
                .shadow(t.popover_shadow())
                .px(px(7.))
                .py(px(3.))
                .max_w(px(360.))
                .text_size(px(TEXT_XS))
                .text_color(t.text1)
                .font_family(crate::theme::FONT_UI)
                .child(self.text.clone()),
        )
    }
}

/// A plain text tooltip, for `.tooltip(...)`.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_window, cx| {
        let text = text.clone();
        cx.new(|_| TooltipView { text }).into()
    }
}

/// Stops a mouse press from reaching whatever is underneath (popover
/// bodies sitting over a dismiss layer).
pub trait SwallowClicks: Sized {
    fn swallow_clicks(self) -> Self;
}

impl SwallowClicks for Div {
    fn swallow_clicks(self) -> Self {
        self.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
    }
}

/// Compact age for dense graph rows: "now", "5m", "3h", "2d", "5w", "8mo",
/// "2y".
pub fn short_age(timestamp: &str) -> String {
    short_age_at(timestamp, chrono::Utc::now())
}

pub fn short_age_at(timestamp: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return String::new();
    };
    let minutes = (now - at.with_timezone(&chrono::Utc)).num_minutes();
    if minutes < 1 {
        return "now".into();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        format!("{days}d")
    } else if days < 60 {
        format!("{}w", days / 7)
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

/// "a few seconds ago", "5 minutes ago", "3 hours ago", "2 days ago" — a
/// sentence-shaped age for an RFC 3339 timestamp.
pub fn from_now(timestamp: &str) -> String {
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return String::new();
    };
    let seconds = (chrono::Utc::now() - at.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    let unit = |n: i64, one: &str, many: &str| {
        if n <= 1 {
            format!("{one} ago")
        } else {
            format!("{n} {many} ago")
        }
    };
    match seconds {
        0..=44 => "a few seconds ago".into(),
        45..=89 => "a minute ago".into(),
        90..=2699 => format!("{} minutes ago", (seconds + 30) / 60),
        2700..=5399 => "an hour ago".into(),
        5400..=79199 => format!("{} hours ago", (seconds + 1800) / 3600),
        79200..=129599 => "a day ago".into(),
        129600..=2246399 => format!("{} days ago", (seconds + 43200) / 86400),
        2246400..=3887999 => "a month ago".into(),
        3888000..=29807999 => unit(seconds / 2_592_000, "a month", "months"),
        _ => unit(seconds / 31_536_000, "a year", "years"),
    }
}

/// Reports where an element landed: lay it over (or inside) the element
/// whose bounds a later event handler needs.
pub fn bounds_probe(
    cell: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
) -> impl IntoElement {
    gpui::canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
        .absolute()
        .size_full()
}

/// A full local date and time, for tooltips.
pub fn absolute_time(timestamp: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%b %-d, %Y at %-I:%M %p")
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_step_through_units() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-10T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(short_age_at("2026-06-10T11:15:00Z", now), "45m");
        assert_eq!(short_age_at("2026-06-08T12:00:00Z", now), "2d");
        assert_eq!(short_age_at("2026-05-01T12:00:00Z", now), "5w");
        assert_eq!(short_age_at("2025-10-10T12:00:00Z", now), "8mo");
        assert_eq!(short_age_at("2024-06-10T12:00:00Z", now), "2y");
    }
}
