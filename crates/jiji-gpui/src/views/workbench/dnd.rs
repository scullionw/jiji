//! The live drag session behind graph drag-and-drop: a press on a row, a
//! small movement threshold so clicks stay clicks, retargeting from row
//! geometry while the pointer moves, ⌥ toggling the move-alone scope
//! mid-drag, Esc cancelling, and edge auto-scroll so long graphs can be
//! reparented end to end. Releasing runs the same rebase/move mutations as
//! the explicit rebase panel — breadcrumb, Undo and selection-follow
//! included.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui::{
    anchored, canvas, deferred, div, point, prelude::*, px, AnyElement, Bounds, Context,
    FontWeight, MouseMoveEvent, Pixels, Point, Window,
};
use jiji_core::snapshot::NodeKind;

use super::WorkbenchView;
use crate::app::DragHint;
use crate::logic::dnd::{can_drag, drag_affected_ids, plan_drop, DropOp, DropPlan};
use crate::logic::inspect::{descendants_of, find_node, summary_of};
use crate::theme::{theme, Theme, RADIUS_L, RADIUS_S, TEXT_S, TEXT_XS};
use crate::ui::{h_flex, truncated, v_flex};

/// Points of travel before a press becomes a drag instead of a click.
const THRESHOLD: f32 = 5.;
/// The edge band that auto-scrolls the pane, and the per-frame speed cap.
const EDGE: f32 = 36.;
const MAX_STEP: f32 = 14.;

/// Each drawn row's id and where it was painted.
pub(super) type RowPlaces = Rc<RefCell<Vec<(String, Bounds<Pixels>)>>>;

#[derive(Default)]
pub(super) struct DragState {
    pressed: Option<(String, Point<Pixels>)>,
    pub(super) active: bool,
    source: String,
    target: Option<String>,
    /// ⌥ held: only this change moves; descendants stay behind.
    alone: bool,
    position: Point<Pixels>,
    plan: Option<DropPlan>,
    /// One drop mutation at a time; new presses wait for the refresh.
    busy: bool,
    /// Where each drawn row sits, recorded as the rows paint.
    pub(super) rows: RowPlaces,
}

/// How one row answers the gesture (or a plan panel's scrub) in progress.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RowWash {
    /// The row in hand.
    Source,
    /// The prospective new parent under the pointer, or a plan panel's
    /// picked destination.
    Target { allowed: bool },
    /// A row the pending rewrite would touch.
    Affected,
}

/// What the rows read while a drag or a plan panel is scrubbing.
#[derive(Default)]
pub(super) struct Washes {
    source: Option<String>,
    target: Option<(String, bool)>,
    affected: HashSet<String>,
}

impl Washes {
    pub(super) fn of(&self, id: &str) -> Option<RowWash> {
        if self.source.as_deref() == Some(id) {
            Some(RowWash::Source)
        } else if let Some((_, allowed)) = self.target.as_ref().filter(|(target, _)| target == id) {
            Some(RowWash::Target { allowed: *allowed })
        } else if self.affected.contains(id) {
            Some(RowWash::Affected)
        } else {
            None
        }
    }
}

