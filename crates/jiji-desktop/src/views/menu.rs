//! The context menu: a native-feeling panel at the pointer, with one level
//! of flyout submenu. Opaque, one hairline, instant.
//!
//! The menu never takes focus: keys are intercepted while it is open, so a
//! text field under it keeps its caret and selection (cut and paste act on
//! exactly what was selected) and nothing behind it sees a blur.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    anchored, deferred, div, point, prelude::*, px, Action, Anchor, AnyElement, App, Bounds,
    Context, Entity, FocusHandle, Keystroke, MouseDownEvent, Pixels, Point, Subscription, Window,
};

use crate::app::AppModel;
use crate::logic::menu::{ClipboardOp, MenuAction, MenuItem, MenuSection};
use crate::theme::{theme, RADIUS_S, TEXT_M, TEXT_S};
use crate::ui::a11y::spoken;
use crate::ui::{bounds_probe, icon, input, popover, truncated, IconName, NodeExt as _};

const ITEM_H: f32 = 26.;
const PAD: f32 = 4.;
/// A separator's hairline plus its margins.
const SEP_H: f32 = 9.;
/// The widest a flyout gets; decides which side of its parent it opens on.
const SUB_MAX_W: f32 = 320.;

struct Row {
    item: MenuItem,
    section: usize,
}

fn flatten(sections: &[MenuSection]) -> Vec<Row> {
    sections
        .iter()
        .enumerate()
        .flat_map(|(section, items)| {
            items.iter().map(move |item| Row {
                item: item.clone(),
                section,
            })
        })
        .collect()
}

/// The next enabled row from `current` in the direction of `delta`,
/// wrapping. With nothing highlighted, down starts at the first row and up
/// at the last.
fn step(rows: &[Row], current: Option<usize>, delta: isize) -> Option<usize> {
    let len = rows.len() as isize;
    if len == 0 {
        return None;
    }
    let mut at = match current {
        Some(at) => at as isize,
        None if delta > 0 => -1,
        None => 0,
    };
    for _ in 0..len {
        at = (at + delta).rem_euclid(len);
        if !rows[at as usize].item.disabled {
            return Some(at as usize);
        }
    }
    current
}

/// Where a row's top edge sits inside its panel.
fn row_top(rows: &[Row], index: usize) -> f32 {
    let separators = (1..=index)
        .filter(|&ix| rows[ix - 1].section != rows[ix].section)
        .count();
    PAD + index as f32 * ITEM_H + separators as f32 * SEP_H
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Top,
    Sub,
}

/// The open flyout: which top-level row spawned it.
struct Sub {
    row: usize,
    rows: Vec<Row>,
    active: Option<usize>,
}

pub struct ContextMenu {
    app: Entity<AppModel>,
    position: Point<Pixels>,
    rows: Vec<Row>,
    active: Option<usize>,
    sub: Option<Sub>,
    /// What had focus when the menu opened; clipboard items act on it.
    field: Option<FocusHandle>,
    panel: Rc<Cell<Bounds<Pixels>>>,
    _keys: Subscription,
}

