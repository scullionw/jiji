//! The re-run-failed-CI review helper: GitHub Actions workflow runs for a
//! PR's head commit, filtered to the re-runnable failures, each asked to
//! re-run only its failed jobs (`rerun-failed-jobs` — passing jobs keep
//! their results).
//!
//! Honest scope: this can only reach GitHub Actions. A failing check from
//! an external CI system (Buildkite, CircleCI, a commit status) shows in
//! the same checks rollup but has no runs here — the report says so
//! instead of pretending nothing failed.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::error::ForgeError;
use crate::github::GitHubClient;
use crate::remote::ForgeRepo;

/// One workflow run for a commit, the slice of
/// `GET /actions/runs?head_sha=…` the re-run flow needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRun {
    pub id: u64,
    /// The workflow this run belongs to; several runs of one workflow can
    /// exist for a single commit (re-triggers), newest first.
    pub workflow_id: u64,
    pub name: String,
    /// GitHub's `status` (`completed`, `in_progress`, `queued`, …).
    pub status: String,
    /// GitHub's `conclusion`; `None` while the run is still going.
    pub conclusion: Option<String>,
}

impl WorkflowRun {
    /// Only a finished run that actually failed has failed jobs to re-run.
    pub fn is_rerunnable(&self) -> bool {
        self.status == "completed"
            && matches!(
                self.conclusion.as_deref(),
                Some("failure") | Some("cancelled") | Some("timed_out")
            )
    }
}

/// What one re-run request accomplished, for the UI to tell straight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CiRerunReport {
    /// Workflow runs whose failed jobs were sent back to work.
    pub rerun: Vec<String>,
    /// Failed runs GitHub refused to re-run (already re-running, logs
    /// expired, …), as plain per-run stories.
    pub refused: Vec<String>,
}

/// Reshape the `GET /actions/runs` answer into [`WorkflowRun`]s. Runs the
/// answer is missing fields for are refused, not skipped — a malformed
/// answer should read as such.
pub fn parse_workflow_runs(data: &Value) -> Result<Vec<WorkflowRun>, ForgeError> {
    let runs = data["workflow_runs"]
        .as_array()
        .ok_or_else(|| ForgeError::Api("unexpected GitHub answer: workflow_runs missing".into()))?;
    runs.iter()
        .map(|run| {
            let id = run["id"].as_u64().ok_or_else(|| {
                ForgeError::Api("unexpected GitHub answer: workflow run without an id".into())
            })?;
            Ok(WorkflowRun {
                id,
                workflow_id: run["workflow_id"].as_u64().unwrap_or(id),
                name: run["name"]
                    .as_str()
                    .unwrap_or("unnamed workflow")
                    .to_owned(),
                status: run["status"].as_str().unwrap_or_default().to_owned(),
                conclusion: run["conclusion"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

/// The failed runs worth re-running: the *latest* run of each workflow
/// (the answer is newest-first), and only when that latest run is a
/// re-runnable failure — re-running an old failure a newer run already
/// superseded would churn CI for nothing.
pub fn rerunnable_runs(runs: &[WorkflowRun]) -> Vec<&WorkflowRun> {
    let mut seen = std::collections::HashSet::new();
    runs.iter()
        .filter(|run| seen.insert(run.workflow_id))
        .filter(|run| run.is_rerunnable())
        .collect()
}

/// Re-run the failed jobs of every failed workflow run on one commit.
/// Empty `rerun` + empty `refused` means Actions has no failed runs for
/// the commit — the failing check the badge shows comes from somewhere
/// Actions' API cannot reach.
pub fn rerun_failed_ci(
    client: &GitHubClient,
    repo: &ForgeRepo,
    head_sha: &str,
) -> Result<CiRerunReport, ForgeError> {
    let runs = client.workflow_runs(&repo.owner, &repo.name, head_sha)?;
    let mut report = CiRerunReport {
        rerun: Vec::new(),
        refused: Vec::new(),
    };
    for run in rerunnable_runs(&runs) {
        match client.rerun_failed_jobs(&repo.owner, &repo.name, run.id) {
            Ok(()) => report.rerun.push(run.name.clone()),
            Err(err) => report.refused.push(format!("{}: {err}", run.name)),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(id: u64, workflow_id: u64, status: &str, conclusion: Option<&str>) -> WorkflowRun {
        WorkflowRun {
            id,
            workflow_id,
            name: format!("wf-{workflow_id}"),
            status: status.to_owned(),
            conclusion: conclusion.map(str::to_owned),
        }
    }

    #[test]
    fn parses_the_runs_answer() {
        let runs = parse_workflow_runs(&json!({
            "workflow_runs": [
                { "id": 11, "workflow_id": 1, "name": "ci", "status": "completed", "conclusion": "failure" },
                { "id": 12, "workflow_id": 2, "name": "docs", "status": "in_progress", "conclusion": null },
            ]
        }))
        .unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].name, "ci");
        assert!(runs[0].is_rerunnable());
        assert!(!runs[1].is_rerunnable());

        let err = parse_workflow_runs(&json!({})).unwrap_err();
        assert_eq!(err.code(), "api_failed");
    }

    #[test]
    fn only_final_failures_are_rerunnable() {
        assert!(run(1, 1, "completed", Some("failure")).is_rerunnable());
        assert!(run(1, 1, "completed", Some("cancelled")).is_rerunnable());
        assert!(run(1, 1, "completed", Some("timed_out")).is_rerunnable());
        assert!(!run(1, 1, "completed", Some("success")).is_rerunnable());
        assert!(!run(1, 1, "completed", Some("skipped")).is_rerunnable());
        assert!(!run(1, 1, "in_progress", None).is_rerunnable());
        assert!(!run(1, 1, "queued", None).is_rerunnable());
    }

    #[test]
    fn superseded_failures_are_not_rerun() {
        // Workflow 1 failed once, then a newer run passed: nothing to do.
        // Workflow 2's latest run failed: that one re-runs. The answer is
        // newest-first, like GitHub's.
        let runs = vec![
            run(31, 1, "completed", Some("success")),
            run(32, 2, "completed", Some("failure")),
            run(30, 1, "completed", Some("failure")),
        ];
        let rerunnable = rerunnable_runs(&runs);
        assert_eq!(rerunnable.len(), 1);
        assert_eq!(rerunnable[0].id, 32);

        // A still-running latest attempt also supersedes its old failure.
        let runs = vec![
            run(41, 1, "in_progress", None),
            run(40, 1, "completed", Some("failure")),
        ];
        assert!(rerunnable_runs(&runs).is_empty());
    }
}
