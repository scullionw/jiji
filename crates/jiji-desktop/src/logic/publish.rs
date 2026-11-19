//! How Publish reads: stack status words, the plan cards' action rows, the
//! auto-land job's standing, and the review list's rows. The plans
//! themselves are jiji-forge's; this only phrases them.

use jiji_forge::{
    AutoLandPhase, AutoLandStatus, ChecksRollup, CiRerunReport, LandAction, LandOptions, LandPlan,
    MergeMethod, PrState, PrSummary, RepoPrState, ShipAction, StackStatus, SubmitAction,
};

use super::autoland::is_interrupted;
use super::inspect::Tone;
use super::pr::{pr_badge, PrBadge};
use crate::logic::text::plural_s;

/// A stack's one-word standing and the tone of its dot.
pub fn stack_summary(status: StackStatus) -> (&'static str, Tone) {
    match status {
        StackStatus::Local => ("Local", Tone::Muted),
        StackStatus::Conflicts => ("Conflicts", Tone::Danger),
        StackStatus::NeedsAttention => ("Needs attention", Tone::Danger),
        StackStatus::Draft => ("Draft", Tone::Muted),
        StackStatus::ChecksRunning => ("Checks running", Tone::Warn),
        StackStatus::InReview => ("In review", Tone::Warn),
        StackStatus::Published => ("Published", Tone::Ok),
    }
}

fn method_label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Squash => "Squash-merge",
        MergeMethod::Rebase => "Rebase-merge",
        MergeMethod::Merge => "Merge",
    }
}

pub fn method_word(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Squash => "squash",
        MergeMethod::Rebase => "rebase",
        MergeMethod::Merge => "merge",
    }
}

fn rebase_text(root: &str, moves: u32, onto: &str) -> String {
    if moves == 1 {
        format!("Rebase {root} onto {onto}")
    } else {
        format!(
            "Rebase {root} and its {} descendant{} onto {onto}",
            moves - 1,
            plural_s(moves as usize - 1)
        )
    }
}

/// One land action as the plan card states it.
pub fn land_action_text(action: &LandAction, remote: &str) -> String {
    match action {
        LandAction::MarkReadyPr { number, bookmark } => {
            format!("Mark #{number} ({bookmark}) ready for review")
        }
        LandAction::MergePr {
            number,
            bookmark,
            method,
            ..
        } => format!("{} #{number} ({bookmark})", method_label(*method)),
        LandAction::EnableAutoMerge { number, method, .. } => format!(
            "Enable auto-merge ({}) on #{number} — GitHub merges it once its requirements are met",
            method_word(*method)
        ),
        LandAction::EnqueuePr {
            number,
            bookmark,
            method,
            ..
        } => format!(
            "Add #{number} ({bookmark}) to the merge queue — GitHub uses its configured {} method",
            method_word(*method)
        ),
        LandAction::FetchRemote { remote } => {
            format!("Fetch from {remote} so the merged trunk arrives locally")
        }
        LandAction::RebaseOntoTrunk { root_change, moves } => {
            rebase_text(root_change, *moves, "the new trunk")
        }
        LandAction::PushStack { bookmarks } => {
            format!("Push the rebased {} to {remote}", bookmarks.join(", "))
        }
        LandAction::RetargetPr {
            number,
            bookmark,
            to_base,
        } => format!("Retarget #{number} ({bookmark}) onto {to_base}"),
        LandAction::CleanupBookmark { bookmark } => {
            format!("Remove the landed bookmark {bookmark} here and on {remote}")
        }
        LandAction::AbandonLanded {
            bookmark,
            change_ids,
        } => match change_ids.as_slice() {
            [only] => {
                format!("Sweep {bookmark}'s landed change {only} — its content lives on trunk now")
            }
            many => format!(
                "Sweep {bookmark}'s {} landed changes — their content lives on trunk now",
                many.len()
            ),
        },
    }
}