/// Records where a row was painted, for retargeting.
pub(super) fn row_probe(rows: RowPlaces, id: String) -> impl IntoElement {
    canvas(
        move |bounds, _, _| rows.borrow_mut().push((id.clone(), bounds)),
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

impl WorkbenchView {
    /// The rows' wash for this frame: the drag in hand, else the plan a
    /// header panel is scrubbing.
    pub(super) fn washes(&self) -> Washes {
        if self.drag.active {
            let moves_alone = match &self.drag.plan {
                Some(DropPlan::Allowed { op, .. }) => *op == DropOp::Move,
                _ => self.drag.alone,
            };
            return Washes {
                source: Some(self.drag.source.clone()),
                target: self.drag.target.clone().map(|target| {
                    let allowed = self.drag.plan.as_ref().is_some_and(DropPlan::allowed);
                    (target, allowed)
                }),
                affected: drag_affected_ids(&self.snapshot, &self.drag.source, moves_alone)
                    .into_iter()
                    .collect(),
            };
        }
        match self.rewrite_preview() {
            Some((affected, destination)) => Washes {
                source: None,
                target: destination.map(|id| (id, true)),
                affected,
            },
            None => Washes::default(),
        }
    }

    /// A left press on a row: the start of a click, or of a drag.
    pub(super) fn press_row(&mut self, id: &str, position: Point<Pixels>) {
        if self.drag.busy || self.drag.active {
            return;
        }
        if find_node(&self.snapshot, id).is_some_and(can_drag) {
            self.drag.pressed = Some((id.to_owned(), position));
        }
    }

    fn publish_hint(&mut self, cx: &mut Context<Self>) {
        let hint = self.drag.active.then(|| match &self.drag.plan {
            Some(DropPlan::Allowed { summary, .. }) => DragHint {
                allowed: Some(true),
                text: summary.clone(),
            },
            Some(DropPlan::Refused(reason)) => DragHint {
                allowed: Some(false),
                text: reason.clone(),
            },
            None => DragHint {
                allowed: None,
                text: "Drop onto the change that becomes the new parent".to_owned(),
            },
        });
        self.app.update(cx, |model, cx| {
            if model.drag_hint != hint {
                model.drag_hint = hint;
                cx.notify();
            }
        });
    }

    fn replan(&mut self, cx: &mut Context<Self>) {
        self.drag.plan =
            self.drag.target.as_ref().map(|target| {
                plan_drop(&self.snapshot, &self.drag.source, target, self.drag.alone)
            });
        self.publish_hint(cx);
        cx.notify();
    }

    fn retarget(&mut self, cx: &mut Context<Self>) {
        let at = self.drag.position;
        // Only rows inside the pane's own viewport are targets.
        let pane = self.scroll.bounds();
        let target = pane
            .contains(&at)
            .then(|| {
                self.drag
                    .rows
                    .borrow()
                    .iter()
                    .find(|(_, bounds)| bounds.contains(&at))
                    .map(|(id, _)| id.clone())
            })
            .flatten();
        if target != self.drag.target {
            self.drag.target = target;
            self.replan(cx);
        }
    }

    fn reset_drag(&mut self, cx: &mut Context<Self>) {
        let was_active = self.drag.active;
        self.drag.pressed = None;
        self.drag.active = false;
        self.drag.target = None;
        self.drag.plan = None;
        if was_active {
            self.publish_hint(cx);
            cx.notify();
        }
    }

    /// Esc cancels the gesture only; whether there was one to cancel.
    pub(super) fn cancel_drag(&mut self, cx: &mut Context<Self>) -> bool {
        let live = self.drag.active || self.drag.pressed.is_some();
        self.reset_drag(cx);
        live
    }

    pub(super) fn drag_row_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A release outside the window never delivers the mouse-up; the
        // next movement shows the button is up: cancel rather than guess
        // at a drop that already happened elsewhere.
        if event.pressed_button != Some(gpui::MouseButton::Left) {
            if self.drag.active || self.drag.pressed.is_some() {
                self.reset_drag(cx);
            }
            return;
        }
        if let (Some((id, start)), false) = (self.drag.pressed.clone(), self.drag.active) {
            let delta = event.position - start;
            let travel = (f32::from(delta.x).powi(2) + f32::from(delta.y).powi(2)).sqrt();
            if travel < THRESHOLD {
                return;
            }
            self.drag.source = id;
            self.drag.active = true;
            self.drag.alone = event.modifiers.alt;
            self.drag.target = None;
            self.drag.plan = None;
            self.publish_hint(cx);
        }
        if !self.drag.active {
            return;
        }
        self.drag.position = event.position;
        if self.drag.alone != event.modifiers.alt {
            self.drag.alone = event.modifiers.alt;
            self.replan(cx);
        }
        self.retarget(cx);
        cx.notify();
    }

    /// ⌥ pressed or released with the pointer at rest.
    pub(super) fn drag_modifiers(&mut self, alt: bool, cx: &mut Context<Self>) {
        if self.drag.active && self.drag.alone != alt {
            self.drag.alone = alt;
            self.replan(cx);
        }
    }

    /// The release: the confirmation of the plan on the card.
    pub(super) fn drop_row(&mut self, cx: &mut Context<Self>) {
        if !self.drag.active {
            self.drag.pressed = None;
            return;
        }
        let (source, target, alone) = (
            self.drag.source.clone(),
            self.drag.target.clone(),
            self.drag.alone,
        );
        self.reset_drag(cx);
        let Some(target) = target else {
            return;
        };
        // Recomputed at release, so a snapshot that refreshed mid-drag
        // cannot execute a stale plan.
        let DropPlan::Allowed { op, .. } = plan_drop(&self.snapshot, &source, &target, alone)
        else {
            return;
        };
        self.drag.busy = true;
        // No panel owns a drop, so failures land in the status bar's
        // error slot, like palette-launched mutations.
        let task = self.app.update(cx, |model, cx| {
            let path = model.repo_path();
            let task = model.mutate(cx, move |backend, repo| match op {
                DropOp::Move => backend.move_change(repo, &source, &target),
                DropOp::Rebase => backend.rebase_change(repo, &source, &target),
            });
            (path, task)
        });
        cx.spawn(async move |this, cx| {
            let (path, task) = task;
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.drag.busy = false;
                if let Err(error) = result {
                    this.app.update(cx, |model, cx| {
                        if model.repo_path() == path {
                            model.set_error(error.message, cx);
                        }
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Auto-scroll while the pointer sits in the pane's edge bands; rows
    /// sliding under a resting pointer re-pick the target. Runs once per
    /// frame while a row is in hand.
    pub(super) fn drag_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.drag.active {
            return;
        }
        window.request_animation_frame();
        let pane = self.scroll.bounds();
        let at = self.drag.position;
        if at.x < pane.left() || at.x > pane.right() {
            return;
        }
        let (y, top, bottom) = (
            f32::from(at.y),
            f32::from(pane.top()),
            f32::from(pane.bottom()),
        );
        let step = if y < top + EDGE {
            ((y - (top + EDGE)) / 4.).max(-MAX_STEP)
        } else if y > bottom - EDGE {
            ((y - (bottom - EDGE)) / 4.).min(MAX_STEP)
        } else {
            0.
        };
        if step == 0. {
            return;
        }
        let offset = self.scroll.offset();
        let limit = self.scroll.max_offset().y;
        let next = (offset.y - px(step)).clamp(-limit, px(0.));
        if next != offset.y {
            self.scroll.set_offset(point(offset.x, next));
            self.retarget(cx);
        }
    }

    /// The gesture's plan card: what is being dragged and what releasing
    /// right here would do, following the pointer. The drop is the
    /// confirmation, so the card carries the same consequences the rebase
    /// panel states before its ⌘↵.
    pub(super) fn render_drag_card(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.drag.active {
            return None;
        }
        let t = theme(cx);
        let source = find_node(&self.snapshot, &self.drag.source)?;
        let descendants = descendants_of(&self.snapshot, &source.id).len();
        // The scope chip states what travels before a target is even
        // hovered; over a target the plan's op is authoritative.
        let alone = match &self.drag.plan {
            Some(DropPlan::Allowed { op, .. }) => *op == DropOp::Move,
            _ => self.drag.alone,
        };
        let (glyph, glyph_color) = match source.kind {
            NodeKind::WorkingCopy => ("@", t.working_copy),
            NodeKind::Mutable => ("○", t.text3),
            NodeKind::Immutable => ("◆", t.text3),
        };
        let (lead, rest) = source.id.split_at(source.id.len().min(2));
        let title = match summary_of(&source.description) {
            "" => "no description",
            title => title,
        };
        let mut held = h_flex()
            .gap(px(8.))
            .min_w_0()
            .text_size(px(TEXT_S))
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(t.mono.clone())
                    .text_color(glyph_color)
                    .child(glyph),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .font_family(t.mono.clone())
                    .text_color(t.text3)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text1)
                            .child(lead.to_owned()),
                    )
                    .child(rest.chars().take(6).collect::<String>()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(t.text2)
                    .child(truncated(title.to_owned())),
            );
        if descendants > 0 {
            let (fg, bg) = if alone {
                (t.warn, Theme::fade(t.warn, 0.14))
            } else {
                (t.accent_strong, t.accent_dim)
            };
            held = held.child(
                div()
                    .flex_shrink_0()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(RADIUS_S))
                    .bg(bg)
                    .text_size(px(TEXT_XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(fg)
                    .child(if alone {
                        "alone".to_owned()
                    } else {
                        format!(
                            "+{descendants} descendant{}",
                            if descendants == 1 { "" } else { "s" }
                        )
                    }),
            );
        }
        let plan_line = |text: String, color, weight| {
            div()
                .mt(px(8.))
                .pt(px(8.))
                .border_t_1()
                .border_color(t.border2)
                .text_size(px(TEXT_S))
                .font_weight(weight)
                .text_color(color)
                .child(text)
        };
        let mut card = v_flex()
            .w(px(320.))
            .p(px(12.))
            .bg(t.bg2)
            .border_1()
            .border_color(t.border1)
            .rounded(px(RADIUS_L))
            .shadow(t.popover_shadow())
            .child(held);
        card = match &self.drag.plan {
            Some(DropPlan::Allowed {
                summary,
                consequences,
                ..
            }) => {
                let mut card = card.child(plan_line(
                    summary.clone(),
                    t.accent_strong,
                    FontWeight::SEMIBOLD,
                ));
                if !consequences.is_empty() {
                    let mut list = v_flex()
                        .gap(px(2.))
                        .mt(px(4.))
                        .text_size(px(TEXT_S))
                        .text_color(t.text2);
                    for line in consequences {
                        list = list.child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(6.))
                                .child(div().flex_shrink_0().text_color(t.text3).child("•"))
                                .child(div().flex_1().min_w_0().child(line.clone())),
                        );
                    }
                    card = card.child(list);
                }
                card
            }
            Some(DropPlan::Refused(reason)) => {
                card.child(plan_line(reason.clone(), t.danger, FontWeight::SEMIBOLD))
            }
            None => card.child(plan_line(
                "Drop onto the change that becomes the new parent".to_owned(),
                t.text3,
                FontWeight::NORMAL,
            )),
        };
        let key = |text: &'static str| {
            div()
                .font_family(t.mono.clone())
                .text_color(t.text2)
                .child(text)
        };
        let mut hints = h_flex()
            .gap(px(5.))
            .mt(px(8.))
            .overflow_hidden()
            .text_size(px(TEXT_XS))
            .text_color(t.text3);
        if descendants > 0 {
            hints = hints
                .child(key("⌥"))
                .child(if self.drag.alone {
                    "brings descendants back"
                } else {
                    "moves only this change"
                })
                .child("·");
        }
        hints = hints
            .child(key("esc"))
            .child("cancels")
            .child("·")
            .child("undo lives in Operations");
        // Follow the pointer, clamped so the card never leaves the window.
        let viewport = window.viewport_size();
        let at = point(
            (self.drag.position.x + px(14.)).min(viewport.width - px(336.)),
            (self.drag.position.y + px(18.)).min(viewport.height - px(220.)),
        );
        Some(
            deferred(anchored().position(at).child(card.child(hints)))
                .with_priority(2)
                .into_any_element(),
        )
    }
}
