use crate::Result;
use jiji_forge::*;
use serde::Serialize;
use std::io::{self, Write};

pub struct Output {
    pub json: bool,
}
impl Output {
    pub fn preflight(&mut self, text: &str) -> Result<()> {
        if !self.json {
            let mut stdout = io::stdout().lock();
            stdout.write_all(text.as_bytes())?;
            stdout.flush()?;
        }
        Ok(())
    }
    pub fn emit(&mut self, value: &impl Serialize, text: impl AsRef<str>) -> Result<()> {
        let text = if self.json {
            format!("{}\n", serde_json::to_string_pretty(value)?)
        } else {
            text.as_ref().to_owned()
        };
        io::stdout().lock().write_all(text.as_bytes())?;
        Ok(())
    }
    pub fn event(&mut self, state: &AutoLandState) -> Result<()> {
        let text = if self.json {
            format!(
                "{}\n",
                serde_json::to_string(&serde_json::json!({"event": "autoLand", "state": state}))?
            )
        } else {
            job(state)
        };
        let mut stdout = io::stdout().lock();
        stdout.write_all(text.as_bytes())?;
        stdout.flush()?;
        Ok(())
    }
}

/// PR titles, descriptions, branch names, and errors are untrusted terminal
/// text. Layout controls come from the renderer, never from forge content.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn stacks(repo: &str, stacks: &[PublishStack], hidden: usize, warnings: &[String]) -> String {
    let mut text = format!(
        "{} · {} stack{}\n\n",
        clean(repo),
        stacks.len(),
        if stacks.len() == 1 { "" } else { "s" }
    );
    for stack in stacks {
        text.push_str(&format!(
            "{} {}\n    {} PRs · {} changes · {} → {}\n    {}\n\n",
            if stack.is_active { "●" } else { " " },
            clean(&stack.key),
            stack.pr_count,
            stack.change_count,
            stack.status.label(),
            clean(&stack.base),
            clean(&stack.title)
        ));
    }
    if stacks.is_empty() {
        text.push_str("No matching stacks.\n");
    }
    if hidden > 0 {
        text.push_str(&format!(
            "{hidden} local stacks hidden; use --all to include them.\n"
        ));
    }
    for warning in warnings {
        text.push_str(&format!("Note: {}\n", clean(warning)));
    }
    text
}

pub fn pr_row(pr: &PrSummary) -> String {
    let mut signals = Vec::new();
    if pr.is_draft {
        signals.push("draft");
    }
    match pr.checks {
        ChecksRollup::Passing => signals.push("checks passed"),
        ChecksRollup::Failing => signals.push("checks failed"),
        ChecksRollup::Pending => signals.push("checks running"),
        ChecksRollup::None => {}
    }
    match pr.review {
        ReviewDecision::Approved => signals.push("approved"),
        ReviewDecision::ChangesRequested => signals.push("changes requested"),
        ReviewDecision::ReviewRequired => signals.push("review needed"),
        ReviewDecision::None => {}
    }
    format!(
        "#{}  {}{}\n",
        pr.number,
        clean(&pr.title),
        if signals.is_empty() {
            String::new()
        } else {
            format!("  [{}]", signals.join(" · "))
        }
    )
}

pub fn stack_detail(stack: &PublishStack) -> String {
    let mut text = format!(
        "{} → {}\n{} PRs · {} changes · {}\n\nLanding order\n  {}\n",
        clean(&stack.key),
        clean(&stack.base),
        stack.pr_count,
        stack.change_count,
        stack.status.label(),
        clean(&stack.base)
    );
    for (index, segment) in stack.segments.iter().enumerate() {
        let title = segment
            .pr
            .as_ref()
            .map(pr_row)
            .unwrap_or_else(|| format!("{}  [unpublished]\n", clean(&segment.title)));
        text.push_str(&format!(
            "  │\n  {}. {}     {} · {} change{}\n",
            index + 1,
            title,
            clean(segment.bookmark.as_deref().unwrap_or("new branch")),
            segment.changes.len(),
            if segment.changes.len() == 1 { "" } else { "s" }
        ));
        if let Some(pr) = &segment.pr {
            text.push_str(&format!("     {}\n", clean(&pr.url)));
        }
    }
    text.push('\n');
    text
}