/// One submit action as the plan card states it.
pub fn submit_action_text(action: &SubmitAction, remote: &str) -> String {
    match action {
        SubmitAction::CreateBookmark { bookmark, change } => {
            format!("Create bookmark {bookmark} at {change} — named from its description")
        }
        SubmitAction::Push {
            bookmark,
            create: true,
        } => format!("Publish {bookmark} as a new branch on {remote}"),
        SubmitAction::Push { bookmark, .. } => format!("Push {bookmark} to {remote}"),
        SubmitAction::CreatePr {
            bookmark,
            base,
            title,
            draft,
            ..
        } => format!(
            "Open a {}pull request: {bookmark} → {base} — \u{201c}{title}\u{201d}",
            if *draft { "draft " } else { "" }
        ),
        SubmitAction::RetargetPr {
            number,
            from_base,
            to_base,
            ..
        } => format!("Retarget #{number}: base {from_base} → {to_base}"),
        SubmitAction::UpdatePrText {
            number,
            bookmark,
            seed: true,
            ..
        } => format!(
            "Adopt #{number}\u{2019}s description — it matches {bookmark}\u{2019}s commit, so \
             Jiji records its fingerprints and keeps it updated from now on"
        ),
        SubmitAction::UpdatePrText {
            number,
            bookmark,
            title,
            ..
        } => format!(
            "Update #{number}\u{2019}s {} from {bookmark}\u{2019}s commit text",
            if title.is_some() {
                "title and description"
            } else {
                "description"
            }
        ),
        SubmitAction::SyncStackComment {
            bookmark,
            number,
            create: true,
        } => format!(
            "Post the stack comment on {}",
            match number {
                Some(number) => format!("#{number}"),
                None => format!("{bookmark}\u{2019}s new pull request"),
            }
        ),
        SubmitAction::SyncStackComment { number, .. } => format!(
            "Update the stack comment on #{}",
            number.map_or_else(String::new, |number| number.to_string())
        ),
    }
}

/// One ship action as the plan card states it.
pub fn ship_action_text(action: &ShipAction) -> String {
    match action {
        ShipAction::RebaseOntoTrunk { root_change, moves } => {
            rebase_text(root_change, *moves, "the fetched trunk first")
        }
        ShipAction::MoveTrunk { bookmark, to } => {
            format!("Point {bookmark} at {to} — the stack becomes trunk history")
        }
        ShipAction::PushTrunk { bookmark, remote } => {
            format!("Push {bookmark} to {remote} — this is the shipping step")
        }
        ShipAction::NewWorkingCopy { on } => {
            format!("Start a fresh working copy on {on} — the current one ships with the stack")
        }
    }
}

/// What the manual button does this round, named for its first real step.
pub fn manual_land_label(plan: &LandPlan) -> &'static str {
    let has = |wanted: fn(&LandAction) -> bool| plan.actions.iter().any(wanted);
    if has(|action| matches!(action, LandAction::MergePr { .. })) {
        "Land next PR"
    } else if has(|action| matches!(action, LandAction::EnqueuePr { .. })) {
        "Join merge queue"
    } else if has(|action| matches!(action, LandAction::EnableAutoMerge { .. })) {
        "Enable auto-merge"
    } else {
        "Reconcile stack"
    }
}

/// The persisted rules a job uses on every re-plan. Rendered beside the
/// resume controls so a surviving admin bypass is never implicit.
pub fn autoland_options_story(options: &LandOptions) -> String {
    let method = match options.method {
        None => "automatic merge method".to_owned(),
        Some(method) => format!("{} merge method", method_word(method)),
    };
    if options.bypass {
        format!(
            "Saved rules: {method}. Admin bypass is on; this job attempts direct merges past \
             checks, reviews, and merge queues."
        )
    } else {
        format!("Saved rules: {method}. Checks, reviews, and merge queues remain enforced.")
    }
}

