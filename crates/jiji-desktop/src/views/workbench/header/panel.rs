//! What every tray is built from: the recessed panel under the header,
//! its title, consequences and confirm row, and the destination list the
//! rebase and move-into trays share.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, App, Context, Div, FontWeight, Stateful, Window,
};
use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot};

use super::{kind_glyph, title_or_none, Tray};
use crate::logic::inspect::{find_node, CompareMode};
use crate::logic::text::short_id;
use crate::shell::Dismiss;
use crate::theme::{theme, Theme, RADIUS_M, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS};
use crate::ui::vertical_only;
use crate::ui::{
    btn, h_flex, input::field, keyboard_on, ref_label, rich, ring, tooltip, truncated, trunk_label,
    v_flex, NodeExt as _,
};
use crate::views::workbench::{TrayConfirm, TrayDown, TrayEnter, TrayUp, WorkbenchView};

impl WorkbenchView {
    /// The tray's panel: a dialog named `name` (an alert dialog when it
    /// asks before rewriting), tinted when what it asks for is destructive.
    /// When it holds a destination
    /// list it is a stop of its own, whose arrows travel the list.
    pub(super) fn tray(
        &self,
        danger: bool,
        name: impl Into<gpui::SharedString>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = theme(cx);
        let listed = match self.header.tray {
            Tray::Rebase | Tray::Compare => true,
            Tray::Split => self.header.split_into,
            _ => false,
        };
        div()
            .id("tray")
            .role(if matches!(self.header.tray, Tray::Confirm(_)) {
                gpui::Role::AlertDialog
            } else {
                gpui::Role::Dialog
            })
            .aria_label(name)
            .key_context("Tray")
            .track_focus(&self.header.tray_focus.clone().tab_stop(listed))
            .flex()
            .flex_col()
            .px(px(16.))
            .py(px(12.))
            .border_t_1()
            .border_color(t.border2)
            .bg(if danger {
                Theme::blend(t.danger, 0.06, t.bg_well)
            } else {
                t.bg_well
            })
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.close_tray(window, cx)))
            .on_action(
                cx.listener(|this, _: &TrayConfirm, window, cx| this.tray_submit(true, window, cx)),
            )
            // Return on a control inside the tray presses that control.
            .on_action(cx.listener(|this, _: &TrayEnter, window, cx| {
                if this.header.tray_focus.is_focused(window) {
                    this.tray_submit(false, window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &TrayUp, _, cx| this.move_dest(-1, cx)))
            .on_action(cx.listener(|this, _: &TrayDown, _, cx| this.move_dest(1, cx)))
    }

    /// A tray's opening line: the verb and its subject in full ink, then
    /// quiet context.
    pub(super) fn tray_title(&self, lead: &str, context: String, cx: &App) -> Div {
        let t = theme(cx);
        h_flex()
            .min_w_0()
            .gap(px(8.))
            .items_baseline()
            .text_size(px(TEXT_M))
            .font_weight(FontWeight::SEMIBOLD)
            .child(div().flex_shrink_0().child(rich(lead, cx)))
            .child(
                div()
                    .min_w_0()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(t.text3)
                    .child(truncated(context)),
            )
    }

    pub(super) fn label(&self, text: &'static str, cx: &App) -> Div {
        div()
            .mt(px(10.))
            .mb(px(4.))
            .text_size(px(TEXT_XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme(cx).text3)
            .child(text)
    }

    /// What will happen, one consequence per line.
    pub(super) fn consequences(&self, items: Vec<(String, bool)>, cx: &App) -> Div {
        let t = theme(cx);
        let mut list = v_flex()
            .mt(px(8.))
            .gap(px(2.))
            .text_size(px(TEXT_S))
            .line_height(gpui::relative(1.5));
        for (text, blocked) in items {
            list = list.child(
                h_flex()
                    .items_start()
                    .gap(px(6.))
                    .text_color(if blocked { t.warn } else { t.text2 })
                    .child(div().flex_shrink_0().child("•"))
                    .child(div().flex_1().min_w_0().child(rich(&text, cx))),
            );
        }
        list
    }

    /// A tray's closing row: a hint, the error if one, then the buttons.
    pub(super) fn confirm_row(&self, hint: String, buttons: Vec<AnyElement>, cx: &App) -> Div {
        let t = theme(cx);
        let mut row = h_flex().mt(px(12.)).gap(px(8.)).child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .child(hint),
        );
        if let Some(error) = &self.header.error {
            row = row.child(
                div()
                    .id("tray-error")
                    .alert(error.clone())
                    .min_w_0()
                    .max_w(px(360.))
                    .text_size(px(TEXT_XS))
                    .text_color(t.danger)
                    .tooltip(tooltip(error.clone()))
                    .child(truncated(
                        error.lines().next().unwrap_or_default().to_owned(),
                    )),
            );
        }
        row.children(buttons)
    }

    pub(super) fn cancel_button(&self, label: &'static str, cx: &mut Context<Self>) -> AnyElement {
        btn("tray-cancel", label)
            .disabled(self.header.acting)
            .on_click(cx.listener(|this, _, window, cx| this.close_tray(window, cx)))
            .into_any_element()
    }

    /// One row of a destination list: marker, short id, title, bookmarks.
    /// The picked one is what the keyboard travels from; `ringed` while the
    /// keyboard holds the tray.
    pub(super) fn dest_row(
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        key: usize,
        selected: bool,
        ringed: bool,
        theme: &Theme,
        cx: &App,
    ) -> Stateful<Div> {
        let glyph_color = match node.kind {
            NodeKind::WorkingCopy => theme.working_copy,
            NodeKind::Mutable => theme.text2,
            NodeKind::Immutable => theme.text3,
        };
        let mut row = h_flex()
            .id(("dest", key))
            .role(gpui::Role::ListBoxOption)
            .aria_label(format!(
                "{}, {}",
                title_or_none(&node.description),
                short_id(&node.id, 8)
            ))
            .aria_selected(selected)
            .when(ringed, |row| row.shadow(ring(theme.accent)))
            .w_full()
            .h(px(26.))
            .gap(px(8.))
            .px(px(8.))
            .rounded(px(RADIUS_S))
            .text_size(px(TEXT_S))
            .cursor_pointer()
            .child(
                div()
                    .w(px(12.))
                    .flex_shrink_0()
                    .font_family(theme.mono.clone())
                    .text_color(glyph_color)
                    .child(kind_glyph(node.kind)),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .font_family(theme.mono.clone())
                    .text_color(theme.text3)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text1)
                            .child(short_id(&node.id, 2).to_owned()),
                    )
                    .child(node.id.chars().skip(2).take(2).collect::<String>()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.text2)
                    .child(truncated(title_or_none(&node.description).to_owned())),
            );
        for name in &node.bookmarks {
            let trunk = *name == snapshot.trunk_bookmark;
            row = row.child(
                if trunk {
                    trunk_label(name.clone(), cx)
                } else {
                    ref_label(None, name.clone(), cx)
                }
                .max_w(px(140.)),
            );
        }
        if selected {
            row.bg(theme.accent_dim)
        } else {
            let hover = theme.bg_hover;
            row.hover(move |style| style.bg(hover))
        }
    }

    /// The scrolling destination list of the open tray, named `name`.
    pub(super) fn dest_list(
        &mut self,
        name: &'static str,
        empty: &'static str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let ids = Rc::new(self.visible_dests(cx));
        let focused = self.header.tray_focus.is_focused(window);
        let ringed = keyboard_on(&self.header.tray_focus, window);
        let frame = div()
            .id("dest-frame")
            .role(gpui::Role::ListBox)
            .aria_label(name)
            .mt(px(2.))
            .p(px(2.))
            .rounded(px(RADIUS_M))
            .border_1()
            .border_color(t.border2)
            .bg(t.bg1);
        if ids.is_empty() {
            return frame
                .child(
                    div()
                        .px(px(8.))
                        .py(px(6.))
                        .text_size(px(TEXT_S))
                        .text_color(t.text3)
                        .child(empty),
                )
                .into_any_element();
        }
        let compare = self.header.tray == Tray::Compare;
        let height = (ids.len() as f32 * 26.).min(200.);
        let list = vertical_only(uniform_list(
            "dest-list",
            ids.len(),
            cx.processor({
                let ids = ids.clone();
                move |this, range: Range<usize>, _, cx| {
                    let t = theme(cx);
                    // The comparison on screen, until the keys pick another.
                    let picked = this.header.dest.clone().or_else(|| match &this.compare {
                        CompareMode::Change(id) if compare => Some(id.clone()),
                        _ => None,
                    });
                    let mut rows = Vec::with_capacity(range.len());
                    for ix in range {
                        let id = ids[ix].clone();
                        let Some(node) = find_node(&this.snapshot, &id) else {
                            rows.push(div().h(px(26.)).into_any_element());
                            continue;
                        };
                        let selected = picked.as_deref() == Some(id.as_str());
                        let hover_id = id.clone();
                        let row = Self::dest_row(
                            &this.snapshot,
                            node,
                            ix,
                            selected,
                            ringed && selected,
                            &t,
                            cx,
                        )
                        .when(focused && selected, |row| row.aria_active_descendant())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if compare {
                                this.set_compare(CompareMode::Change(id.clone()), cx);
                                this.close_tray(window, cx);
                            } else {
                                this.header.dest = Some(id.clone());
                                cx.notify();
                            }
                        }))
                        .on_hover(cx.listener(
                            move |this, hovered: &bool, _, cx| {
                                let next = hovered.then(|| hover_id.clone());
                                if (*hovered || this.header.dest_hover.as_ref() == Some(&hover_id))
                                    && this.header.dest_hover != next
                                {
                                    this.header.dest_hover = next;
                                    cx.notify();
                                }
                            },
                        ));
                        rows.push(row.into_any_element());
                    }
                    rows
                }
            }),
        ))
        .track_scroll(&self.header.dest_scroll)
        .h(px(height));
        frame.child(list).into_any_element()
    }

    pub(super) fn dest_filter(&self, window: &Window, cx: &App) -> Option<Div> {
        (self.dest_candidates().len() > 6).then(|| {
            field(&self.header.filter, window, cx)
                .w(px(260.))
                .font_family(theme(cx).mono.clone())
        })
    }
}
