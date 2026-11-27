//! Taking over someone else's pull request, and giving it back: the
//! question asked on the pull request's row, and the card in the action
//! column that says what it will do and waits to be confirmed.

use gpui::{div, prelude::*, px, Context};
use jiji_forge::{PrSummary, TakeOverPlan};

use crate::logic::publish::{other_pr, take_over_action_text};
use crate::theme::{theme, TEXT_S};
use crate::ui::{btn, h_flex, v_flex};
use crate::views::publish::PublishView;

impl PublishView {
    /// The pull request of this stack that was taken over as `number`.
    fn taken_over_pr(&self, number: u64) -> Option<PrSummary> {
        self.stacks
            .iter()
            .flat_map(|stack| &stack.segments)
            .find(|segment| {
                segment
                    .taken_over
                    .as_ref()
                    .is_some_and(|taken| taken.number == number)
            })
            .and_then(|segment| segment.pr.clone())
    }

    /// Ask about taking over the pull request `number`. Its plan opens in
    /// the action column, and nothing runs until it is confirmed there.
    pub(in crate::views::publish) fn ask_take_over(&mut self, number: u64, cx: &mut Context<Self>) {
        self.ask(number, false, cx);
    }

    /// The same for giving back one that was taken over.
    pub(in crate::views::publish) fn ask_give_back(&mut self, number: u64, cx: &mut Context<Self>) {
        self.ask(number, true, cx);
    }