/// The tone a job card's dot wears; `None` is the accent of live work.
pub fn job_tone(status: &AutoLandStatus) -> Option<Tone> {
    if is_interrupted(status) {
        return Some(Tone::Warn);
    }
    match &status.record.state.phase {
        AutoLandPhase::Done => Some(Tone::Ok),
        AutoLandPhase::Failed { .. } => Some(Tone::Danger),
        AutoLandPhase::Stopped => Some(Tone::Muted),
        AutoLandPhase::Waiting {
            attention: true, ..
        } => Some(Tone::Warn),
        AutoLandPhase::Waiting { .. } | AutoLandPhase::Round => None,
    }
}

pub fn job_title(status: &AutoLandStatus) -> &'static str {
    if is_interrupted(status) {
        return "Interrupted";
    }
    match &status.record.state.phase {
        AutoLandPhase::Done => "Stack landed",
        AutoLandPhase::Failed { .. } => "Needs attention",
        AutoLandPhase::Stopped => "Stopped",
        AutoLandPhase::Waiting { .. } | AutoLandPhase::Round => "Auto-landing",
    }
}

/// The stack comment as it will read on GitHub: the marker comments Jiji
/// finds it by are not part of the story.
pub fn comment_preview(raw: &str) -> String {
    raw.lines()
        .filter(|line| !line.starts_with("<!--"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// One open PR as a row of the review list.
pub struct ReviewRow<'a> {
    pub pr: &'a PrSummary,
    pub badge: PrBadge,
    /// `owner:branch` for fork rows, the plain branch name otherwise.
    pub head_label: String,
    /// Failed CI worth re-running: open, and the rollup says failing.
    pub can_rerun: bool,
}

/// Failed CI worth re-running. Shared by the Publish rows and the
/// workbench header chip so the two surfaces cannot disagree.
pub fn can_rerun(pr: &PrSummary) -> bool {
    pr.state == PrState::Open && pr.checks == ChecksRollup::Failing
}

/// Every open PR as a review row, in the report's newest-updated order.
pub fn review_rows<'a>(prs: Option<&'a RepoPrState>, owner: Option<&str>) -> Vec<ReviewRow<'a>> {
    let (Some(prs), Some(owner)) = (prs, owner) else {
        return Vec::new();
    };
    prs.report
        .prs
        .iter()
        .map(|pr| {
            // The head lives on a fork (or the fork is gone): exactly the
            // PRs only `refs/pull/N/head` can reach.
            let fork = pr
                .head_owner
                .as_deref()
                .is_none_or(|head| !head.eq_ignore_ascii_case(owner));
            ReviewRow {
                pr,
                badge: pr_badge(pr),
                head_label: match (&pr.head_owner, fork) {
                    (Some(head), true) => format!("{head}:{}", pr.head_branch),
                    _ => pr.head_branch.clone(),
                },
                can_rerun: can_rerun(pr),
            }
        })
        .collect()
}

