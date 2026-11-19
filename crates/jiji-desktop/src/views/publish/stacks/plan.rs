//! The plan card: what publishing, landing or shipping the selected
//! stack will do, the blockers in its way, and the action column that
//! holds it.

use gpui::{div, prelude::*, px, Anchor, AnyElement, Context, FontWeight, SharedString};
use jiji_core::snapshot::RepoSnapshot;
use jiji_forge::{LandSegmentStatus, MergeMethod, PublishStack, ReviewDecision};

use super::ACTIONS_W;
use crate::logic::autoland::can_queue;
use crate::logic::publish::{
    comment_preview, land_action_text, manual_land_label, method_word, ship_action_text,
    submit_action_text,
};
use crate::logic::text::plural_s;
use crate::theme::{theme, Theme, RADIUS_M, RADIUS_S, TEXT_L, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, checkbox, h_flex, icon, menu_row, popover, popover_layer, scrollbar, segmented, v_flex,
    IconName, Segment,
};
use crate::views::publish::{Mode, Page, PublishView};

impl PublishView {
    pub(super) fn plan_heading(
        &self,
        text: impl Into<SharedString>,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(6.))
            .text_size(px(TEXT_L))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.text1)
            .child(text.into())
    }

    pub(super) fn plan_text(&self, text: impl Into<SharedString>, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(12.))
            .text_size(px(TEXT_S))
            .line_height(gpui::relative(1.55))
            .text_color(t.text2)
            .child(text.into())
    }

    pub(super) fn footnote(&self, text: impl Into<SharedString>, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(12.))
            .text_size(px(TEXT_XS))
            .line_height(gpui::relative(1.5))
            .text_color(t.text3)
            .child(text.into())
    }

    pub(super) fn note(
        &self,
        text: impl Into<SharedString>,
        warn: bool,
        cx: &Context<Self>,
    ) -> gpui::Div {
        let t = theme(cx);
        div()
            .mb(px(8.))
            .text_size(px(TEXT_S))
            .line_height(gpui::relative(1.5))
            .text_color(if warn { t.warn } else { t.text2 })
            .child(text.into())
    }

    pub(super) fn error_line(&self, text: String, cx: &Context<Self>) -> gpui::Div {
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
    pub(super) fn blockers(&self, items: Vec<(String, bool)>, cx: &Context<Self>) -> gpui::Div {
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
    pub(super) fn action_list(&self, steps: Vec<String>, cx: &Context<Self>) -> AnyElement {
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

    pub(super) fn skeleton(&self, cx: &Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let bar = |width: f32| {
            div()
                .h(px(10.))
                .my(px(8.))
                .w(gpui::relative(width))
                .rounded(px(RADIUS_S))
                .bg(Theme::fade(t.text1, 0.08))
        };
        crate::ui::motion::breathing("plan-skeleton", v_flex().child(bar(1.)).child(bar(0.62)))
    }

    pub(super) fn stacked(&self) -> gpui::Div {
        v_flex().gap(px(8.)).mt(px(16.)).mb(px(12.))
    }

    pub(super) fn render_land_plan(
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
                            "Merge method",
                            vec![
                                Segment::text(None, "Auto"),
                                Segment::text(Some(MergeMethod::Squash), "Squash"),
                                Segment::text(Some(MergeMethod::Rebase), "Rebase"),
                                Segment::text(Some(MergeMethod::Merge), "Merge"),
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
                                    "Bypass checks & reviews",
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
                    Some(format!("{count} step{}", plural_s(count))),
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

    pub(super) fn render_submit_plan(
        &self,
        stack: &PublishStack,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
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

    pub(super) fn render_ship_plan(&self, cx: &mut Context<Self>) -> gpui::Div {
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

    pub(super) fn render_action_column(
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
            let mut options = Vec::new();
            if stack.head_bookmark.is_some() && stack.pr_count > 0 {
                options.push(Segment::text(Mode::Land, "Land"));
            }
            options.push(Segment::text(
                Mode::Publish,
                if stack.pr_count > 0 {
                    "Update"
                } else {
                    "Publish"
                },
            ));
            let view = cx.entity().downgrade();
            let (bookmark, submit) = (stack.head_bookmark.clone(), stack.submit_head.clone());
            head = head.child(segmented(
                "stack-mode",
                "Stack action",
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
                        .expanded(self.more.is_open())
                        .tooltip("More stack actions")
                        .trailing(icon(IconName::ChevronDown, 12., t.text3))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.more.toggle(window, cx);
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
                        ship.on_click(cx.listener(move |this, _, window, cx| {
                            this.more.close(window, cx);
                            this.load_ship_plan(head_change.clone(), cx)
                        }))
                    };
                    more = more.child(popover_layer(
                        Anchor::TopRight,
                        30.,
                        self.more.panel(
                            "stack-more-menu",
                            gpui::Role::Menu,
                            "More stack actions",
                            popover(cx).min_w(px(220.)).p(px(4.)).child(ship),
                        ),
                        cx.listener(|this, _, window, cx| {
                            this.more.close(window, cx);
                            cx.notify();
                        }),
                    ));
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
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.connection.open(window, cx);
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
                            .scroll_y()
                            .track_scroll(&self.plan_scroll)
                            .p(px(16.))
                            .child(plan),
                    )
                    .child(scrollbar("plan-scrollbar", &self.plan_scroll)),
            )
    }
}
