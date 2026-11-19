//! The Stacks page: the stack browser on the left, and for the selected
//! stack its landing-order ledger beside the plan card that acts on it.

mod browser;
mod ledger;
mod plan;

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, Window};
use jiji_core::snapshot::RepoSnapshot;
use jiji_forge::{AutoLandStatus, LandSegment, PublishStack, SubmitHead};

use super::{Mode, Page, PublishView};
use crate::logic::publish::stack_summary;
use crate::logic::text::plural_s;
use crate::theme::{theme, TEXT_L, TEXT_S, TOOLBAR_H};
use crate::ui::{btn, dot, empty_state, h_flex, truncated, trunk_label, v_flex, IconName};

const BROWSER_W: f32 = 280.;
const ACTIONS_W: f32 = 300.;

impl PublishView {
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
        self.more.reset();
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

    pub(super) fn publish(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn replan_land(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn land_stack(&mut self, cx: &mut Context<Self>) {
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
        self.more.reset();
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

    pub(super) fn ship_stack(&mut self, cx: &mut Context<Self>) {
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
                    .child(dot(t.tone(tone), 6.))
                    .child(label),
            )
            .child(div().w(px(1.)).h(px(11.)).flex_shrink_0().bg(t.border1))
            .child(fact().child(format!(
                "{} pull request{}",
                stack.pr_count,
                plural_s(stack.pr_count as usize)
            )));
        if stack.change_count > 0 {
            meta_bar = meta_bar.child(fact().child(format!(
                "{} change{}",
                stack.change_count,
                plural_s(stack.change_count as usize)
            )));
        }
        meta_bar = meta_bar.child(fact().child("into").child(trunk_label(
            if stack.base.is_empty() {
                snapshot.trunk_bookmark.clone()
            } else {
                stack.base.clone()
            },
            cx,
        )));
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