impl ContextMenu {
    pub fn new(
        position: Point<Pixels>,
        sections: &[MenuSection],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let this = cx.weak_entity();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            let _ = this.update(cx, |this, cx| this.on_key(&event.keystroke, window, cx));
        });
        Self {
            app: crate::app::app(cx),
            position,
            rows: flatten(sections),
            active: None,
            sub: None,
            field: window.focused(cx),
            panel: Rc::default(),
            _keys: keys,
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| {
            model.close_menu(cx);
        });
    }

    fn open_sub(&mut self, index: usize, highlight_first: bool, cx: &mut Context<Self>) {
        let Some(children) = self.rows.get(index).map(|row| &row.item.children) else {
            return;
        };
        if children.is_empty() {
            if self.sub.take().is_some() {
                cx.notify();
            }
            return;
        }
        if self.sub.as_ref().is_some_and(|sub| sub.row == index) && !highlight_first {
            return;
        }
        let rows = flatten(children);
        let active = highlight_first.then(|| step(&rows, None, 1)).flatten();
        self.sub = Some(Sub {
            row: index,
            rows,
            active,
        });
        cx.notify();
    }

    fn on_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        // App shortcuts dismiss the menu and carry on to their own handler.
        if keystroke.modifiers.platform || keystroke.modifiers.control {
            self.close(cx);
            return;
        }
        // Everything else stops here, so a focused field under the menu
        // (the revset form, say) never receives a stray key or a submit.
        cx.stop_propagation();
        match keystroke.key.as_str() {
            "escape" => {
                if self.sub.take().is_some() {
                    cx.notify();
                } else {
                    self.close(cx);
                }
            }
            "down" | "up" => {
                let delta = if keystroke.key == "down" { 1 } else { -1 };
                match &mut self.sub {
                    Some(sub) => sub.active = step(&sub.rows, sub.active, delta),
                    None => self.active = step(&self.rows, self.active, delta),
                }
                cx.notify();
            }
            "right" => {
                if let (Some(active), None) = (self.active, &self.sub) {
                    self.open_sub(active, true, cx);
                }
            }
            "left" => {
                if self.sub.take().is_some() {
                    cx.notify();
                }
            }
            "enter" | "space" => {
                let picked = match &self.sub {
                    Some(sub) => sub.active.map(|ix| (Level::Sub, ix)),
                    None => self.active.map(|ix| (Level::Top, ix)),
                };
                match picked {
                    Some((level, index)) => self.run(level, index, window, cx),
                    None if self.sub.is_none() => self.close(cx),
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn run(&mut self, level: Level, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let row = match (level, &self.sub) {
            (Level::Top, _) => self.rows.get(index),
            (Level::Sub, Some(sub)) => sub.rows.get(index),
            (Level::Sub, None) => None,
        };
        let Some(item) = row.map(|row| &row.item) else {
            return;
        };
        if item.disabled {
            return;
        }
        if !item.children.is_empty() {
            if level == Level::Top {
                self.open_sub(index, true, cx);
            }
            return;
        }
        let Some(action) = item.action.clone() else {
            return;
        };
        let field = self.field.clone();
        self.close(cx);
        match action {
            MenuAction::Clipboard(op) => {
                let Some(field) = field else {
                    return;
                };
                let action: Box<dyn Action> = match op {
                    ClipboardOp::Cut => Box::new(input::Cut),
                    ClipboardOp::Copy => Box::new(input::Copy),
                    ClipboardOp::Paste => Box::new(input::Paste),
                    ClipboardOp::SelectAll => Box::new(input::SelectAll),
                };
                window.focus(&field, cx);
                field.dispatch_action(action.as_ref(), window, cx);
            }
            MenuAction::Spell(choice) => {
                if let Some(field) = field {
                    window.focus(&field, cx);
                    field.dispatch_action(&input::Spell(choice), window, cx);
                }
            }
            action => self
                .app
                .update(cx, |model, cx| model.run_action(action, cx)),
        }
    }

    fn render_row(
        &self,
        row: &Row,
        index: usize,
        level: Level,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let item = &row.item;
        let highlighted = match (level, &self.sub) {
            (Level::Top, sub) => {
                self.active == Some(index) || sub.as_ref().is_some_and(|sub| sub.row == index)
            }
            (Level::Sub, Some(sub)) => sub.active == Some(index),
            (Level::Sub, None) => false,
        } && !item.disabled;
        let (bg, fg, quiet) = match (highlighted, item.danger, item.disabled) {
            (_, _, true) => (None, t.text3, t.text3),
            (true, true, _) => (Some(t.danger), gpui::white(), gpui::white()),
            (true, false, _) => (Some(t.accent), t.accent_contrast, t.accent_contrast),
            (false, true, _) => (None, t.danger, t.danger),
            (false, false, _) => (None, t.text1, t.text3),
        };
        let id = match level {
            Level::Top => ("menu-item", index),
            Level::Sub => ("submenu-item", index),
        };
        let (disabled, opens) = (item.disabled, !item.children.is_empty());
        let mut el = div()
            .id(id)
            .role(gpui::Role::MenuItem)
            .aria_label(item.label.clone())
            .when_some(item.shortcut, |el, keys| el.aria_keyshortcuts(keys))
            .a11y(move |node| {
                if disabled {
                    node.set_disabled();
                }
                if opens {
                    node.set_has_popup(gpui::accesskit::HasPopup::Menu);
                }
            })
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .h(px(ITEM_H))
            .flex_shrink_0()
            .px(px(8.))
            .rounded(px(RADIUS_S))
            .text_size(px(TEXT_M))
            .text_color(fg)
            .when_some(bg, |el, bg| el.bg(bg))
            .child(
                div()
                    .w(px(16.))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .children(item.icon.map(|glyph| icon(glyph, 13., quiet))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(item.strong, |el| el.font_weight(gpui::FontWeight::SEMIBOLD))
                    .child(truncated(item.label.clone())),
            );
        if !item.children.is_empty() {
            el = el.child(icon(IconName::ChevronRight, 12., quiet));
        } else if let Some(keys) = item.shortcut {
            el = el.child(
                div()
                    .flex_shrink_0()
                    .ml(px(12.))
                    .text_size(px(TEXT_S))
                    .text_color(quiet)
                    .child(keys),
            );
        }
        el.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            match (level, *hovered) {
                (Level::Top, true) => {
                    this.active = Some(index);
                    this.open_sub(index, false, cx);
                }
                // Leaving a row that owns the open flyout keeps it lit.
                (Level::Top, false) => {
                    let owns_sub = this.sub.as_ref().is_some_and(|sub| sub.row == index);
                    if this.active == Some(index) && !owns_sub {
                        this.active = None;
                    }
                }
                (Level::Sub, true) => {
                    if let Some(sub) = &mut this.sub {
                        sub.active = Some(index);
                    }
                }
                (Level::Sub, false) => {}
            }
            cx.notify();
        }))
        .on_click(cx.listener(move |this, _, window, cx| this.run(level, index, window, cx)))
        .into_any_element()
    }

    fn render_panel(
        &self,
        rows: &[Row],
        level: Level,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let t = theme(cx);
        let mut panel = popover(cx)
            .id(match level {
                Level::Top => "context-menu",
                Level::Sub => "context-submenu",
            })
            .role(gpui::Role::Menu)
            .aria_label(match level {
                Level::Top => "Context menu",
                Level::Sub => "Submenu",
            })
            .flex()
            .flex_col()
            .p(px(PAD))
            .min_w(px(if level == Level::Top { 220. } else { 190. }))
            .max_w(px(SUB_MAX_W))
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation());
        for (index, row) in rows.iter().enumerate() {
            if index > 0 && rows[index - 1].section != row.section {
                panel = panel.child(
                    div()
                        .h(px(1.))
                        .flex_shrink_0()
                        .my(px(4.))
                        .mx(px(8.))
                        .bg(t.border2),
                );
            }
            panel = panel.child(self.render_row(row, index, level, cx));
        }
        panel
    }
}

