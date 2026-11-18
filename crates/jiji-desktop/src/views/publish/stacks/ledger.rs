//! The landing-order ledger: the selected stack's segments from trunk
//! up, each with its pull request, checks and review.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString};
use jiji_forge::{
    ChecksRollup, LandSegment, LandSegmentStatus, PrSummary, PublishStack, ReviewDecision,
    StackSegment,
};

use crate::logic::inspect::summary_of;
use crate::logic::text::{plural_s, short_id};
use crate::theme::{theme, Theme, RADIUS_S, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::ScrollY as _;
use crate::ui::{
    h_flex, icon, ref_label, scrollbar, tooltip, truncated, trunk_label, v_flex, IconName,
};
use crate::views::publish::PublishView;

impl PublishView {
    /// One fact about a PR, glyph first: its checks, its review, its
    /// draft state.
    pub(super) fn signal(
        &self,
        glyph: IconName,
        text: &'static str,
        color: gpui::Hsla,
    ) -> gpui::Div {
        h_flex()
            .flex_shrink_0()
            .gap(px(4.))
            .text_color(color)
            .child(icon(glyph, 11., color))
            .child(text)
    }

    pub(super) fn render_segment(
        &self,
        segment: &StackSegment,
        index: usize,
        last: bool,
        landing: &[LandSegment],
        hue: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let key = segment
            .bookmark
            .clone()
            .unwrap_or_else(|| index.to_string());
        let planned = landing
            .iter()
            .find(|item| Some(&item.bookmark) == segment.bookmark.as_ref());
        let status = planned.map(|item| &item.status);
        let merged = match status {
            Some(LandSegmentStatus::Merged { number, url }) => Some((*number, url.clone())),
            _ => None,
        };
        let next = matches!(
            status,
            Some(LandSegmentStatus::Landing | LandSegmentStatus::Waiting)
        );
        let pr: Option<&PrSummary> = planned
            .and_then(|item| item.pr.as_ref())
            .or(segment.pr.as_ref());
        let open = self.open.contains(&format!("segment-{key}"));

        // The rail: the stack's own line, its stations numbered in landing
        // order; a landed one turns the ok hue.
        let station = if merged.is_some() { t.ok } else { hue };
        let mut marker = div()
            .flex()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(10.))
            .border(px(if next { 2.5 } else { 2. }))
            .border_color(station)
            .bg(if next {
                Theme::fade(station, 0.18)
            } else {
                t.bg1
            })
            .text_size(px(TEXT_XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if next { t.text1 } else { station });
        marker = if merged.is_some() {
            marker.child(icon(IconName::Check, 11., t.ok))
        } else {
            marker.child((index + 1).to_string())
        };
        let mut rail = div()
            .relative()
            .w(px(20.))
            .flex_shrink_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(8.5))
                    .w(px(3.))
                    .h(px(10.))
                    .bg(hue),
            );
        if !last {
            rail = rail.child(
                div()
                    .absolute()
                    .top(px(30.))
                    .bottom_0()
                    .left(px(8.5))
                    .w(px(3.))
                    .bg(hue),
            );
        }
        rail = rail.child(div().mt(px(10.)).child(marker));

        let mut line = h_flex().gap(px(8.)).min_w_0().min_h(px(22.));
        let number = merged
            .clone()
            .or_else(|| pr.map(|pr| (pr.number, pr.url.clone())));
        if let Some((number, url)) = number {
            line = line.child(
                div()
                    .id(SharedString::from(format!("seg-pr-{key}")))
                    .flex_shrink_0()
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .cursor_pointer()
                    .hover(|style| style.text_color(t.accent_strong))
                    .tooltip(tooltip(format!("Open pull request #{number} on GitHub")))
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        crate::views::menu::pr_menu(number),
                    )
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .child(format!("#{number}")),
            );
        }
        line = line.child(
            div()
                .min_w(px(40.))
                .flex_shrink_1()
                .text_size(px(TEXT_M))
                .text_color(if merged.is_some() { t.text3 } else { t.text1 })
                .child(truncated(
                    pr.map_or_else(|| segment.title.clone(), |pr| pr.title.clone()),
                )),
        );
        if merged.is_some() {
            line = line.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(TEXT_XS))
                    .text_color(t.ok)
                    .child("Merged"),
            );
        } else if next {
            line = line.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(TEXT_XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.accent_strong)
                    .child("Next to land"),
            );
        }

        let mut meta = h_flex()
            .gap(px(12.))
            .min_w_0()
            .overflow_hidden()
            .text_size(px(TEXT_XS))
            .text_color(t.text3);
        meta = match &segment.bookmark {
            Some(bookmark) => meta.child(
                ref_label(None, bookmark.clone(), cx)
                    .id(SharedString::from(format!("seg-ref-{key}")))
                    .flex_shrink_1()
                    .min_w_0()
                    .max_w(px(220.))
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        crate::views::menu::bookmark_menu(bookmark.clone(), None),
                    ),
            ),
            None => meta.child(
                div()
                    .id(SharedString::from(format!("seg-new-{key}")))
                    .flex_shrink_0()
                    .tooltip(tooltip("Branch name is chosen when publishing"))
                    .child("new branch"),
            ),
        };
        match (pr, merged.is_some()) {
            (Some(pr), false) => {
                if pr.is_draft {
                    meta = meta.child(self.signal(IconName::Edit, "Draft", t.text3));
                }
                meta = match pr.checks {
                    ChecksRollup::Passing => {
                        meta.child(self.signal(IconName::Check, "Checks passed", t.ok))
                    }
                    ChecksRollup::Failing => {
                        meta.child(self.signal(IconName::Close, "Checks failed", t.danger))
                    }
                    ChecksRollup::Pending => {
                        meta.child(self.signal(IconName::Operations, "Checks running", t.warn))
                    }
                    ChecksRollup::None => meta,
                };
                meta = match pr.review {
                    ReviewDecision::Approved => {
                        meta.child(self.signal(IconName::BadgeCheck, "Approved", t.ok))
                    }
                    ReviewDecision::ChangesRequested => {
                        meta.child(self.signal(IconName::Eye, "Changes requested", t.danger))
                    }
                    ReviewDecision::ReviewRequired => {
                        meta.child(self.signal(IconName::Eye, "Review needed", t.text2))
                    }
                    ReviewDecision::None => meta,
                };
            }
            (None, false) => meta = meta.child(div().flex_shrink_0().child("Not published")),
            _ => {}
        }
        if !segment.changes.is_empty() {
            let toggle = format!("segment-{key}");
            meta = meta.child(
                h_flex()
                    .id(SharedString::from(format!("seg-changes-{key}")))
                    .flex_shrink_0()
                    .gap(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.text_color(t.text1))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_open(&toggle, cx)))
                    .child(format!(
                        "{} change{}",
                        segment.changes.len(),
                        plural_s(segment.changes.len())
                    ))
                    .child(icon(
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        11.,
                        t.text3,
                    )),
            );
        }
        let mut body = v_flex()
            .flex_1()
            .min_w_0()
            .gap(px(3.))
            .py(px(9.))
            .child(line)
            .child(meta);
        if open {
            let mut changes = v_flex().mt(px(4.));
            for (cx_index, change) in segment.changes.iter().enumerate() {
                let (app, id) = (self.app.clone(), change.id.clone());
                let title = match summary_of(&change.description) {
                    "" => "Untitled change",
                    title => title,
                };
                changes = changes.child(
                    h_flex()
                        .id(SharedString::from(format!("seg-change-{key}-{cx_index}")))
                        .gap(px(8.))
                        .h(px(26.))
                        .mx(px(-6.))
                        .px(px(6.))
                        .rounded(px(RADIUS_S))
                        .text_size(px(TEXT_S))
                        .text_color(t.text2)
                        .cursor_pointer()
                        .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
                        .tooltip(tooltip("Review this change on the workbench"))
                        .on_mouse_down(
                            gpui::MouseButton::Right,
                            crate::views::menu::change_menu(change.id.clone()),
                        )
                        .on_click(move |_, _, cx| {
                            app.update(cx, |model, cx| model.jump_to_change(&id, cx))
                        })
                        .child(icon(IconName::Commit, 13., t.text3))
                        .child(div().flex_1().min_w_0().child(truncated(title.to_owned())))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .text_color(t.text3)
                                .child(short_id(&change.change_id, 8).to_owned()),
                        ),
                );
            }
            body = body.child(changes);
        }
        let mut row = div().flex().flex_row().gap(px(12.)).px(px(16.));
        if next {
            row = row.bg(Theme::fade(t.accent, 0.05));
        }
        row.child(rail).child(body).into_any_element()
    }

    /// The stack as a ledger: one row per segment in landing order, the
    /// base branch at the top, a hairline rail threading the positions.
    /// The land plan or job overlays each row's landing status when one
    /// is live.
    pub(super) fn render_chain(
        &self,
        stack: &PublishStack,
        landing: &[LandSegment],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let total = stack.segments.len();
        // The stack's line, as the graph draws it.
        let hue = self
            .snapshot
            .as_deref()
            .zip(stack.head_change.as_deref())
            .and_then(|(snapshot, head)| crate::ui::line_of(snapshot, head, &t))
            .unwrap_or(t.accent);
        let merged = landing
            .iter()
            .filter(|segment| matches!(segment.status, LandSegmentStatus::Merged { .. }))
            .count();
        let mut ledger = v_flex()
            .id("chain-scroll")
            .flex_1()
            .min_h_0()
            .scroll_y()
            .track_scroll(&self.chain_scroll)
            .pb(px(20.))
            .child(
                h_flex()
                    .gap(px(12.))
                    .px(px(16.))
                    .pt(px(12.))
                    .child(
                        div()
                            .relative()
                            .w(px(20.))
                            .h(px(26.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .absolute()
                                    .top(px(16.))
                                    .bottom_0()
                                    .left(px(8.5))
                                    .w(px(3.))
                                    .bg(hue),
                            )
                            .child(
                                // Trunk, where the line lands: history's
                                // diamond.
                                div().mt(px(3.)).child(crate::ui::node_mark(
                                    crate::ui::Mark::Base,
                                    Theme::blend(t.text3, 0.8, t.bg1),
                                    14.,
                                )),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .h(px(20.))
                            .mb(px(6.))
                            .child(trunk_label(
                                if stack.base.is_empty() {
                                    "trunk".to_owned()
                                } else {
                                    stack.base.clone()
                                },
                                cx,
                            ))
                            .child(
                                div()
                                    .text_size(px(TEXT_XS))
                                    .text_color(t.text3)
                                    .child("target branch"),
                            ),
                    ),
            );
        for (index, segment) in stack.segments.iter().enumerate() {
            ledger = ledger.child(self.render_segment(
                segment,
                index,
                index + 1 == total,
                landing,
                hue,
                cx,
            ));
        }
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                // The ledger's own toolbar: same height and hairline as
                // the action column's head beside it, so one line runs
                // across the detail.
                h_flex()
                    .flex_shrink_0()
                    .h(px(TOOLBAR_H))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(t.border2)
                    .text_size(px(TEXT_S))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text2)
                            .child("Landing order"),
                    )
                    .child(div().flex_1())
                    .child(div().text_color(t.text3).child(if stack.pr_count > 0 {
                        format!("{merged} of {total} merged")
                    } else {
                        format!("{total} segment{}", plural_s(total))
                    })),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(ledger)
                    .child(scrollbar("chain-scrollbar", &self.chain_scroll)),
            )
    }
}
