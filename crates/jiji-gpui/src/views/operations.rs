//! Operations: everything that has happened to the repository, newest
//! first, with restore and revert as plan-then-confirm actions.

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, FocusHandle, Focusable, FontWeight,
    MouseButton, MouseDownEvent, ScrollHandle, SharedString, Window,
};
use jiji_core::snapshot::{OpEffectKind, OperationItem};

use super::ConfirmPlan;
use crate::app::{app, AppModel};
use crate::logic::ops::{
    clock_time, group_operations, is_root_op, now, op_kind, ops_since, OpKind, TimelineRow,
};
use crate::logic::text::{abbreviate_hashes, short_id, short_user_host};
use crate::shell::Dismiss;
use crate::theme::{theme, Theme, TEXT_M, TEXT_S, TEXT_XS};
use crate::ui::pinned;
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, consequences, group_head, h_flex, icon, label_box, node_mark, page_bar_marked,
    plan_actions, plan_panel, plan_title, scrollbar, tag, tooltip, truncated, v_flex, wrapped,
    IconName, Mark, Tabular as _, PAGE_PAD,
};

/// The clock gutter down the journal's leading edge.
const TIME_W: f32 = 40.;

/// An operation family's hue: the lane set, so the journal and the graph
/// share one palette.
pub fn op_hue(kind: OpKind, t: &Theme) -> gpui::Hsla {
    match kind {
        OpKind::Change => t.lane(0),
        OpKind::Remote => t.lane(1),
        OpKind::Rewrite => t.lane(2),
        OpKind::Undo => t.lane(3),
        OpKind::Bookmark => t.lane(4),
        OpKind::Snapshot | OpKind::Other => Theme::blend(t.text3, 0.85, t.text2),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpAction {
    Restore,
    Revert,
}

pub struct OperationsView {
    app: Entity<AppModel>,
    focus: FocusHandle,
    /// Keys (first op id) of the snapshot runs the user opened.
    expanded: HashSet<String>,
    /// One open plan panel across the whole timeline.
    confirm: Option<(String, OpAction)>,
    acting: bool,
    error: Option<String>,
    scroll: ScrollHandle,
}

impl Focusable for OperationsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl OperationsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let app = app(cx);
        cx.observe(&app, |_, _, cx| cx.notify()).detach();
        Self {
            app,
            focus: cx.focus_handle(),
            expanded: HashSet::new(),
            confirm: None,
            acting: false,
            error: None,
            scroll: ScrollHandle::new(),
        }
    }

    fn toggle(
        &mut self,
        op_id: &str,
        action: OpAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        let same = self
            .confirm
            .as_ref()
            .is_some_and(|(open, kind)| open == op_id && *kind == action);
        self.confirm = (!same).then(|| (op_id.to_owned(), action));
        window.focus(&self.focus);
        cx.notify();
    }

    fn run_confirmed(&mut self, cx: &mut Context<Self>) {
        let Some((op_id, action)) = self.confirm.clone() else {
            return;
        };
        if self.acting {
            return;
        }
        self.acting = true;
        self.error = None;
        cx.notify();
        let task = self.app.update(cx, |model, cx| {
            model.mutate(cx, move |backend, path| match action {
                OpAction::Restore => backend.restore_operation(path, &op_id),
                OpAction::Revert => backend.revert_operation(path, &op_id),
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.acting = false;
                match result {
                    Ok(_) => this.confirm = None,
                    Err(error) => this.error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.confirm.take().is_some() {
            self.error = None;
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    /// The timeline rail: a continuous line the glyphs sit on, jj op log
    /// style. `stub` is how far the upper segment reaches down.
    fn rail(
        &self,
        up: bool,
        down: bool,
        stub: f32,
        glyph: AnyElement,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let t = theme(cx);
        let line = Theme::blend(t.text3, 0.42, t.bg1);
        let mut rail = div()
            .relative()
            .flex_shrink_0()
            .w(px(22.))
            .flex()
            .justify_center();
        if up {
            rail = rail.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(10.))
                    .w(px(2.))
                    .h(px(stub))
                    .bg(line),
            );
        }
        if down {
            rail = rail.child(
                div()
                    .absolute()
                    .top(px(21.))
                    .bottom_0()
                    .left(px(10.))
                    .w(px(2.))
                    .bg(line),
            );
        }
        rail.child(glyph)
    }

    fn render_confirm(
        &self,
        op: &OperationItem,
        action: OpAction,
        restore_count: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let context = format!(
            "\u{201c}{}\u{201d}",
            if op.description.is_empty() {
                "root()".to_owned()
            } else {
                abbreviate_hashes(&op.description)
            }
        );
        let (title, lines, go) = match action {
            OpAction::Restore => (
                "Restore the repo to this point",
                vec![
                    match restore_count {
                        Some(count) if count > 0 => format!(
                            "{count} later operation{} unwind{} — the repo returns to exactly \
                             the state this operation left it in.",
                            if count == 1 { "" } else { "s" },
                            if count == 1 { "s" } else { "" }
                        ),
                        _ => "Everything recorded after this operation is unwound — the repo \
                              returns to exactly the state it left."
                            .to_owned(),
                    },
                    "Files on disk follow the restored working copy.".to_owned(),
                    "Nothing is lost: this records a new operation, and every undone operation \
                     stays in this log."
                        .to_owned(),
                ],
                if self.acting {
                    "Restoring…"
                } else {
                    "Restore"
                },
            ),
            OpAction::Revert => (
                "Revert this operation",
                vec![
                    if op.is_current {
                        "This is the latest operation, so reverting it is a plain undo.".to_owned()
                    } else {
                        "Only this operation is backed out — everything recorded after it stays."
                            .to_owned()
                    },
                    "Nothing is lost: reverting records a new operation, and reverting that \
                     brings this one back."
                        .to_owned(),
                ],
                if self.acting {
                    "Reverting…"
                } else {
                    "Revert"
                },
            ),
        };
        plan_panel(cx)
            .mt(px(8.))
            .mb(px(4.))
            .child(plan_title(title, context, cx))
            .child(consequences(lines, cx))
            .child(plan_actions(
                "⌘↵ to confirm",
                self.error.as_deref(),
                [
                    btn("op-cancel", "Cancel")
                        .disabled(self.acting)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm = None;
                            this.error = None;
                            cx.notify();
                        })),
                    btn("op-go", go)
                        .primary()
                        .disabled(self.acting)
                        .on_click(cx.listener(|this, _, _, cx| this.run_confirmed(cx))),
                ],
                cx,
            ))
    }

    fn render_op(
        &self,
        op: &OperationItem,
        index: usize,
        (up, down): (bool, bool),
        ops: &[OperationItem],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let clock = now();
        let quiet = op.is_snapshot && !op.is_current;
        // The root operation is the state before the repo existed: nothing
        // to revert, and restoring to it would empty the repo.
        let actionable = !is_root_op(op);
        // Restoring to the current operation is a no-op; reverting it is
        // undo.
        let can_restore = actionable && !op.is_current;
        let confirm = self
            .confirm
            .as_ref()
            .filter(|(open, _)| *open == op.id)
            .map(|(_, action)| *action);
        let group = SharedString::from(format!("op-{}", op.id));

        // Each operation is a station on the journal's line, colored by
        // what it did; the current one is where the repo is now.
        let hue = op_hue(op_kind(&op.description, op.is_snapshot), &t);
        let glyph = if op.is_current {
            div()
                .mt(px(8.))
                .p(px(1.))
                .rounded(px(9.))
                .bg(t.bg1)
                .child(node_mark(Mark::WorkingCopy, hue, 14.))
                .into_any_element()
        } else {
            let size = if quiet { 7. } else { 12. };
            div()
                .mt(px(if quiet { 12. } else { 10. }))
                .size(px(size))
                .rounded(px(size / 2.))
                .border(px(if quiet { 1.5 } else { 2.5 }))
                .border_color(if quiet {
                    Theme::fade(t.text3, 0.6)
                } else {
                    hue
                })
                .bg(t.bg1)
                .into_any_element()
        };

        // The whole description, on as many lines as it needs. (A flex
        // box of its own, so the paragraph inside is handed the width it
        // must wrap to.)
        let mut desc = div()
            .flex()
            .min_w(px(40.))
            .flex_shrink()
            .text_size(px(TEXT_M))
            .text_color(t.text1);
        if op.description.is_empty() {
            desc = desc
                .font_family(t.mono.clone())
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child("root()");
        } else {
            if op.is_current {
                desc = desc.font_weight(FontWeight::SEMIBOLD);
            }
            if quiet {
                desc = desc.text_color(t.text3);
            }
            desc = desc.child(wrapped(abbreviate_hashes(&op.description)));
        }
        let mut head = h_flex().gap(px(8.)).min_w_0().min_h(px(22.)).child(desc);
        if op.is_current {
            head = head.child(tag("current", t.accent_strong));
        }
        head = head.child(div().flex_1());
        if actionable {
            // Time-travel actions are hover-revealed so the journal stays
            // calm; an open panel keeps its row's on screen.
            let mut actions = h_flex().flex_shrink_0().gap(px(4.));
            if confirm.is_none() {
                actions = actions
                    .opacity(0.)
                    .group_hover(group.clone(), |style| style.opacity(1.));
            }
            if can_restore {
                let id = op.id.clone();
                actions = actions.child(
                    btn(("op-restore", index), "Restore")
                        .small()
                        .icon(IconName::Restore)
                        .armed(confirm == Some(OpAction::Restore))
                        .tooltip("Restore the repo to this point")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle(&id, OpAction::Restore, window, cx)
                        })),
                );
            }
            let id = op.id.clone();
            actions = actions.child(
                btn(("op-revert", index), "Revert")
                    .small()
                    .icon(IconName::Undo)
                    .armed(confirm == Some(OpAction::Revert))
                    .tooltip(if op.is_current {
                        "Undo this operation"
                    } else {
                        "Back out just this operation"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.toggle(&id, OpAction::Revert, window, cx)
                    })),
            );
            head = head.child(actions);
        }

        let mut meta = h_flex()
            .flex_wrap()
            .gap_x(px(8.))
            .gap_y(px(3.))
            .mt(px(if quiet { 0. } else { 1. }))
            .text_size(px(TEXT_S))
            .text_color(t.text3)
            .child(
                div()
                    .id(("op-id", index))
                    .font_family(t.mono.clone())
                    .text_color(t.text2)
                    .tooltip(tooltip(op.id.clone()))
                    .child(short_id(&op.id, 8).to_owned()),
            )
            .child(
                div()
                    .id(("op-user", index))
                    .max_w(px(220.))
                    .tooltip(tooltip(op.user.clone()))
                    .child(truncated(short_user_host(&op.user).to_owned())),
            );
        let chip = || label_box(t.text2, t.border2).gap(px(4.));
        for effect in &op.effects {
            let lead = match effect.kind {
                OpEffectKind::WorkingCopy => {
                    node_mark(Mark::WorkingCopy, t.working_copy, 9.).into_any_element()
                }
                OpEffectKind::RemoteBookmark => {
                    icon(IconName::Bookmark, 9., t.accent).into_any_element()
                }
                OpEffectKind::Bookmark => icon(IconName::Bookmark, 9., t.text3).into_any_element(),
            };
            meta = meta.child(chip().child(lead).child(effect.label.clone()));
        }
        if op.more_effects > 0 {
            meta = meta.child(
                chip()
                    .text_color(t.text3)
                    .child(format!("+{} more", op.more_effects)),
            );
        }

        let mut body = v_flex()
            .flex_1()
            .min_w_0()
            .pt(px(6.))
            .pb(px(7.))
            .child(head)
            .child(meta);
        if let Some(action) = confirm {
            body = body.child(self.render_confirm(op, action, ops_since(ops, &op.id), cx));
        }
        let id = op.id.clone();
        // The clock runs down the page's leading edge, the journal's own
        // gutter: when, then what.
        let time = div()
            .id(("op-time", index))
            .w(px(TIME_W))
            .flex_shrink_0()
            .pt(px(9.))
            .flex()
            .justify_end()
            .tabular()
            .text_size(px(TEXT_S))
            .text_color(if quiet { t.text3 } else { t.text2 })
            .tooltip(tooltip(crate::ui::absolute_time(&op.timestamp)))
            .child(clock_time(&op.timestamp, clock));
        div()
            .id(("op-row", index))
            .group(group)
            .flex()
            .flex_row()
            .gap(px(8.))
            .pl(px(PAGE_PAD))
            .pr(px(12.))
            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                app(cx).update(cx, |model, cx| {
                    model.open_operation_menu(&id, event.position, cx)
                });
            })
            .child(time)
            .child(self.rail(up, down, 11., glyph, cx))
            .child(body)
            .into_any_element()
    }
}