fn plan_text(
    title: String,
    actions: impl Iterator<Item = String>,
    blockers: impl Iterator<Item = String>,
    warnings: &[String],
) -> String {
    let mut text = format!("{title}\n");
    let mut count = 0;
    for action in actions {
        count += 1;
        text.push_str(&format!("  {count}. {}\n", clean(&action)));
    }
    let mut blocked = false;
    for blocker in blockers {
        blocked = true;
        text.push_str(&format!("  Blocked: {}\n", clean(&blocker)));
    }
    if count == 0 && !blocked {
        text.push_str("  No actions this round.\n");
    }
    for warning in warnings {
        text.push_str(&format!("  Note: {}\n", clean(warning)));
    }
    text
}

pub fn rules(options: LandOptions) -> String {
    format!(
        "method: {} · bypass: {}",
        options.method.map(|m| m.rest_name()).unwrap_or("automatic"),
        if options.bypass {
            "ON (checks, reviews, and queues may be bypassed)"
        } else {
            "off"
        }
    )
}

pub fn land_plan(plan: &LandPlan) -> String {
    plan_text(
        format!(
            "Land {} → {} ({})",
            clean(&plan.head_bookmark),
            clean(&plan.base_branch),
            rules(plan.options)
        ),
        plan.actions.iter().map(land_action),
        plan.blockers.iter().map(|b| b.message.clone()),
        &plan.warnings,
    )
}
pub fn submit_plan(plan: &SubmitPlan) -> String {
    plan_text(
        format!(
            "Publish {} → {} · {} PR segments",
            clean(&plan.head_bookmark),
            clean(&plan.base_branch),
            plan.segments.len()
        ),
        plan.actions.iter().map(submit_action),
        plan.blockers.iter().cloned(),
        &plan.warnings,
    )
}
pub fn ship_plan(plan: &ShipPlan) -> String {
    plan_text(
        format!(
            "Ship {} → {}",
            clean(&plan.head_change),
            clean(&plan.trunk_bookmark)
        ),
        plan.actions.iter().map(ship_action),
        plan.blockers.iter().cloned(),
        &plan.warnings,
    )
}

pub fn readiness_plan(plan: &ReadinessPlan) -> String {
    let head = clean(&plan.head);
    let (title, already) = match plan.target {
        PrReadiness::ReadyForReview => (
            format!("Mark {head} ready for review"),
            "Already ready for review",
        ),
        PrReadiness::Draft if plan.unchanged.len() == 1 => {
            (format!("Convert {head} back to drafts"), "Already a draft")
        }
        PrReadiness::Draft => (format!("Convert {head} back to drafts"), "Already drafts"),
    };
    let mut notes = plan.warnings.clone();
    if !plan.unchanged.is_empty() {
        let numbers: Vec<_> = plan
            .unchanged
            .iter()
            .map(|pr| format!("#{}", pr.number))
            .collect();
        notes.push(format!("{already}: {}", numbers.join(", ")));
    }
    plan_text(
        title,
        plan.actions.iter().map(readiness_action),
        plan.blockers.iter().cloned(),
        &notes,
    )
}

fn readiness_action(action: &ReadinessAction) -> String {
    match action {
        ReadinessAction::MarkReadyPr { number, bookmark } => {
            format!("Mark #{number} ({bookmark}) ready for review")
        }
        ReadinessAction::ConvertPrToDraft { number, bookmark } => {
            format!("Convert #{number} ({bookmark}) to a draft")
        }
    }
}

