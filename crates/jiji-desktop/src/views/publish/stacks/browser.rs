//! The stack browser: every stack this repo has, searchable, grouped by
//! where it stands.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString};
use jiji_forge::PublishStack;

use super::BROWSER_W;
use crate::logic::publish::stack_summary;
use crate::logic::text::plural_s;
use crate::theme::{theme, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::ScrollY as _;
use crate::ui::{
    dot, h_flex, icon, kbd, lead_bar, node_mark, scrollbar, tooltip, truncated, v_flex, IconName,
    Mark, Tabbable as _, Tabular as _,
};
use crate::views::publish::PublishView;

impl PublishView {
    pub(super) fn render_stack_row(
        &self,
        stack: &PublishStack,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let (label, tone) = stack_summary(stack.status);
        let selected = self.selected.as_deref() == Some(&stack.key);
        let key = stack.key.clone();
        // The stack is a line on the graph: it wears that line's hue here,
        // on its station and, while chosen, across its row.
        let line = self
            .snapshot
            .as_deref()
            .zip(stack.head_change.as_deref())
            .and_then(|(snapshot, head)| crate::ui::line_of(snapshot, head, &t));
        let hue = line.unwrap_or(t.text3);
        let mark = div()
            .id(SharedString::from(format!("mark-{}", stack.key)))
            .w(px(12.))
            .flex_shrink_0()
            .flex()
            .justify_center()
            .when(stack.is_active, |el| el.tooltip(tooltip("Current stack")))
            .child(if stack.is_active {
                node_mark(Mark::WorkingCopy, t.working_copy, 11.).into_any_element()
            } else {
                node_mark(Mark::Station { filled: selected }, hue, 10.).into_any_element()
            });
        let title = h_flex().gap(px(6.)).min_w_0().child(
            div()
                .min_w_0()
                .flex_shrink_1()
                .text_size(px(TEXT_M))
                .text_color(t.text1)
                .child(truncated(stack.title.clone())),
        );
        let count = if stack.pr_count > 0 {
            format!("{} PR{}", stack.pr_count, plural_s(stack.pr_count as usize))
        } else {
            format!(
                "{} change{}",
                stack.change_count,
                plural_s(stack.change_count as usize)
            )
        };
        let id = SharedString::from(format!("stack-{}", stack.key));
        let spoken = format!("{}, {label}, {count}", stack.title);
        let mut row = if stack.pr_count == 0 {
            // Local work is all one state — the group says it — so a row
            // is the title and how much is in it.
            h_flex()
                .id(id)
                .relative()
                .flex_shrink_0()
                .h(px(30.))
                .gap(px(9.))
                .pl(px(12.))
                .pr(px(14.))
                .child(mark)
                .child(title.flex_1())
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(TEXT_XS))
                        .text_color(t.text3)
                        .tabular()
                        .child(stack.change_count.to_string()),
                )
                .tooltip(tooltip(count))
        } else {
            h_flex()
                .id(id)
                .relative()
                .flex_shrink_0()
                .items_start()
                .gap(px(9.))
                .py(px(7.))
                .pl(px(12.))
                .pr(px(12.))
                .child(mark.mt(px(3.)))
                .child(
                    v_flex().flex_1().min_w_0().child(title).child(
                        h_flex()
                            .gap(px(6.))
                            .mt(px(3.))
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .child(dot(t.tone(tone), 6.))
                            .child(label)
                            .child(div().w(px(1.)).h(px(9.)).bg(t.border1))
                            .child(count),
                    ),
                )
        };
        row = if selected {
            match line {
                Some(hue) => row
                    .bg(crate::theme::Theme::fade(
                        hue,
                        if t.is_dark() { 0.16 } else { 0.11 },
                    ))
                    .child(crate::ui::lead_bar_in(hue)),
                None => row.bg(t.selected).child(lead_bar(cx)),
            }
        } else {
            row.hover(|style| style.bg(t.bg_hover))
        };
        let row = row
            .role(gpui::Role::ListBoxOption)
            .aria_label(spoken)
            .aria_selected(selected);
        if self.executing() {
            row.opacity(0.6).into_any_element()
        } else {
            row.tabbable(cx)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.select_stack(&key, cx)))
                .into_any_element()
        }
    }

    pub(super) fn render_browser(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let query = self.query.read(cx).text().trim().to_lowercase();
        let matches = |stack: &&PublishStack| stack.search_text.contains(&query);
        let published: Vec<&PublishStack> = self
            .stacks
            .iter()
            .filter(|stack| stack.pr_count > 0)
            .collect();
        let local: Vec<&PublishStack> = self
            .stacks
            .iter()
            .filter(|stack| stack.pr_count == 0)
            .collect();
        let shown_published: Vec<&PublishStack> =
            published.iter().copied().filter(matches).collect();
        let shown_local: Vec<&PublishStack> = local.iter().copied().filter(matches).collect();
        let (prs_loading, has_prs) = {
            let forge = &self.app.read(cx).forge;
            (forge.prs_loading, forge.prs.is_some())
        };
        let heading = |label: &'static str, count: usize| {
            h_flex()
                .gap(px(5.))
                .h(px(28.))
                .mt(px(8.))
                .pl(px(16.))
                .pr(px(12.))
                .flex_shrink_0()
                .text_size(px(TEXT_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text3)
                .child(label)
                .child(
                    div()
                        .font_weight(FontWeight::NORMAL)
                        .child(count.to_string()),
                )
        };
        let quiet = |text: &'static str| {
            div()
                .py(px(10.))
                .px(px(16.))
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child(text)
        };
        let mut list = v_flex()
            .id("stack-list")
            .role(gpui::Role::ListBox)
            .aria_label("Stacks")
            .flex_1()
            .min_h_0()
            .scroll_y()
            .track_scroll(&self.list_scroll)
            .pb(px(20.));
        if !published.is_empty() {
            list = list.child(heading("Published", shown_published.len()));
            for stack in &shown_published {
                list = list.child(self.render_stack_row(stack, cx));
            }
        }
        if (prs_loading && !has_prs) || (self.stacks_loading && self.stacks.is_empty()) {
            list = list.child(quiet("Finding pull requests…"));
        }
        if !local.is_empty() {
            let open = self.show_local || published.is_empty() || !query.is_empty();
            list = list.child(
                heading("Local work", shown_local.len())
                    .id("local-heading")
                    .role(gpui::Role::Button)
                    .aria_label(format!("Local work, {}", shown_local.len()))
                    .aria_expanded(open)
                    .tabbable(cx)
                    .cursor_pointer()
                    .hover(|style| style.text_color(t.text2))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_local = !this.show_local;
                        cx.notify();
                    }))
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
            if open {
                for stack in &shown_local {
                    list = list.child(self.render_stack_row(stack, cx));
                }
            }
        }
        if !query.is_empty() && shown_published.is_empty() && shown_local.is_empty() {
            list = list.child(quiet("No matching stacks."));
        }
        v_flex()
            .w(px(BROWSER_W))
            .flex_shrink_0()
            .min_h_0()
            .border_r_1()
            .border_color(t.border2)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(px(TOOLBAR_H))
                    .gap(px(8.))
                    .pl(px(16.))
                    .pr(px(10.))
                    .border_b_1()
                    .border_color(t.border2)
                    .text_size(px(TEXT_S))
                    .line_height(px(18.))
                    .child(icon(IconName::Search, 13., t.text3))
                    .child(self.query.clone())
                    .child(kbd("/", cx)),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(scrollbar("stack-list-scroll", &self.list_scroll)),
            )
    }
}
