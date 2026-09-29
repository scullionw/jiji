//! Review readiness for a stack's PRs: mark drafts ready for review, or
//! convert open PRs back to drafts. Nothing pushes and no base or text
//! changes — only the draft flag moves.
//!
//! Same plan-then-execute shape as submit and land: the plan comes from
//! the stack's open PRs as discovered, and execution re-reads each PR just
//! before writing, so a PR that closed or already changed state since the
//! plan was read is reported rather than written blindly. Each PR is its
//! own step; one failure does not stop the others.

use serde::{Deserialize, Serialize};

use crate::error::ForgeError;
use crate::stacks::PublishStack;
use crate::submit::SubmitStepStatus;

/// The review state a readiness plan moves a stack's PRs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrReadiness {
    ReadyForReview,
    Draft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReadinessAction {
    #[serde(rename_all = "camelCase")]
    MarkReadyPr { number: u64, bookmark: String },
    #[serde(rename_all = "camelCase")]
    ConvertPrToDraft { number: u64, bookmark: String },
}

impl ReadinessAction {
    pub fn number(&self) -> u64 {
        match self {
            Self::MarkReadyPr { number, .. } | Self::ConvertPrToDraft { number, .. } => *number,
        }
    }
}

/// A PR the plan leaves alone because it already reads as the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessUnchanged {
    pub number: u64,
    pub bookmark: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessPlan {
    /// The stack (or the prefix of it through a named bookmark) the plan covers.
    pub head: String,
    pub target: PrReadiness,
    /// Bottom first, in landing order.
    pub actions: Vec<ReadinessAction>,
    pub unchanged: Vec<ReadinessUnchanged>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessStep {
    pub action: ReadinessAction,
    pub status: SubmitStepStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessOutcome {
    pub steps: Vec<ReadinessStep>,
    pub failed: bool,
}

/// A PR's draft flag as GitHub reports it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrDraftState {
    pub node_id: String,
    pub open: bool,
    pub is_draft: bool,
}

/// The forge side of a readiness change; [`RepoForge`](crate::RepoForge)
/// over the real client, a stub in tests.
pub trait ReadinessForge {
    fn pr_draft_state(&self, number: u64) -> Result<PrDraftState, ForgeError>;
    fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError>;
    fn convert_to_draft(&self, node_id: &str) -> Result<(), ForgeError>;
}

impl ReadinessForge for crate::submit::RepoForge<'_> {
    fn pr_draft_state(&self, number: u64) -> Result<PrDraftState, ForgeError> {
        self.client
            .pr_draft_state(&self.repo.owner, &self.repo.name, number)
    }

    fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
        self.client.mark_ready(node_id)
    }

    fn convert_to_draft(&self, node_id: &str) -> Result<(), ForgeError> {
        self.client.convert_to_draft(node_id)
    }
}

/// Plan moving every open PR in `stack` to `target`. Segments without a
/// PR are noted, not published; a stack with no open PR at all blocks.
pub fn plan_readiness(stack: &PublishStack, target: PrReadiness) -> ReadinessPlan {
    let mut plan = ReadinessPlan {
        head: stack.key.clone(),
        target,
        actions: Vec::new(),
        unchanged: Vec::new(),
        blockers: Vec::new(),
        warnings: Vec::new(),
    };
    for segment in &stack.segments {
        let bookmark = segment.bookmark.clone().unwrap_or_default();
        let Some(pr) = &segment.pr else {
            plan.warnings.push(format!(
                "{} has no open PR yet",
                segment.bookmark.as_deref().unwrap_or(&segment.title)
            ));
            continue;
        };
        let number = pr.number;
        match (target, pr.is_draft) {
            (PrReadiness::ReadyForReview, true) => plan
                .actions
                .push(ReadinessAction::MarkReadyPr { number, bookmark }),
            (PrReadiness::Draft, false) => plan
                .actions
                .push(ReadinessAction::ConvertPrToDraft { number, bookmark }),
            _ => plan.unchanged.push(ReadinessUnchanged { number, bookmark }),
        }
    }
    if stack.pr_count == 0 {
        plan.blockers
            .push("This stack has no open PRs; publish it first".to_owned());
    }
    plan
}