fn submit_action(action: &SubmitAction) -> String {
    match action {
        SubmitAction::CreateBookmark { bookmark, change } => {
            format!("Create bookmark {bookmark} at {change}")
        }
        SubmitAction::Push { bookmark, .. } => format!("Push {bookmark}"),
        SubmitAction::CreatePr {
            bookmark,
            base,
            title,
            draft,
            ..
        } => format!(
            "Open {}PR: {bookmark} → {base} · {title}",
            if *draft { "draft " } else { "" }
        ),
        SubmitAction::RetargetPr {
            number, to_base, ..
        } => format!("Retarget #{number} to {to_base}"),
        SubmitAction::UpdatePrText { number, seed, .. } => format!(
            "{} #{number}'s managed description",
            if *seed { "Adopt" } else { "Update" }
        ),
        SubmitAction::SyncStackComment { bookmark, .. } => {
            format!("Update the stack comment on {bookmark}")
        }
    }
}
fn land_action(action: &LandAction) -> String {
    match action {
        LandAction::MarkReadyPr { number, .. } => format!("Mark #{number} ready for review"),
        LandAction::MergePr { number, method, .. } => {
            format!("Merge #{number} using {}", method.label())
        }
        LandAction::EnableAutoMerge { number, method, .. } => format!(
            "Enable GitHub auto-merge on #{number} using {}",
            method.label()
        ),
        LandAction::EnqueuePr { number, method, .. } => {
            format!("Enqueue #{number} using {}", method.label())
        }
        LandAction::FetchRemote { remote } => format!("Fetch {remote}"),
        LandAction::RebaseOntoTrunk { root_change, moves } => {
            format!("Rebase {root_change} onto trunk ({moves} changes)")
        }
        LandAction::PushStack { bookmarks } => {
            format!("Push the rebased stack: {}", bookmarks.join(", "))
        }
        LandAction::RetargetPr {
            number, to_base, ..
        } => format!("Retarget #{number} to {to_base}"),
        LandAction::CleanupBookmark { bookmark } => format!("Clean up landed bookmark {bookmark}"),
        LandAction::AbandonLanded {
            bookmark,
            change_ids,
        } => format!(
            "Clean up {} landed changes under {bookmark}",
            change_ids.len()
        ),
    }
}
fn ship_action(action: &ShipAction) -> String {
    match action {
        ShipAction::RebaseOntoTrunk { root_change, .. } => {
            format!("Rebase {root_change} onto trunk")
        }
        ShipAction::MoveTrunk { bookmark, to } => format!("Move {bookmark} to {to}"),
        ShipAction::PushTrunk { bookmark, remote } => format!("Push {bookmark} to {remote}"),
        ShipAction::NewWorkingCopy { on } => format!("Start a new working copy on {on}"),
    }
}
fn step(status: SubmitStepStatus, action: String, detail: Option<&str>) -> String {
    format!(
        "  {} {}{}\n",
        match status {
            SubmitStepStatus::Done => "✓",
            SubmitStepStatus::Failed => "✗",
            SubmitStepStatus::Skipped => "–",
        },
        clean(&action),
        detail
            .map(|s| format!(": {}", clean(s)))
            .unwrap_or_default()
    )
}
pub fn submit_outcome(outcome: &SubmitOutcome) -> String {
    let mut text = String::from("\nResults\n");
    for s in &outcome.steps {
        text.push_str(&step(
            s.status,
            submit_action(&s.action),
            s.detail.as_deref(),
        ));
        if let Some(pr) = &s.pr {
            text.push_str(&format!("    {}\n", clean(&pr.url)));
        }
    }
    text
}
pub fn readiness_outcome(outcome: &ReadinessOutcome) -> String {
    let mut text = String::from("\nResults\n");
    for s in &outcome.steps {
        text.push_str(&step(
            s.status,
            readiness_action(&s.action),
            s.detail.as_deref(),
        ));
    }
    text
}
pub fn land_outcome(outcome: &LandOutcome) -> String {
    let mut text = String::from("\nResults\n");
    for s in &outcome.steps {
        text.push_str(&step(s.status, land_action(&s.action), s.detail.as_deref()));
    }
    text
}
pub fn ship_outcome(outcome: &ShipOutcome) -> String {
    let mut text = String::from("\nResults\n");
    for s in &outcome.steps {
        text.push_str(&step(s.status, ship_action(&s.action), s.detail.as_deref()));
    }
    text
}
pub fn job(state: &AutoLandState) -> String {
    let phase = match &state.phase {
        AutoLandPhase::Waiting { attention, reasons } => format!(
            "{}: {}",
            if *attention {
                "Needs attention"
            } else {
                "Watching"
            },
            reasons.join("; ")
        ),
        AutoLandPhase::Round => format!("Running round {}", state.rounds + 1),
        AutoLandPhase::Done => "Stack landed".into(),
        AutoLandPhase::Stopped => "Stopped; use `jiji watch --resume` to continue".into(),
        AutoLandPhase::Failed { message } => format!("Stopped with an error: {message}"),
    };
    format!(
        "{} · {}\n  {} rounds · {} PRs landed · {}\n",
        clean(&state.head_bookmark),
        clean(&phase),
        state.rounds,
        state.merged.len(),
        rules(state.options)
    )
}