    fn ask(&mut self, number: u64, giving_back: bool, cx: &mut Context<Self>) {
        if self.executing() {
            return;
        }
        let Some(repo) = self.repo_path(cx) else {
            return;
        };
        self.takeover_seq += 1;
        let seq = self.takeover_seq;
        self.takeover_for = Some(number);
        self.takeover = None;
        self.takeover_outcome = None;
        self.takeover_error = None;
        self.takeover_loading = true;
        cx.notify();
        let pr = giving_back.then(|| self.taken_over_pr(number)).flatten();
        let planned = self.app.read(cx).bg(cx, move |host| {
            if giving_back {
                host.give_back_plan(&repo, number, pr.as_ref())
            } else {
                host.take_over_plan(&repo, number)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = planned.await;
            let _ = this.update(cx, |this, cx| {
                if this.takeover_seq != seq {
                    return;
                }
                this.takeover_loading = false;
                match result {
                    Ok(plan) => this.takeover = Some(plan),
                    Err(error) => this.takeover_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run the plan on the card. A take-over puts their pull request in a
    /// stack under its own branch's name, and that stack is shown next.
    pub(in crate::views::publish) fn confirm_take_over(&mut self, cx: &mut Context<Self>) {
        let (Some(plan), Some(repo)) = (self.takeover.clone(), self.repo_path(cx)) else {
            return;
        };
        if self.executing() || !plan.blockers.is_empty() {
            return;
        }
        self.taking_over = true;
        self.takeover_error = None;
        cx.notify();
        let confirmed = plan.clone();
        let ran = self
            .app
            .read(cx)
            .bg(cx, move |host| host.take_over(&repo, &confirmed));
        cx.spawn(async move |this, cx| {
            let result = ran.await;
            let _ = this.update(cx, |this, cx| {
                this.taking_over = false;
                match result {
                    Ok(outcome) => {
                        if !outcome.failed && !plan.giving_back {
                            this.follow = Some(plan.branch.clone());
                        }
                        this.takeover_outcome = Some(outcome);
                        // The stacks are read again for what GitHub has.
                        this.app.update(cx, |model, cx| model.refresh_forge_prs(cx));
                    }
                    Err(error) => this.takeover_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Close the card: nothing was run, or what ran has been read.
    pub(in crate::views::publish) fn dismiss_take_over(&mut self, cx: &mut Context<Self>) {
        if self.taking_over {
            return;
        }
        self.takeover_seq += 1;
        self.takeover_for = None;
        self.takeover = None;
        self.takeover_outcome = None;
        self.takeover_error = None;
        self.takeover_loading = false;
        cx.notify();
    }

    fn take_over_heading(plan: &TakeOverPlan) -> String {
        let who = other_pr(plan.number, plan.author.as_deref());
        if plan.giving_back {
            format!("Give back {who}")
        } else {
            format!("Take over {who}")
        }
    }

    /// The card in the action column: what taking over, or giving back,
    /// will do and what it means, with the one button that runs it.
    pub(super) fn render_take_over(&self, cx: &mut Context<Self>) -> gpui::Div {
        let t = theme(cx);
        let mut card = v_flex();
        let close = |label: &'static str, cx: &mut Context<Self>| {
            btn("take-over-close", label)
                .on_click(cx.listener(|this, _, _, cx| this.dismiss_take_over(cx)))
        };
        if self.takeover_loading {
            return card
                .child(self.plan_heading("Reading the pull request…", cx))
                .child(self.plan_text("Asking GitHub for it as it is now.", cx))
                .child(self.skeleton(cx));
        }
        let Some(plan) = &self.takeover else {
            if let Some(error) = &self.takeover_error {
                card = card
                    .child(self.plan_heading("Could not read the pull request", cx))
                    .child(self.error_line(error.clone(), cx));
            }
            return card.child(h_flex().mb(px(16.)).child(close("Close", cx)));
        };
        if let Some(outcome) = &self.takeover_outcome {
            let steps = outcome
                .steps
                .iter()
                .map(|step| {
                    (
                        step.status,
                        take_over_action_text(&step.action),
                        step.detail.clone(),
                    )
                })
                .collect();
            let who = other_pr(plan.number, plan.author.as_deref());
            return card
                .child(self.plan_heading(
                    match (outcome.failed, plan.giving_back) {
                        (true, false) => "Taking over interrupted".to_owned(),
                        (true, true) => "Giving back interrupted".to_owned(),
                        (false, false) => format!("Took over {who}"),
                        (false, true) => format!("Gave back {who}"),
                    },
                    cx,
                ))
                .child(self.result_steps(steps, cx))
                .child(h_flex().mt(px(8.)).mb(px(16.)).child(close("Done", cx)));
        }
        card = card
            .child(self.plan_heading(Self::take_over_heading(plan), cx))
            .child(self.plan_text(
                match (plan.title.is_empty(), &plan.base_branch) {
                    (false, Some(base)) => {
                        format!("\u{201c}{}\u{201d} · {} → {base}", plan.title, plan.branch)
                    }
                    (false, None) => format!("\u{201c}{}\u{201d} · {}", plan.title, plan.branch),
                    (true, _) => plan.branch.clone(),
                },
                cx,
            ));
        if !plan.blockers.is_empty() {
            card = card.child(self.blockers(
                plan.blockers.iter().map(|b| (b.clone(), false)).collect(),
                cx,
            ));
            return card.child(h_flex().mb(px(16.)).child(close("Close", cx)));
        }
        let steps = plan.actions.iter().map(take_over_action_text).collect();
        // The steps, the button that runs them, and under it what they
        // mean, in full and in view: nothing of it is folded away.
        card = card.child(self.action_list(steps, cx)).child(
            self.stacked()
                .child(
                    btn(
                        "take-over-go",
                        match (self.taking_over, plan.giving_back) {
                            (true, false) => "Taking over…",
                            (true, true) => "Giving back…",
                            (false, false) => "Take over",
                            (false, true) => "Give back",
                        },
                    )
                    .primary()
                    .large()
                    .disabled(self.executing())
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_take_over(cx))),
                )
                .child(
                    btn("take-over-close", "Cancel")
                        .large()
                        .disabled(self.taking_over)
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_take_over(cx))),
                ),
        );
        if let Some(error) = &self.takeover_error {
            card = card.child(self.error_line(error.clone(), cx));
        }
        let mut notes = v_flex()
            .pt(px(10.))
            .border_t_1()
            .border_color(t.border2)
            .child(
                div()
                    .mb(px(8.))
                    .text_size(px(TEXT_S))
                    .text_color(t.text3)
                    .child("What this means"),
            );
        for note in &plan.warnings {
            notes = notes.child(self.note(note.clone(), false, cx));
        }
        card.child(notes)
    }
}