impl Render for ContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The menu leaves focus where it was, so the item the arrows reach
        // is read out.
        let highlighted = match &self.sub {
            Some(sub) => sub.active.and_then(|ix| sub.rows.get(ix)),
            None => self.active.and_then(|ix| self.rows.get(ix)),
        }
        .map(|row| row.item.label.to_string())
        .unwrap_or_default();
        let panel = self
            .render_panel(&self.rows, Level::Top, cx)
            .child(bounds_probe(self.panel.clone()))
            .child(spoken("menu-highlight", highlighted));
        // The flyout hugs its parent row: to the right of the panel, or to
        // its left when the right side runs out.
        let flyout = self.sub.as_ref().map(|sub| {
            let panel = self.panel.get();
            let top = panel.top() + px(row_top(&self.rows, sub.row) - PAD - 1.);
            let fits_right =
                panel.right() + px(2. + SUB_MAX_W + 8.) <= window.viewport_size().width;
            let (corner, x) = if fits_right {
                (Anchor::TopLeft, panel.right() + px(2.))
            } else {
                (Anchor::TopRight, panel.left() - px(2.))
            };
            anchored()
                .anchor(corner)
                .position(point(x, top))
                .snap_to_window_with_margin(px(8.))
                .child(self.render_panel(&sub.rows, Level::Sub, cx))
        });
        // One layer above everything: a press or a scroll anywhere outside
        // the panels dismisses, and nothing under it hovers meanwhile.
        deferred(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .on_any_mouse_down(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.close(cx);
                }))
                .on_scroll(cx.listener(|this, _, _, cx| this.close(cx)))
                .child(anchored().position(self.position).child(panel))
                .children(flyout),
        )
        .with_priority(3)
    }
}