/// The re-run answer as one plain line. Empty on both sides is the honest
/// "Actions cannot see that failure" story.
pub fn rerun_summary(report: &CiRerunReport) -> String {
    let mut parts = Vec::new();
    if !report.rerun.is_empty() {
        parts.push(format!(
            "Re-running failed jobs in {}.",
            report.rerun.join(", ")
        ));
    }
    if !report.refused.is_empty() {
        parts.push(format!("GitHub refused: {}.", report.refused.join("; ")));
    }
    if parts.is_empty() {
        "No failed GitHub Actions runs on this PR's head — the failing check may come from \
         another CI system."
            .to_owned()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn land_actions_read_as_plain_steps() {
        let merge = LandAction::MergePr {
            number: 12,
            bookmark: "feat".into(),
            method: MergeMethod::Squash,
            expected_head: "abc".into(),
            commit_title: None,
            commit_message: None,
        };
        assert_eq!(
            land_action_text(&merge, "origin"),
            "Squash-merge #12 (feat)"
        );
        let rebase = |moves| LandAction::RebaseOntoTrunk {
            root_change: "kxyz".into(),
            moves,
        };
        assert_eq!(
            land_action_text(&rebase(1), "origin"),
            "Rebase kxyz onto the new trunk"
        );
        assert_eq!(
            land_action_text(&rebase(2), "origin"),
            "Rebase kxyz and its 1 descendant onto the new trunk"
        );
        assert_eq!(
            land_action_text(&rebase(4), "origin"),
            "Rebase kxyz and its 3 descendants onto the new trunk"
        );
        assert_eq!(
            land_action_text(
                &LandAction::PushStack {
                    bookmarks: vec!["a".into(), "b".into()]
                },
                "upstream"
            ),
            "Push the rebased a, b to upstream"
        );
    }

    #[test]
    fn submit_actions_tell_new_branches_from_pushes() {
        let push = |create| SubmitAction::Push {
            bookmark: "feat".into(),
            create,
        };
        assert_eq!(
            submit_action_text(&push(true), "origin"),
            "Publish feat as a new branch on origin"
        );
        assert_eq!(
            submit_action_text(&push(false), "origin"),
            "Push feat to origin"
        );
        let comment = SubmitAction::SyncStackComment {
            bookmark: "feat".into(),
            number: None,
            create: true,
        };
        assert_eq!(
            submit_action_text(&comment, "origin"),
            "Post the stack comment on feat\u{2019}s new pull request"
        );
    }

    #[test]
    fn saved_rules_never_hide_a_bypass() {
        let rules = |bypass| LandOptions {
            method: Some(MergeMethod::Rebase),
            bypass,
        };
        assert!(autoland_options_story(&rules(true)).contains("Admin bypass is on"));
        assert!(autoland_options_story(&rules(false)).contains("remain enforced"));
        assert!(autoland_options_story(&LandOptions {
            method: None,
            bypass: false
        })
        .starts_with("Saved rules: automatic merge method."));
    }

    #[test]
    fn the_comment_preview_drops_its_markers() {
        assert_eq!(
            comment_preview("<!-- jiji:stack -->\nThis stack:\n- #1\n<!-- end -->\n"),
            "This stack:\n- #1"
        );
    }

    #[test]
    fn rerun_answers_cover_both_sides_and_neither() {
        let report = |rerun: &[&str], refused: &[&str]| CiRerunReport {
            rerun: rerun.iter().map(|s| s.to_string()).collect(),
            refused: refused.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(
            rerun_summary(&report(&["CI"], &["Lint: logs expired"])),
            "Re-running failed jobs in CI. GitHub refused: Lint: logs expired."
        );
        assert!(rerun_summary(&report(&[], &[])).starts_with("No failed GitHub Actions runs"));
    }

    // ── Ported from the Tauri app's review.test.ts and stacks.test.ts ──

    fn watcher(edit: impl FnOnce(&mut PrSummary)) -> PrSummary {
        let mut pr = PrSummary {
            number: 213,
            title: "Add the watcher".into(),
            url: "https://github.com/o/r/pull/213".into(),
            state: PrState::Open,
            is_draft: false,
            head_branch: "watcher-fix".into(),
            head_commit: "a".repeat(40),
            head_owner: Some("o".into()),
            base_branch: "main".into(),
            body: None,
            review: jiji_forge::ReviewDecision::None,
            checks: ChecksRollup::None,
        };
        edit(&mut pr);
        pr
    }

    fn state(prs: Vec<PrSummary>) -> RepoPrState {
        RepoPrState {
            report: jiji_forge::PrStateReport {
                prs,
                truncated: false,
            },
            by_branch: Default::default(),
        }
    }

    fn row_of(pr: PrSummary) -> (String, bool) {
        let prs = state(vec![pr]);
        let rows = review_rows(Some(&prs), Some("o"));
        (rows[0].head_label.clone(), rows[0].can_rerun)
    }

    #[test]
    fn a_same_repo_pr_reads_as_its_branch_whatever_the_owner_s_case() {
        assert_eq!(row_of(watcher(|_| {})).0, "watcher-fix");
        assert_eq!(
            row_of(watcher(|pr| pr.head_owner = Some("O".into()))).0,
            "watcher-fix"
        );
    }

    #[test]
    fn a_fork_s_pr_reads_as_owner_and_branch() {
        assert_eq!(
            row_of(watcher(|pr| pr.head_owner = Some("contributor".into()))).0,
            "contributor:watcher-fix"
        );
        // A fork since deleted keeps the plain branch.
        assert_eq!(row_of(watcher(|pr| pr.head_owner = None)).0, "watcher-fix");
    }

    #[test]
    fn re_running_ci_is_offered_on_an_open_pr_whose_checks_fail() {
        let rerun = |edit: fn(&mut PrSummary)| row_of(watcher(edit)).1;
        assert!(rerun(|pr| pr.checks = ChecksRollup::Failing));
        assert!(!rerun(|pr| pr.checks = ChecksRollup::Passing));
        assert!(!rerun(|pr| pr.checks = ChecksRollup::Pending));
        assert!(!rerun(|pr| {
            pr.state = PrState::Merged;
            pr.checks = ChecksRollup::Failing;
        }));
    }

    #[test]
    fn every_reported_pr_is_a_row_once_the_state_and_the_repo_are_known() {
        let prs = state(vec![
            watcher(|_| {}),
            watcher(|pr| {
                pr.number = 7;
                pr.head_owner = Some("fork-owner".into());
            }),
        ]);
        let rows = review_rows(Some(&prs), Some("o"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].head_label, "fork-owner:watcher-fix");
        assert!(review_rows(None, Some("o")).is_empty());
        assert!(review_rows(Some(&prs), None).is_empty());
    }

    #[test]
    fn a_stack_s_status_comes_from_the_engine_and_reads_as_a_word() {
        assert_eq!(stack_summary(StackStatus::Draft), ("Draft", Tone::Muted));
        assert_eq!(stack_summary(StackStatus::Published).0, "Published");
        assert_eq!(stack_summary(StackStatus::Local).0, "Local");
    }

    // ── Ported from the Tauri app's land.test.ts, ship.test.ts and
    // submit.test.ts: how each planned step reads ──

    fn land(action: LandAction) -> String {
        land_action_text(&action, "origin")
    }

    #[test]
    fn a_merge_reads_with_its_method() {
        let merge = |number, bookmark: &str, method| LandAction::MergePr {
            number,
            bookmark: bookmark.into(),
            method,
            expected_head: "feedface".into(),
            commit_title: None,
            commit_message: None,
        };
        assert_eq!(
            land(merge(1, "auth", MergeMethod::Squash)),
            "Squash-merge #1 (auth)"
        );
        assert_eq!(land(merge(2, "b", MergeMethod::Merge)), "Merge #2 (b)");
        assert_eq!(
            land(LandAction::MarkReadyPr {
                number: 1,
                bookmark: "auth".into()
            }),
            "Mark #1 (auth) ready for review"
        );
    }

    #[test]
    fn a_hand_off_to_github_says_whose_method_it_is() {
        let auto = land(LandAction::EnableAutoMerge {
            number: 1,
            bookmark: "auth".into(),
            method: MergeMethod::Squash,
            expected_head: Some("feedface".into()),
            commit_title: None,
            commit_message: None,
        });
        assert!(auto.contains("Enable auto-merge (squash) on #1"), "{auto}");
        assert_eq!(
            land(LandAction::EnqueuePr {
                number: 1,
                bookmark: "auth".into(),
                method: MergeMethod::Rebase,
                expected_head: "feedface".into(),
            }),
            "Add #1 (auth) to the merge queue — GitHub uses its configured rebase method"
        );
    }

    #[test]
    fn the_reconcile_after_a_merge_reads_step_by_step() {
        assert!(land(LandAction::FetchRemote {
            remote: "origin".into()
        })
        .contains("Fetch from origin"));
        assert_eq!(
            land(LandAction::RetargetPr {
                number: 7,
                bookmark: "profile".into(),
                to_base: "main".into()
            }),
            "Retarget #7 (profile) onto main"
        );
        assert_eq!(
            land(LandAction::CleanupBookmark {
                bookmark: "auth".into()
            }),
            "Remove the landed bookmark auth here and on origin"
        );
        let swept = |ids: &[&str]| {
            land(LandAction::AbandonLanded {
                bookmark: "auth".into(),
                change_ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            })
        };
        assert!(swept(&["a1"]).contains("landed change a1"));
        assert!(swept(&["a2", "a1"]).contains("2 landed changes"));
    }

    #[test]
    fn each_ship_step_reads_as_what_it_does() {
        let rebase = ship_action_text(&ShipAction::RebaseOntoTrunk {
            root_change: "abcd".into(),
            moves: 2,
        });
        assert!(rebase.contains("its 1 descendant"), "{rebase}");
        assert!(ship_action_text(&ShipAction::MoveTrunk {
            bookmark: "main".into(),
            to: "abcd".into()
        })
        .contains("Point main at abcd"));
        assert!(ship_action_text(&ShipAction::PushTrunk {
            bookmark: "main".into(),
            remote: "origin".into()
        })
        .contains("Push main to origin"));
        assert!(
            ship_action_text(&ShipAction::NewWorkingCopy { on: "abcd".into() })
                .contains("fresh working copy")
        );
    }

    fn submit(action: SubmitAction) -> String {
        submit_action_text(&action, "origin")
    }

    #[test]
    fn each_publish_step_reads_as_what_it_does() {
        assert_eq!(
            submit(SubmitAction::CreateBookmark {
                bookmark: "add-parser".into(),
                change: "c2".into()
            }),
            "Create bookmark add-parser at c2 — named from its description"
        );
        let open = |draft| {
            submit(SubmitAction::CreatePr {
                bookmark: "feat".into(),
                base: "main".into(),
                title: "feat: thing".into(),
                body: String::new(),
                draft,
            })
        };
        assert!(open(false).contains("feat → main"));
        assert!(open(true).contains("Open a draft pull request"));
        assert_eq!(
            submit(SubmitAction::RetargetPr {
                number: 7,
                bookmark: "feat".into(),
                from_base: "main".into(),
                to_base: "auth".into()
            }),
            "Retarget #7: base main → auth"
        );
    }

    #[test]
    fn a_text_update_says_what_it_changes() {
        let update = |title: Option<&str>, seed| {
            submit(SubmitAction::UpdatePrText {
                number: 7,
                bookmark: "feat".into(),
                title: title.map(str::to_owned),
                body: "wrapped".into(),
                seed,
            })
        };
        assert_eq!(
            update(None, false),
            "Update #7\u{2019}s description from feat\u{2019}s commit text"
        );
        assert!(update(Some("feat: renamed"), false).contains("title and description"));
        assert!(update(None, true).contains("Adopt #7\u{2019}s description"));
    }

    #[test]
    fn a_stack_comment_is_posted_or_updated_on_the_pr_it_belongs_to() {
        let comment = |number, create| {
            submit(SubmitAction::SyncStackComment {
                bookmark: "feat".into(),
                number,
                create,
            })
        };
        assert_eq!(comment(Some(7), false), "Update the stack comment on #7");
        assert_eq!(
            comment(None, true),
            "Post the stack comment on feat\u{2019}s new pull request"
        );
        assert_eq!(comment(Some(7), true), "Post the stack comment on #7");
    }
}