/// Run a plan. Each PR is re-read first: one that closed fails its step,
/// one that already reads as the target is done without a write.
pub fn execute_readiness(
    plan: &ReadinessPlan,
    forge: &dyn ReadinessForge,
) -> Result<ReadinessOutcome, ForgeError> {
    if !plan.blockers.is_empty() {
        return Err(ForgeError::Plan(plan.blockers.join("; ")));
    }
    let want_draft = plan.target == PrReadiness::Draft;
    let steps: Vec<_> = plan
        .actions
        .iter()
        .map(|action| {
            let number = action.number();
            let result = forge.pr_draft_state(number).and_then(|state| {
                if !state.open {
                    return Err(ForgeError::Plan(format!("#{number} is no longer open")));
                }
                if state.is_draft == want_draft {
                    return Ok(Some(if want_draft {
                        "already a draft"
                    } else {
                        "already ready for review"
                    }));
                }
                match action {
                    ReadinessAction::MarkReadyPr { .. } => forge.mark_ready(&state.node_id),
                    ReadinessAction::ConvertPrToDraft { .. } => {
                        forge.convert_to_draft(&state.node_id)
                    }
                }
                .map(|()| None)
            });
            match result {
                Ok(detail) => ReadinessStep {
                    action: action.clone(),
                    status: SubmitStepStatus::Done,
                    detail: detail.map(str::to_owned),
                },
                Err(err) => ReadinessStep {
                    action: action.clone(),
                    status: SubmitStepStatus::Failed,
                    detail: Some(err.to_string()),
                },
            }
        })
        .collect();
    let failed = steps.iter().any(|s| s.status == SubmitStepStatus::Failed);
    Ok(ReadinessOutcome { steps, failed })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;
    use crate::{discover_stacks, RepoPrState};
    use jiji_core::RepoSnapshot;

    #[derive(serde::Deserialize)]
    struct Fixture {
        snapshot: RepoSnapshot,
        prs: RepoPrState,
    }

    /// Five stacked PRs, #753–#757, all drafts.
    fn stack() -> PublishStack {
        let f: Fixture =
            serde_json::from_str(include_str!("../tests/fixtures/publish-stack.json")).unwrap();
        discover_stacks(&f.snapshot, Some(&f.prs)).remove(0)
    }

    #[derive(Default)]
    struct StubForge {
        drafts: RefCell<HashMap<u64, bool>>,
        closed: Vec<u64>,
        log: RefCell<Vec<String>>,
    }

    impl ReadinessForge for StubForge {
        fn pr_draft_state(&self, number: u64) -> Result<PrDraftState, ForgeError> {
            Ok(PrDraftState {
                node_id: format!("PR{number}"),
                open: !self.closed.contains(&number),
                is_draft: *self.drafts.borrow().get(&number).unwrap_or(&true),
            })
        }
        fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
            self.log.borrow_mut().push(format!("ready:{node_id}"));
            Ok(())
        }
        fn convert_to_draft(&self, node_id: &str) -> Result<(), ForgeError> {
            self.log.borrow_mut().push(format!("draft:{node_id}"));
            Ok(())
        }
    }

    fn numbers(plan: &ReadinessPlan) -> Vec<u64> {
        plan.actions.iter().map(ReadinessAction::number).collect()
    }

    #[test]
    fn ready_marks_every_draft_in_landing_order() {
        let plan = plan_readiness(&stack(), PrReadiness::ReadyForReview);
        assert_eq!(numbers(&plan), [753, 754, 755, 756, 757]);
        assert!(plan.blockers.is_empty() && plan.unchanged.is_empty());
        assert!(matches!(
            &plan.actions[0],
            ReadinessAction::MarkReadyPr { bookmark, .. } if bookmark == "ws/tu1"
        ));
    }

    #[test]
    fn an_intermediate_bookmark_limits_the_change_to_that_prefix() {
        let prefix = stack().through_bookmark("ws/tu2").unwrap();
        let plan = plan_readiness(&prefix, PrReadiness::ReadyForReview);
        assert_eq!(numbers(&plan), [753, 754]);
        assert_eq!(plan.head, "ws/tu2");
    }

    #[test]
    fn prs_already_at_the_target_are_left_alone() {
        let mut stack = stack();
        stack.segments[1].pr.as_mut().unwrap().is_draft = false;
        let draft = plan_readiness(&stack, PrReadiness::Draft);
        assert_eq!(numbers(&draft), [754]);
        assert_eq!(draft.unchanged.len(), 4);
        let ready = plan_readiness(&stack, PrReadiness::ReadyForReview);
        assert_eq!(numbers(&ready), [753, 755, 756, 757]);
        assert_eq!(ready.unchanged[0].number, 754);
    }

    #[test]
    fn unpublished_segments_are_noted_and_an_unpublished_stack_blocks() {
        let mut stack = stack();
        stack.segments[4].pr = None;
        stack.pr_count = 4;
        let plan = plan_readiness(&stack, PrReadiness::ReadyForReview);
        assert_eq!(numbers(&plan), [753, 754, 755, 756]);
        assert_eq!(plan.warnings.len(), 1);

        for segment in &mut stack.segments {
            segment.pr = None;
        }
        stack.pr_count = 0;
        let plan = plan_readiness(&stack, PrReadiness::Draft);
        assert!(plan.actions.is_empty());
        assert_eq!(plan.blockers.len(), 1);
        assert!(execute_readiness(&plan, &StubForge::default()).is_err());
    }

    #[test]
    fn execute_rereads_each_pr_and_keeps_going_past_a_failure() {
        let prefix = stack().through_bookmark("ws/tu3-deps").unwrap();
        let plan = plan_readiness(&prefix, PrReadiness::ReadyForReview);
        assert_eq!(numbers(&plan), [753, 754, 755]);
        let forge = StubForge {
            closed: vec![753],
            ..StubForge::default()
        };
        // #754 became ready elsewhere after the plan was read.
        forge.drafts.borrow_mut().insert(754, false);
        let outcome = execute_readiness(&plan, &forge).unwrap();

        assert!(outcome.failed);
        let statuses: Vec<_> = outcome.steps.iter().map(|s| s.status).collect();
        assert_eq!(
            statuses,
            [
                SubmitStepStatus::Failed,
                SubmitStepStatus::Done,
                SubmitStepStatus::Done
            ]
        );
        assert_eq!(
            outcome.steps[1].detail.as_deref(),
            Some("already ready for review")
        );
        assert_eq!(forge.log.borrow().as_slice(), ["ready:PR755"]);
    }

    #[test]
    fn execute_converts_ready_prs_to_drafts() {
        let mut stack = stack();
        for segment in &mut stack.segments {
            segment.pr.as_mut().unwrap().is_draft = false;
        }
        let plan = plan_readiness(&stack, PrReadiness::Draft);
        let forge = StubForge::default();
        for number in 753..=757 {
            forge.drafts.borrow_mut().insert(number, false);
        }
        let outcome = execute_readiness(&plan, &forge).unwrap();
        assert!(!outcome.failed);
        assert_eq!(
            forge.log.borrow().as_slice(),
            [
                "draft:PR753",
                "draft:PR754",
                "draft:PR755",
                "draft:PR756",
                "draft:PR757"
            ]
        );
    }
}