type RightClick = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// Right-click handlers for the things that carry their own menu wherever
/// they appear. Each claims the click, so the surface behind stays quiet.
pub fn change_menu(id: impl Into<String>) -> RightClick {
    let id = id.into();
    Box::new(move |event, _, cx| {
        cx.stop_propagation();
        crate::app::app(cx).update(cx, |model, cx| {
            model.open_change_menu(&id, event.position, cx)
        });
    })
}

pub fn bookmark_menu(name: impl Into<String>, remote: Option<String>) -> RightClick {
    let name = name.into();
    Box::new(move |event, _, cx| {
        cx.stop_propagation();
        crate::app::app(cx).update(cx, |model, cx| {
            model.open_bookmark_menu(&name, remote.as_deref(), event.position, cx)
        });
    })
}

pub fn pr_menu(number: u64) -> RightClick {
    Box::new(move |event, _, cx| {
        cx.stop_propagation();
        crate::app::app(cx).update(cx, |model, cx| {
            model.open_pr_menu(number, event.position, cx)
        });
    })
}

/// The menu layer for the model's open menu.
pub fn build(
    app: &Entity<AppModel>,
    window: &mut Window,
    cx: &mut App,
) -> Option<(u64, Entity<ContextMenu>)> {
    let open = app.read(cx).menu.as_ref()?;
    let (seq, position, sections) = (open.seq, open.position, open.sections.clone());
    Some((
        seq,
        cx.new(|cx| ContextMenu::new(position, &sections, window, cx)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(spec: &[(usize, bool)]) -> Vec<Row> {
        spec.iter()
            .map(|&(section, disabled)| Row {
                item: MenuItem {
                    id: "x",
                    label: String::new(),
                    icon: None,
                    shortcut: None,
                    danger: false,
                    disabled,
                    strong: false,
                    action: None,
                    children: Vec::new(),
                },
                section,
            })
            .collect()
    }

    #[test]
    fn stepping_skips_disabled_rows_and_wraps() {
        let rows = rows(&[(0, true), (0, false), (1, true), (1, false)]);
        assert_eq!(step(&rows, None, 1), Some(1));
        assert_eq!(step(&rows, None, -1), Some(3));
        assert_eq!(step(&rows, Some(1), 1), Some(3));
        assert_eq!(step(&rows, Some(3), 1), Some(1));
        assert_eq!(step(&rows, Some(1), -1), Some(3));
        assert_eq!(step(&[], None, 1), None);
    }

    #[test]
    fn rows_sit_below_the_separators_before_them() {
        let rows = rows(&[(0, false), (0, false), (1, false), (2, false)]);
        assert_eq!(row_top(&rows, 0), PAD);
        assert_eq!(row_top(&rows, 1), PAD + ITEM_H);
        assert_eq!(row_top(&rows, 2), PAD + 2. * ITEM_H + SEP_H);
        assert_eq!(row_top(&rows, 3), PAD + 3. * ITEM_H + 2. * SEP_H);
    }
}