impl Render for OperationsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let Some(snapshot) = self.app.read(cx).snapshot().cloned() else {
            return div().into_any_element();
        };
        let ops = &snapshot.operations;
        let groups = group_operations(ops, now());
        // Block flow, not a flex column: a flex column asked for its
        // height sizes each row before stretching it to the column's
        // width, so a row whose text wraps would be measured unwrapped
        // and then squeezed, and a group would sit at its content's
        // width instead of the page's.
        let mut column = div().w_full().pb(px(32.));
        let mut index = 0;
        for group in &groups {
            // The day stays in view while its operations scroll under it.
            let heading = group_head(group.label.clone(), None, None, cx);
            let mut day = div();
            let last = group.rows.len().saturating_sub(1);
            for (i, row) in group.rows.iter().enumerate() {
                let (up, down) = (i > 0, i < last);
                match row {
                    TimelineRow::Op(op) => {
                        day = day.child(self.render_op(op, index, (up, down), ops, cx));
                        index += 1;
                    }
                    TimelineRow::Snapshots(run) => {
                        let key = run[0].id.clone();
                        let expanded = self.expanded.contains(&key);
                        // Collapsed snapshot runs read like jj's elided
                        // revisions rows.
                        let glyph = div()
                            .mt(px(4.))
                            .bg(t.bg1)
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_S))
                            .text_color(t.text3)
                            .child("~")
                            .into_any_element();
                        day = day.child(
                            div()
                                .id(SharedString::from(format!("op-run-{key}")))
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.))
                                .pl(px(PAGE_PAD + TIME_W + 8.))
                                .pr(px(12.))
                                .cursor_pointer()
                                .hover(|style| style.bg(t.bg_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.expanded.remove(&key) {
                                        this.expanded.insert(key.clone());
                                    }
                                    cx.notify();
                                }))
                                .child(self.rail(up, down || expanded, 7., glyph, cx).h(px(28.)))
                                .child(
                                    div()
                                        .py(px(5.))
                                        .text_size(px(TEXT_S))
                                        .text_color(t.text3)
                                        .child(format!("{} working-copy snapshots", run.len())),
                                )
                                .child(icon(
                                    if expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    },
                                    11.,
                                    t.text3,
                                )),
                        );
                        if expanded {
                            for (j, op) in run.iter().enumerate() {
                                let below = j < run.len() - 1 || down;
                                day = day.child(self.render_op(op, index, (true, below), ops, cx));
                                index += 1;
                            }
                        } else {
                            index += run.len();
                        }
                    }
                }
            }
            column = column.child(pinned(heading, day));
        }
        if ops.len() >= 50 {
            column = column.child(
                div()
                    .mt(px(12.))
                    .pl(px(PAGE_PAD + TIME_W + 38.))
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child("Showing the 50 most recent operations."),
            );
        }
        let mut root = div()
            .id("operations")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .on_action(cx.listener(|this, _: &ConfirmPlan, _, cx| this.run_confirmed(cx)));
        root = root.key_context(if self.confirm.is_some() {
            "Operations PlanPanel"
        } else {
            "Operations"
        });
        root.child(
            v_flex()
                .size_full()
                .child(page_bar_marked(
                    Some((
                        crate::app::Section::Operations.glyph(),
                        crate::app::Section::Operations.hue(&theme(cx)),
                    )),
                    "Operations",
                    Some(ops.len().to_string()),
                    cx,
                ))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            div()
                                .id("operations-scroll")
                                .size_full()
                                .scroll_y()
                                .track_scroll(&self.scroll)
                                .child(column),
                        )
                        .child(scrollbar("operations-scrollbar", &self.scroll)),
                ),
        )
        .into_any_element()
    }
}
