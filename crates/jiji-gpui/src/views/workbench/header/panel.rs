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
use crate::ui::{btn, h_flex, input::field, ref_label, rich, tooltip, truncated, v_flex};
use crate::views::workbench::{TrayConfirm, TrayDown, TrayEnter, TrayUp, WorkbenchView};

impl WorkbenchView {
    pub(super) fn tray(&self, danger: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme(cx);
        div()
            .id("tray")
            .key_context("Tray")
            .track_focus(&self.header.tray_focus)
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
            .on_action(
                cx.listener(|this, _: &TrayEnter, window, cx| this.tray_submit(false, window, cx)),
            )
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
    pub(super) fn dest_row(
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        key: usize,
        selected: bool,
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
            let mut chip = ref_label(None, name.clone(), cx).max_w(px(140.));
            if trunk {
                chip = chip.bg(theme.accent_dim).text_color(theme.accent_strong);
            }
            row = row.child(chip);
        }
        if selected {
            row.bg(theme.accent_dim)
        } else {
            let hover = theme.bg_hover;
            row.hover(move |style| style.bg(hover))
        }
    }

    /// The scrolling destination list of the open tray.
    pub(super) fn dest_list(&mut self, empty: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let ids = Rc::new(self.visible_dests(cx));
        let frame = div()
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
                    let picked = if compare {
                        match &this.compare {
                            CompareMode::Change(id) => Some(id.clone()),
                            _ => None,
                        }
                    } else {
                        this.header.dest.clone()
                    };
                    let mut rows = Vec::with_capacity(range.len());
                    for ix in range {
                        let id = ids[ix].clone();
                        let Some(node) = find_node(&this.snapshot, &id) else {
                            rows.push(div().h(px(26.)).into_any_element());
                            continue;
                        };
                        let selected = picked.as_deref() == Some(id.as_str());
                        let hover_id = id.clone();
                        let row = Self::dest_row(&this.snapshot, node, ix, selected, &t, cx)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if compare {
                                    this.set_compare(CompareMode::Change(id.clone()), cx);
                                    this.close_tray(window, cx);
                                } else {
                                    this.header.dest = Some(id.clone());
                                    cx.notify();
                                }
                            }))
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                let next = hovered.then(|| hover_id.clone());
                                if (*hovered || this.header.dest_hover.as_ref() == Some(&hover_id))
                                    && this.header.dest_hover != next
                                {
                                    this.header.dest_hover = next;
                                    cx.notify();
                                }
                            }));
                        rows.push(row.into_any_element());
                    }
                    rows
                }
            }),
        ))
        .track_scroll(self.header.dest_scroll.clone())
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
