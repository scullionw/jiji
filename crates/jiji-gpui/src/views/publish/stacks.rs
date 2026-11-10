//! The Stacks page: the stack browser on the left, and for the selected
//! stack its landing-order ledger beside the plan card that acts on it.

use gpui::{div, prelude::*, px, AnyElement, Context, Corner, FontWeight, SharedString, Window};
use jiji_core::snapshot::RepoSnapshot;
use jiji_forge::{
    AutoLandStatus, ChecksRollup, LandSegment, LandSegmentStatus, MergeMethod, PrSummary,
    PublishStack, ReviewDecision, StackSegment, SubmitHead,
};

use super::{tone_color, Mode, Page, PublishView};
use crate::logic::autoland::can_queue;
use crate::logic::inspect::summary_of;
use crate::logic::publish::{
    comment_preview, land_action_text, manual_land_label, method_word, ship_action_text,
    stack_summary, submit_action_text,
};
use crate::theme::{theme, Theme, RADIUS_M, RADIUS_S, TEXT_L, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::{
    btn, checkbox, dot, empty_state, floating, h_flex, icon, kbd, menu_row, popover, ref_label,
    scrollbar, segmented, tooltip, truncated, v_flex, IconName, SwallowClicks,
};

const BROWSER_W: f32 = 280.;
const ACTIONS_W: f32 = 300.;

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

impl PublishView {
    // ── Selecting and planning ──

    /// Selecting a stack answers a read-only plan; executing replaces it
    /// with per-step results.
    pub(super) fn select_stack(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.executing() {
            return;
        }
        let Some(stack) = self.stacks.iter().find(|stack| stack.key == key).cloned() else {
            return;
        };
        self.selected = Some(stack.key.clone());
        self.page = Page::Stacks;
        self.plan_seq += 1;
        self.land_seq += 1;
        self.ship_seq += 1;
        self.plan = None;
        self.land = None;
        self.ship = None;
        self.outcome = None;
        self.land_outcome = None;
        self.ship_outcome = None;
        self.plan_error = None;
        self.land_error = None;
        self.ship_error = None;
        self.queue_error = None;
        self.plan_loading = false;
        self.land_loading = false;
        self.ship_loading = false;
        self.land_method = None;
        self.land_bypass = false;
        self.more.close();
        if self.connected(cx) {
            match (stack.pr_count > 0, stack.head_bookmark, stack.submit_head) {
                (true, Some(bookmark), Some(_)) => self.load_land_plan(bookmark, cx),
                (_, _, Some(head)) => self.load_plan(head, cx),
                _ => {}
            }
        }
        cx.notify();
    }

    pub(super) fn load_plan(&mut self, head: SubmitHead, cx: &mut Context<Self>) {
        let Some(repo) = self.repo_path(cx) else {
            return;
        };
        self.mode = Mode::Publish;
        self.plan_seq += 1;
        let seq = self.plan_seq;
        self.plan_head = Some(head.clone());
        self.plan = None;
        self.outcome = None;
        self.plan_error = None;
        self.plan_loading = true;
        cx.notify();
        let planned = self
            .app
            .read(cx)
            .bg(cx, move |host| host.submit_plan(&repo, &head));
        cx.spawn(async move |this, cx| {
            let result = planned.await;
            let _ = this.update(cx, |this, cx| {
                if this.plan_seq != seq {
                    return;
                }
                this.plan_loading = false;
                match result {
                    Ok(plan) => this.plan = Some(plan),
                    Err(error) => this.plan_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        let (Some(plan), Some(head), Some(repo)) = (
            self.plan.clone(),
            self.plan_head.clone(),
            self.repo_path(cx),
        ) else {
            return;
        };
        if self.publishing || !plan.blockers.is_empty() {
            return;
        }
        self.publishing = true;
        self.plan_error = None;
        cx.notify();
        let ran = self
            .app
            .read(cx)
            .bg(cx, move |host| host.submit_stack(&repo, &head, &plan));
        cx.spawn(async move |this, cx| {
            let result = ran.await;
            let _ = this.update(cx, |this, cx| {
                this.publishing = false;
                match result {
                    Ok(outcome) => {
                        this.outcome = Some(outcome);
                        // Fresh badges and PR counts follow what just
                        // landed on GitHub.
                        this.app.update(cx, |model, cx| model.refresh_forge_prs(cx));
                    }
                    Err(error) => this.plan_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_land_plan(&mut self, head_bookmark: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo_path(cx) else {
            return;
        };
        self.mode = Mode::Land;
        self.land_seq += 1;
        let seq = self.land_seq;
        self.land_for = Some(head_bookmark.clone());
        self.land = None;
        self.land_outcome = None;
        self.land_error = None;
        self.land_loading = true;
        cx.notify();
        let options = self.land_options();
        let planned = self.app.read(cx).bg(cx, move |host| {
            host.land_plan(&repo, &head_bookmark, Some(options))
        });
        cx.spawn(async move |this, cx| {
            let result = planned.await;
            let _ = this.update(cx, |this, cx| {
                if this.land_seq != seq {
                    return;
                }
                this.land_loading = false;
                match result {
                    Ok(plan) => this.land = Some(plan),
                    Err(error) => this.land_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn replan_land(&mut self, cx: &mut Context<Self>) {
        if let (Some(head), false) = (self.land_for.clone(), self.landing) {
            self.load_land_plan(head, cx);
        }
    }

    /// The steps republished the snapshot as they ran; pulling it once
    /// more keeps the flow self-contained, and badges and PR counts follow
    /// what just changed on GitHub.
    fn after_run(&mut self, repo: &str, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| {
            if let Some(snapshot) = model.host.snapshot_for(repo) {
                model.apply_snapshot(snapshot, cx);
            }
            model.refresh_forge_prs(cx);
        });
    }

    fn land_stack(&mut self, cx: &mut Context<Self>) {
        let (Some(plan), Some(head), Some(repo)) =
            (self.land.clone(), self.land_for.clone(), self.repo_path(cx))
        else {
            return;
        };
        if self.landing || !plan.blockers.is_empty() {
            return;
        }
        self.landing = true;
        self.land_error = None;
        cx.notify();
        let ran = self.app.read(cx).bg(cx, {
            let repo = repo.clone();
            move |host| host.land_stack(&repo, &head, &plan)
        });
        cx.spawn(async move |this, cx| {
            let result = ran.await;
            let _ = this.update(cx, |this, cx| {
                this.landing = false;
                match result {
                    Ok(outcome) => {
                        this.land_outcome = Some(outcome);
                        this.after_run(&repo, cx);
                    }
                    Err(error) => this.land_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_ship_plan(&mut self, head_change: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repo_path(cx) else {
            return;
        };
        self.mode = Mode::Ship;
        self.more.close();
        self.ship_seq += 1;
        let seq = self.ship_seq;
        self.ship_for = Some(head_change.clone());
        self.ship = None;
        self.ship_outcome = None;
        self.ship_error = None;
        self.ship_loading = true;
        cx.notify();
        let planned = self
            .app
            .read(cx)
            .bg(cx, move |host| host.ship_plan(&repo, &head_change));
        cx.spawn(async move |this, cx| {
            let result = planned.await;
            let _ = this.update(cx, |this, cx| {
                if this.ship_seq != seq {
                    return;
                }
                this.ship_loading = false;
                match result {
                    Ok(plan) => this.ship = Some(plan),
                    Err(error) => this.ship_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ship_stack(&mut self, cx: &mut Context<Self>) {
        let (Some(plan), Some(head), Some(repo)) =
            (self.ship.clone(), self.ship_for.clone(), self.repo_path(cx))
        else {
            return;
        };
        if self.shipping || !plan.blockers.is_empty() || plan.actions.is_empty() {
            return;
        }
        self.shipping = true;
        self.ship_error = None;
        cx.notify();
        let ran = self.app.read(cx).bg(cx, {
            let repo = repo.clone();
            move |host| host.ship_stack(&repo, &head, &plan)
        });
        cx.spawn(async move |this, cx| {
            let result = ran.await;
            let _ = this.update(cx, |this, cx| {
                this.shipping = false;
                match result {
                    Ok(outcome) => {
                        this.ship_outcome = Some(outcome);
                        this.after_run(&repo, cx);
                    }
                    Err(error) => this.ship_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ── The browser ──

    fn render_stack_row(&self, stack: &PublishStack, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let (label, tone) = stack_summary(stack.status);
        let selected = self.selected.as_deref() == Some(&stack.key);
        let key = stack.key.clone();
        let mut title = h_flex().gap(px(6.)).min_w_0().child(
            div()
                .min_w_0()
                .flex_shrink()
                .text_size(px(TEXT_M))
                .text_color(t.text1)
                .child(truncated(stack.title.clone())),
        );
        if stack.is_active {
            title = title.child(
                div()
                    .id(SharedString::from(format!("current-{}", stack.key)))
                    .tooltip(tooltip("Current stack"))
                    .child(dot(t.working_copy, 6.)),
            );
        }
        let count = if stack.pr_count > 0 {
            format!("{} PR{}", stack.pr_count, plural(stack.pr_count as usize))
        } else {
            format!(
                "{} change{}",
                stack.change_count,
                plural(stack.change_count as usize)
            )
        };
        let mut row = v_flex()
            .id(SharedString::from(format!("stack-{}", stack.key)))
            .relative()
            .flex_shrink_0()
            .py(px(7.))
            .pl(px(16.))
            .pr(px(12.))
            .child(title)
            .child(
                h_flex()
                    .gap(px(6.))
                    .mt(px(3.))
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child(dot(tone_color(&t, tone), 6.))
                    .child(label)
                    .child(div().w(px(1.)).h(px(9.)).bg(t.border1))
                    .child(count),
            );
        row = if selected {
            row.bg(Theme::fade(t.accent, 0.12)).child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(t.accent),
            )
        } else {
            row.hover(|style| style.bg(t.bg_hover))
        };
        if self.executing() {
            row.opacity(0.6).into_any_element()
        } else {
            row.cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.select_stack(&key, cx)))
                .into_any_element()
        }
    }

    fn render_browser(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
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

    // ── The ledger ──

    /// One fact about a PR, glyph first: its checks, its review, its
    /// draft state.
    fn signal(&self, glyph: IconName, text: &'static str, color: gpui::Hsla) -> gpui::Div {
        h_flex()
            .flex_shrink_0()
            .gap(px(4.))
            .text_color(color)
            .child(icon(glyph, 11., color))
            .child(text)
    }

    fn render_segment(
        &self,
        segment: &StackSegment,
        index: usize,
        last: bool,
        landing: &[LandSegment],
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

        // The rail: a position marker on a hairline threading the rows.
        let marker_color = if merged.is_some() {
            t.ok
        } else if next {
            t.accent_strong
        } else {
            t.text3
        };
        let mut marker = div()
            .flex()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(10.))
            .border_1()
            .border_color(if next { t.accent } else { t.border1 })
            .bg(t.bg1)
            .text_size(px(TEXT_XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(marker_color);
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
                    .left(px(9.5))
                    .w(px(1.))
                    .h(px(10.))
                    .bg(t.border1),
            );
        if !last {
            rail = rail.child(
                div()
                    .absolute()
                    .top(px(30.))
                    .bottom_0()
                    .left(px(9.5))
                    .w(px(1.))
                    .bg(t.border1),
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
                .flex_shrink()
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
                    .flex_shrink()
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
                        plural(segment.changes.len())
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
                                .child(change.change_id.chars().take(8).collect::<String>()),
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
    fn render_chain(
        &self,
        stack: &PublishStack,
        landing: &[LandSegment],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let total = stack.segments.len();
        let merged = landing
            .iter()
            .filter(|segment| matches!(segment.status, LandSegmentStatus::Merged { .. }))
            .count();
        let mut ledger = v_flex()
            .id("chain-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
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
                                    .top(px(20.))
                                    .bottom_0()
                                    .left(px(9.5))
                                    .w(px(1.))
                                    .bg(t.border1),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(20.))
                                    .rounded(px(10.))
                                    .bg(t.accent_dim)
                                    .child(icon(IconName::Branch, 11., t.accent_strong)),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .h(px(20.))
                            .mb(px(6.))
                            .child(
                                div()
                                    .h(px(17.))
                                    .px(px(5.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(RADIUS_S))
                                    .bg(t.accent_dim)
                                    .font_family(t.mono.clone())
                                    .text_size(px(TEXT_XS))
                                    .text_color(t.accent_strong)
                                    .child(if stack.base.is_empty() {
                                        "trunk".to_owned()
                                    } else {
                                        stack.base.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(TEXT_XS))
                                    .text_color(t.text3)
                                    .child("target branch"),
                            ),
                    ),
            );
        for (index, segment) in stack.segments.iter().enumerate() {
            ledger =
                ledger.child(self.render_segment(segment, index, index + 1 == total, landing, cx));
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
                        format!("{total} segment{}", plural(total))
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

    // ── The plan card ──

    fn plan_heading(&self, text: impl Into<SharedString>, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(6.))
            .text_size(px(TEXT_L))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.text1)
            .child(text.into())
    }

    fn plan_text(&self, text: impl Into<SharedString>, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(12.))
            .text_size(px(TEXT_S))
            .line_height(gpui::relative(1.55))
            .text_color(t.text2)
            .child(text.into())
    }

    fn footnote(&self, text: impl Into<SharedString>, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(12.))
            .text_size(px(TEXT_XS))
            .line_height(gpui::relative(1.5))
            .text_color(t.text3)
            .child(text.into())
    }

    fn note(&self, text: impl Into<SharedString>, warn: bool, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(8.))
            .text_size(px(TEXT_S))
            .line_height(gpui::relative(1.5))
            .text_color(if warn { t.warn } else { t.text2 })
            .child(text.into())
    }

    fn error_line(&self, text: String, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(8.))
            .text_size(px(TEXT_S))
            .line_height(gpui::relative(1.5))
            .text_color(t.danger)
            .child(text)
    }

    /// What stands in the way: a bar per blocker, quieter for the ones
    /// that clear by themselves.
    fn blockers(&self, items: Vec<(String, bool)>, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        let mut list = v_flex().mb(px(8.));
        for (message, wait) in items {
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(10.))
                    .py(px(4.))
                    .text_size(px(TEXT_S))
                    .line_height(gpui::relative(1.5))
                    .text_color(t.text1)
                    .child(div().w(px(3.)).flex_shrink_0().rounded(px(2.)).bg(if wait {
                        t.warn
                    } else {
                        t.danger
                    }))
                    .child(div().flex_1().min_w_0().child(message)),
            );
        }
        list
    }

    /// The steps a run will take, numbered.
    fn action_list(&self, steps: Vec<String>, cx: &Context<Self>) -> AnyElement {
        let t = theme(cx);
        let mut list = v_flex()
            .mb(px(4.))
            .text_size(px(TEXT_S))
            .line_height(gpui::relative(1.5))
            .text_color(t.text1);
        for (index, step) in steps.into_iter().enumerate() {
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .py(px(3.))
                    .child(
                        div()
                            .w(px(14.))
                            .flex_shrink_0()
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .text_color(t.text3)
                            .child(format!("{}", index + 1)),
                    )
                    .child(div().flex_1().min_w_0().child(step)),
            );
        }
        list.into_any_element()
    }

    fn skeleton(&self, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        let bar = |width: f32| {
            div()
                .h(px(10.))
                .my(px(8.))
                .w(gpui::relative(width))
                .rounded(px(RADIUS_S))
                .bg(Theme::fade(t.text1, 0.08))
        };
        v_flex().child(bar(1.)).child(bar(0.62))
    }

    fn stacked(&self) -> gpui::Div {
        v_flex().gap(px(8.)).mt(px(16.)).mb(px(12.))
    }

    fn render_land_plan(
        &self,
        stack: &PublishStack,
        watching: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let t = theme(cx);
        let executing = self.executing();
        let job_active = self.job_active(cx);
        let mut card = v_flex();
        if watching {
            card = card
                .child(self.plan_heading("Landing in progress", cx))
                .child(self.plan_text(
                    "Jiji is watching this stack. Each PR lands when its requirements are met.",
                    cx,
                ))
                .child(self.footnote("Keep this repository open.", cx));
        } else if self.land_loading {
            card = card
                .child(self.plan_heading("Checking readiness…", cx))
                .child(self.plan_text("Reading checks and reviews from GitHub.", cx))
                .child(self.skeleton(cx));
        } else if let Some(outcome) = &self.land_outcome {
            let remote = self
                .land
                .as_ref()
                .map_or_else(|| self.job_remote(cx), |plan| plan.remote.clone());
            let steps = outcome
                .steps
                .iter()
                .map(|step| {
                    (
                        step.status,
                        land_action_text(&step.action, &remote),
                        step.detail.clone(),
                    )
                })
                .collect();
            card = card
                .child(self.plan_heading(
                    if outcome.failed {
                        "Round interrupted"
                    } else {
                        "Round complete"
                    },
                    cx,
                ))
                .child(self.plan_text(
                    if outcome.failed {
                        "Completed steps are preserved. Check the result and try again."
                    } else {
                        "Check the stack for its next landing step."
                    },
                    cx,
                ))
                .child(self.result_steps(steps, cx))
                .child(h_flex().mt(px(8.)).mb(px(16.)).child(
                    btn("land-again", "Check again").on_click(cx.listener(|this, _, _, cx| {
                        if let Some(head) = this.land_for.clone() {
                            this.load_land_plan(head, cx);
                        }
                    })),
                ));
        } else if let Some(plan) = &self.land {
            let candidate = plan
                .segments
                .iter()
                .find(|segment| {
                    matches!(
                        segment.status,
                        LandSegmentStatus::Landing | LandSegmentStatus::Waiting
                    )
                })
                .and_then(|segment| segment.pr.as_ref());
            let needs_user = plan.blockers.iter().any(|blocker| !blocker.wait);
            card = card
                .child(self.plan_heading(
                    if needs_user {
                        "Before this can land"
                    } else if !plan.blockers.is_empty() {
                        "Waiting on GitHub"
                    } else {
                        "Land this stack"
                    },
                    cx,
                ))
                .child(self.plan_text(
                    if stack.pr_count > 1 {
                        format!(
                            "Land {} pull requests in order, rebasing the stack as it goes.",
                            stack.pr_count
                        )
                    } else {
                        format!(
                            "Merge into {} and clean up the local branch.",
                            plan.base_branch
                        )
                    },
                    cx,
                ));
            if let Some(pr) = candidate.filter(|pr| {
                pr.review == ReviewDecision::ReviewRequired
                    && !plan.options.bypass
                    && plan.blockers.is_empty()
            }) {
                card = card.child(self.note(
                    format!("#{} is waiting for review", pr.number),
                    true,
                    cx,
                ));
            }
            if stack
                .segments
                .iter()
                .any(|segment| segment.pr.as_ref().is_some_and(|pr| pr.is_draft))
            {
                card = card.child(self.note(
                    "Draft PRs will be marked ready as they land.",
                    false,
                    cx,
                ));
            }
            if !plan.blockers.is_empty() {
                card = card.child(
                    self.blockers(
                        plan.blockers
                            .iter()
                            .map(|blocker| (blocker.message.clone(), blocker.wait))
                            .collect(),
                        cx,
                    ),
                );
            }
            if plan.options.bypass {
                card = card.child(self.note(
                    "Bypass is on. This attempts to merge past checks, reviews, and merge queues.",
                    true,
                    cx,
                ));
            }
            let queueable = can_queue(plan) && !job_active;
            let mut go = self.stacked();
            if queueable {
                go = go.child(
                    btn(
                        "autoland-queue",
                        if self.queueing {
                            "Starting…"
                        } else {
                            "Auto-land when ready"
                        },
                    )
                    .primary()
                    .large()
                    .trailing(icon(IconName::ArrowRight, 14., t.accent_contrast))
                    .disabled(self.queueing || executing)
                    .on_click(cx.listener(|this, _, _, cx| this.queue_autoland(cx))),
                );
            }
            if !plan.actions.is_empty() {
                let mut land = btn(
                    "land-go",
                    if self.landing {
                        "Landing…"
                    } else {
                        manual_land_label(plan)
                    },
                )
                .large()
                .disabled(executing || !plan.blockers.is_empty() || job_active)
                .on_click(cx.listener(|this, _, _, cx| this.land_stack(cx)));
                if !queueable {
                    land = land.primary();
                }
                go = go.child(land);
            }
            card = card.child(go);
            if job_active {
                card = card.child(self.footnote(
                    "Another stack is auto-landing. Stop it to start this one.",
                    cx,
                ));
            } else if queueable {
                card = card.child(self.footnote("Runs while Jiji is open. Stop at any time.", cx));
            }

            let view = cx.entity().downgrade();
            let method = self.land_method;
            let bypass = self.land_bypass;
            card = card.child(self.disclosure(
                "land-settings",
                "Merge settings",
                Some(method.map_or("Auto", method_word).to_owned()),
                |cx| {
                    v_flex()
                        .gap(px(8.))
                        .pt(px(4.))
                        .pb(px(8.))
                        .text_size(px(TEXT_S))
                        .text_color(t.text2)
                        .child("Merge method")
                        .child(segmented(
                            "land-method",
                            vec![
                                (None, "Auto".into(), None),
                                (Some(MergeMethod::Squash), "Squash".into(), None),
                                (Some(MergeMethod::Rebase), "Rebase".into(), None),
                                (Some(MergeMethod::Merge), "Merge".into(), None),
                            ],
                            &method,
                            cx,
                            move |picked, _, cx| {
                                let picked = *picked;
                                let _ = view.update(cx, |this, cx| {
                                    if this.executing() {
                                        return;
                                    }
                                    this.land_method = picked;
                                    this.replan_land(cx);
                                });
                            },
                        ))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(checkbox(
                                    "land-bypass",
                                    Some(bypass),
                                    cx,
                                    cx.listener(|this, _, _, cx| {
                                        if this.executing() {
                                            return;
                                        }
                                        this.land_bypass = !this.land_bypass;
                                        this.replan_land(cx);
                                    }),
                                ))
                                .child("Bypass checks & reviews"),
                        )
                        .into_any_element()
                },
                cx,
            ));
            if !plan.actions.is_empty() {
                let steps: Vec<String> = plan
                    .actions
                    .iter()
                    .map(|action| land_action_text(action, &plan.remote))
                    .collect();
                let count = steps.len();
                card = card.child(self.disclosure(
                    "land-round",
                    "This round",
                    Some(format!("{count} step{}", plural(count))),
                    |cx| self.action_list(steps, cx),
                    cx,
                ));
            }
            if !plan.warnings.is_empty() {
                let warnings = plan.warnings.clone();
                card = card.child(self.disclosure(
                    "land-notes",
                    "Notes",
                    Some(warnings.len().to_string()),
                    |cx| {
                        let mut notes = v_flex().pt(px(4.));
                        for warning in warnings {
                            notes = notes.child(self.note(warning, true, cx));
                        }
                        notes.into_any_element()
                    },
                    cx,
                ));
            }
        }
        if let Some(error) = &self.land_error {
            card =
                card.child(self.error_line(error.clone(), cx))
                    .child(
                        h_flex()
                            .mb(px(16.))
                            .child(btn("land-retry", "Try again").on_click(cx.listener(
                                |this, _, _, cx| {
                                    if let Some(head) = this.land_for.clone() {
                                        this.load_land_plan(head, cx);
                                    }
                                },
                            ))),
                    );
        }
        if let Some(error) = &self.queue_error {
            card = card.child(self.error_line(error.clone(), cx));
        }
        card
    }

    fn render_submit_plan(&self, stack: &PublishStack, cx: &mut Context<Self>) -> gpui::Div {
        let executing = self.executing();
        let job_active = self.job_active(cx);
        let recheck = |this: &mut Self, cx: &mut Context<Self>| {
            if let Some(head) = this
                .selected_stack()
                .and_then(|stack| stack.submit_head.clone())
            {
                this.load_plan(head, cx);
            }
        };
        let mut card = v_flex();
        if self.plan_loading {
            card = card
                .child(self.plan_heading("Checking for updates…", cx))
                .child(self.plan_text("Comparing your stack with GitHub.", cx))
                .child(self.skeleton(cx));
        } else if let Some(outcome) = &self.outcome {
            let remote = self
                .plan
                .as_ref()
                .map_or_else(|| self.job_remote(cx), |plan| plan.remote.clone());
            let steps = outcome
                .steps
                .iter()
                .map(|step| {
                    (
                        step.status,
                        submit_action_text(&step.action, &remote),
                        step.detail.clone(),
                    )
                })
                .collect();
            card = card
                .child(self.plan_heading(
                    if outcome.failed {
                        "Publishing interrupted"
                    } else {
                        "Stack published"
                    },
                    cx,
                ))
                .child(self.result_steps(steps, cx))
                .child(
                    h_flex().mt(px(8.)).mb(px(16.)).child(
                        btn("submit-again", "Check again")
                            .on_click(cx.listener(move |this, _, _, cx| recheck(this, cx))),
                    ),
                );
        } else if let Some(plan) = &self.plan {
            let idle = plan.actions.is_empty() && plan.blockers.is_empty();
            card = card
                .child(self.plan_heading(
                    if idle {
                        "All up to date"
                    } else if stack.pr_count > 0 {
                        "Update this stack"
                    } else {
                        "Publish this stack"
                    },
                    cx,
                ))
                .child(self.plan_text(
                    if idle {
                        "Your local stack and GitHub are in sync."
                    } else {
                        "Push your changes and keep the pull requests connected."
                    },
                    cx,
                ));
            if !plan.blockers.is_empty() {
                card = card.child(self.blockers(
                    plan.blockers.iter().map(|b| (b.clone(), false)).collect(),
                    cx,
                ));
            }
            if !plan.actions.is_empty() {
                let steps = plan
                    .actions
                    .iter()
                    .map(|action| submit_action_text(action, &plan.remote))
                    .collect();
                card = card.child(self.action_list(steps, cx)).child(
                    self.stacked().child(
                        btn(
                            "submit-go",
                            if self.publishing {
                                "Publishing…"
                            } else if stack.pr_count > 0 {
                                "Publish updates"
                            } else {
                                "Publish stack"
                            },
                        )
                        .primary()
                        .large()
                        .disabled(executing || !plan.blockers.is_empty() || job_active)
                        .on_click(cx.listener(|this, _, _, cx| this.publish(cx))),
                    ),
                );
            }
            if let Some(preview) = &plan.stack_comment_preview {
                let text = comment_preview(preview);
                let t = theme(cx);
                card = card.child(self.disclosure(
                    "submit-comment",
                    "Stack comment preview",
                    None,
                    |_| {
                        div()
                            .my(px(4.))
                            .p(px(10.))
                            .rounded(px(RADIUS_M))
                            .bg(t.bg_well)
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_XS))
                            .line_height(gpui::relative(1.5))
                            .text_color(t.text2)
                            .child(text)
                            .into_any_element()
                    },
                    cx,
                ));
            }
            if let Some(path) = &plan.pr_template_path {
                card = card.child(self.footnote(format!("Uses {path}."), cx).mt(px(8.)));
            }
            if !plan.warnings.is_empty() {
                let warnings = plan.warnings.clone();
                card = card.child(self.disclosure(
                    "submit-notes",
                    "Notes",
                    Some(warnings.len().to_string()),
                    |cx| {
                        let mut notes = v_flex().pt(px(4.));
                        for warning in warnings {
                            notes = notes.child(self.note(warning, true, cx));
                        }
                        notes.into_any_element()
                    },
                    cx,
                ));
            }
        }
        if let Some(error) = &self.plan_error {
            card = card.child(self.error_line(error.clone(), cx)).child(
                h_flex().mb(px(16.)).child(
                    btn("submit-retry", "Try again")
                        .on_click(cx.listener(move |this, _, _, cx| recheck(this, cx))),
                ),
            );
        }
        card
    }

    fn render_ship_plan(&self, cx: &mut Context<Self>) -> gpui::Div {
        let executing = self.executing();
        let job_active = self.job_active(cx);
        let mut card = v_flex();
        if self.ship_loading {
            card = card
                .child(self.plan_heading("Checking trunk…", cx))
                .child(self.plan_text("Fetching the latest remote state.", cx))
                .child(self.skeleton(cx));
        } else if let Some(outcome) = &self.ship_outcome {
            let steps = outcome
                .steps
                .iter()
                .map(|step| {
                    (
                        step.status,
                        ship_action_text(&step.action),
                        step.detail.clone(),
                    )
                })
                .collect();
            card = card
                .child(self.plan_heading(
                    if outcome.failed {
                        "Shipping interrupted"
                    } else {
                        "Shipped to trunk"
                    },
                    cx,
                ))
                .child(self.result_steps(steps, cx));
        } else if let Some(plan) = &self.ship {
            card = card
                .child(self.plan_heading(format!("Ship to {}", plan.trunk_bookmark), cx))
                .child(self.plan_text(
                    "Push this stack directly to trunk. This skips pull request review.",
                    cx,
                ));
            if !plan.blockers.is_empty() {
                card = card.child(self.blockers(
                    plan.blockers.iter().map(|b| (b.clone(), false)).collect(),
                    cx,
                ));
            }
            if !plan.actions.is_empty() {
                card = card.child(
                    self.action_list(plan.actions.iter().map(ship_action_text).collect(), cx),
                );
            }
            for warning in &plan.warnings {
                card = card.child(self.note(warning.clone(), true, cx));
            }
            if !plan.actions.is_empty() {
                card = card.child(
                    self.stacked().child(
                        btn(
                            "ship-go",
                            if self.shipping {
                                "Shipping…".to_owned()
                            } else {
                                format!("Ship to {}", plan.trunk_bookmark)
                            },
                        )
                        .primary()
                        .large()
                        .disabled(executing || !plan.blockers.is_empty() || job_active)
                        .on_click(cx.listener(|this, _, _, cx| this.ship_stack(cx))),
                    ),
                );
            } else if plan.blockers.is_empty() {
                card = card.child(self.footnote("Already shipped.", cx));
            }
        }
        if let Some(error) = &self.ship_error {
            card = card.child(self.error_line(error.clone(), cx));
        }
        card
    }

    fn render_action_column(
        &mut self,
        stack: &PublishStack,
        snapshot: &RepoSnapshot,
        watching: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let connected = self.connected(cx);
        let executing = self.executing();
        let mut head = h_flex()
            .flex_shrink_0()
            .h(px(TOOLBAR_H))
            .gap(px(8.))
            .px(px(12.))
            .border_b_1()
            .border_color(t.border2);
        if connected && stack.submit_head.is_some() {
            let mut options: Vec<(Mode, SharedString, Option<IconName>)> = Vec::new();
            if stack.head_bookmark.is_some() && stack.pr_count > 0 {
                options.push((Mode::Land, "Land".into(), None));
            }
            options.push((
                Mode::Publish,
                if stack.pr_count > 0 {
                    "Update"
                } else {
                    "Publish"
                }
                .into(),
                None,
            ));
            let view = cx.entity().downgrade();
            let (bookmark, submit) = (stack.head_bookmark.clone(), stack.submit_head.clone());
            head = head.child(segmented(
                "stack-mode",
                options,
                &self.mode,
                cx,
                move |mode, _, cx| {
                    let mode = *mode;
                    let (bookmark, submit) = (bookmark.clone(), submit.clone());
                    let _ = view.update(cx, |this, cx| {
                        if this.executing() {
                            return;
                        }
                        match (mode, bookmark, submit) {
                            (Mode::Land, Some(bookmark), _) => this.load_land_plan(bookmark, cx),
                            (Mode::Publish, _, Some(head)) => this.load_plan(head, cx),
                            _ => {}
                        }
                    });
                },
            ));
            if let Some(head_change) = stack.head_change.clone() {
                let mut more = div().relative().child(
                    btn("stack-more", "More")
                        .ghost()
                        .on(self.mode == Mode::Ship || self.more.is_open())
                        .trailing(icon(IconName::ChevronDown, 12., t.text3))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.more.toggle();
                            cx.notify();
                        })),
                );
                if self.more.is_open() {
                    let trunk = snapshot.trunk_bookmark.clone();
                    let mut ship = menu_row(
                        "stack-ship",
                        Some(IconName::ArrowUp),
                        format!("Ship directly to {trunk}"),
                        cx,
                    );
                    ship = if executing {
                        ship.opacity(0.5)
                    } else {
                        ship.on_click(cx.listener(move |this, _, _, cx| {
                            this.load_ship_plan(head_change.clone(), cx)
                        }))
                    };
                    more = more.child(
                        div().absolute().top(px(30.)).right_0().child(floating(
                            Corner::TopRight,
                            popover(cx)
                                .swallow_clicks()
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    this.more.close();
                                    cx.notify();
                                }))
                                .min_w(px(220.))
                                .p(px(4.))
                                .child(ship),
                        )),
                    );
                }
                head = head.child(div().flex_1()).child(more);
            }
        } else {
            head = head.child(
                div()
                    .pl(px(4.))
                    .text_size(px(TEXT_S))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text2)
                    .child("Next step"),
            );
        }

        let plan = if !connected {
            v_flex()
                .child(self.plan_heading("Connect to publish", cx))
                .child(self.plan_text("Sign in to see checks, reviews, and landing options.", cx))
                .child(
                    self.stacked().child(
                        btn("plan-connect", "Connect GitHub")
                            .primary()
                            .large()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.connection.open();
                                cx.notify();
                            })),
                    ),
                )
        } else if stack.submit_head.is_none() {
            let pr = stack.segments.last().and_then(|segment| segment.pr.clone());
            v_flex()
                .child(self.plan_heading("Bring this stack home", cx))
                .child(self.plan_text(
                    "This stack is on GitHub. Fetch its head to review it locally.",
                    cx,
                ))
                .child(
                    self.stacked().child(
                        btn("plan-fetch", "Fetch for review")
                            .primary()
                            .large()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(pr) = pr.clone() {
                                    this.page = Page::Review;
                                    this.open_review_panel(pr, window, cx);
                                }
                            })),
                    ),
                )
        } else {
            match self.mode {
                Mode::Land => self.render_land_plan(stack, watching, cx),
                Mode::Publish => self.render_submit_plan(stack, cx),
                Mode::Ship => self.render_ship_plan(cx),
            }
        };
        v_flex()
            .w(px(ACTIONS_W))
            .flex_shrink_0()
            .min_h_0()
            .border_l_1()
            .border_color(t.border2)
            .child(head)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("plan-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.plan_scroll)
                            .p(px(16.))
                            .child(plan),
                    )
                    .child(scrollbar("plan-scrollbar", &self.plan_scroll)),
            )
    }

    fn render_detail(
        &mut self,
        stack: &PublishStack,
        snapshot: &RepoSnapshot,
        jobs: &[AutoLandStatus],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let (label, tone) = stack_summary(stack.status);
        let job = jobs.iter().find(|status| {
            status.live && Some(&status.record.state.head_bookmark) == stack.head_bookmark.as_ref()
        });
        // The live job's segments lead; else the plan on screen overlays
        // its own.
        let landing: Vec<LandSegment> = match (job, &self.land) {
            (Some(job), _) => job.record.state.segments.clone(),
            (None, Some(plan)) if self.land_for.as_ref() == stack.head_bookmark.as_ref() => {
                plan.segments.clone()
            }
            _ => Vec::new(),
        };
        let fact = || h_flex().flex_shrink_0().gap(px(6.));
        // The stack's header, two bars like a change's: the title and its
        // one verb, then where the stack stands.
        let mut title_bar = h_flex()
            .h(px(TOOLBAR_H))
            .gap(px(8.))
            .pl(px(16.))
            .pr(px(8.))
            .border_b_1()
            .border_color(t.border2)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(TEXT_L))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text1)
                    .child(truncated(stack.title.clone())),
            );
        if let Some(head) = stack.head_change.clone() {
            let app = self.app.clone();
            title_bar = title_bar.child(
                btn("stack-workbench", "Workbench")
                    .ghost()
                    .icon(IconName::Workbench)
                    .tooltip("Open this stack on the workbench")
                    .on_click(move |_, _, cx| {
                        app.update(cx, |model, cx| model.jump_to_change(&head, cx))
                    }),
            );
        }
        let mut meta_bar = h_flex()
            .min_h(px(30.))
            .gap(px(12.))
            .pl(px(16.))
            .pr(px(8.))
            .overflow_hidden()
            .text_size(px(TEXT_S))
            .text_color(t.text3)
            .child(
                fact()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.text2)
                    .child(dot(tone_color(&t, tone), 6.))
                    .child(label),
            )
            .child(div().w(px(1.)).h(px(11.)).flex_shrink_0().bg(t.border1))
            .child(fact().child(format!(
                "{} pull request{}",
                stack.pr_count,
                plural(stack.pr_count as usize)
            )));
        if stack.change_count > 0 {
            meta_bar = meta_bar.child(fact().child(format!(
                "{} change{}",
                stack.change_count,
                plural(stack.change_count as usize)
            )));
        }
        meta_bar = meta_bar.child(
            fact().child("into").child(
                div()
                    .h(px(17.))
                    .px(px(5.))
                    .flex()
                    .items_center()
                    .rounded(px(RADIUS_S))
                    .bg(t.accent_dim)
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_XS))
                    .text_color(t.accent_strong)
                    .child(if stack.base.is_empty() {
                        snapshot.trunk_bookmark.clone()
                    } else {
                        stack.base.clone()
                    }),
            ),
        );
        if stack.is_active {
            meta_bar = meta_bar.child(fact().child(dot(t.working_copy, 6.)).child("Current stack"));
        } else if stack.submit_head.is_none() {
            meta_bar = meta_bar.child(fact().child("On GitHub only"));
        }
        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(t.border2)
                    .child(title_bar)
                    .child(meta_bar),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_chain(stack, &landing, cx))
                    .child(self.render_action_column(stack, snapshot, job.is_some(), cx)),
            )
            .into_any_element()
    }

    pub(super) fn render_stacks_page(
        &mut self,
        snapshot: &RepoSnapshot,
        jobs: &[AutoLandStatus],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = match self.selected_stack().cloned() {
            Some(stack) => self.render_detail(&stack, snapshot, jobs, cx),
            None => div()
                .flex_1()
                .min_w_0()
                .child(if self.stacks.is_empty() {
                    empty_state(
                        IconName::Stack,
                        "A clear path to shipped",
                        "Your next change starts on the workbench. Published stacks will appear \
                         here, ready to land.",
                        None,
                        cx,
                    )
                } else {
                    empty_state(
                        IconName::Stack,
                        "Choose a stack",
                        "Select a stack to see its pull requests and next steps.",
                        None,
                        cx,
                    )
                })
                .into_any_element(),
        };
        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .child(self.render_browser(cx))
            .child(detail)
            .into_any_element()
    }
}
