//! PR state mapping: GitHub's API answers, reshaped into Jiji-owned DTOs.
//!
//! The parse targets the one batched GraphQL query in `github.rs` — open
//! PRs with review decision and CI check rollup — which is what keeps
//! workbench badges cheap enough to refresh on the background-fetch
//! cadence instead of per-PR REST polling.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::error::ForgeError;

/// One pull request's review-relevant state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    /// Web URL of the PR.
    pub url: String,
    pub state: PrState,
    pub is_draft: bool,
    /// Branch the PR merges from — the pushed bookmark's name.
    pub head_branch: String,
    /// Full commit id of the PR's current head.
    pub head_commit: String,
    /// Owner of the repo the head branch lives in; differs from the base
    /// repo's owner for cross-fork PRs, `None` when the fork is gone.
    pub head_owner: Option<String>,
    pub base_branch: String,
    pub review: ReviewDecision,
    pub checks: ChecksRollup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// GitHub's `reviewDecision`: `None` when the repo does not require reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

/// CI state of the PR's head commit, GitHub's own rollup across check runs
/// and commit statuses. `None` when nothing is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ChecksRollup {
    Passing,
    Failing,
    Pending,
    None,
}

/// The open-PR state of one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PrStateReport {
    pub prs: Vec<PrSummary>,
    /// True when the repo has more open PRs than the one batched query
    /// returns (capped at 100, newest-updated first).
    pub truncated: bool,
}

/// The open-PR answer as the UI consumes it: the report plus its
/// bookmark-attachment view — head branch → PR, fork-filtered via
/// [`prs_by_branch`], so a local bookmark's badge is one map lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RepoPrState {
    pub report: PrStateReport,
    pub by_branch: HashMap<String, PrSummary>,
}

impl RepoPrState {
    /// `owner` is the detected repo's owner — the fork rule's anchor.
    pub fn new(report: PrStateReport, owner: &str) -> Self {
        let by_branch = prs_by_branch(&report.prs, owner)
            .into_iter()
            .map(|(branch, pr)| (branch.to_owned(), pr.clone()))
            .collect();
        Self { report, by_branch }
    }
}

/// Reshape the batched open-PRs GraphQL answer (`data`) into the report.
pub fn parse_open_prs(data: &Value) -> Result<PrStateReport, ForgeError> {
    let repository = data.get("repository").filter(|v| !v.is_null()).ok_or_else(|| {
        ForgeError::NotFound("the repository is not visible to this token".to_owned())
    })?;
    let prs_conn = &repository["pullRequests"];
    let truncated = prs_conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
    let nodes = prs_conn["nodes"]
        .as_array()
        .ok_or_else(|| malformed("pullRequests.nodes missing"))?;
    let prs = nodes
        .iter()
        .map(parse_pr_node)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PrStateReport { prs, truncated })
}

fn parse_pr_node(node: &Value) -> Result<PrSummary, ForgeError> {
    let number = node["number"]
        .as_u64()
        .ok_or_else(|| malformed("pull request without a number"))?;
    let str_field = |key: &str| -> Result<String, ForgeError> {
        node[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| malformed(&format!("pull request #{number} missing {key}")))
    };

    let state = match node["state"].as_str() {
        Some("OPEN") => PrState::Open,
        Some("MERGED") => PrState::Merged,
        Some("CLOSED") => PrState::Closed,
        other => {
            return Err(malformed(&format!(
                "pull request #{number} has unexpected state {other:?}"
            )))
        }
    };
    let review = match node["reviewDecision"].as_str() {
        Some("APPROVED") => ReviewDecision::Approved,
        Some("CHANGES_REQUESTED") => ReviewDecision::ChangesRequested,
        Some("REVIEW_REQUIRED") => ReviewDecision::ReviewRequired,
        _ => ReviewDecision::None,
    };
    let checks = match node["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["state"].as_str()
    {
        Some("SUCCESS") => ChecksRollup::Passing,
        Some("FAILURE") | Some("ERROR") => ChecksRollup::Failing,
        Some("PENDING") | Some("EXPECTED") => ChecksRollup::Pending,
        // A state this build does not know yet is by definition not final.
        Some(_) => ChecksRollup::Pending,
        None => ChecksRollup::None,
    };

    Ok(PrSummary {
        number,
        title: str_field("title")?,
        url: str_field("url")?,
        state,
        is_draft: node["isDraft"].as_bool().unwrap_or(false),
        head_branch: str_field("headRefName")?,
        head_commit: str_field("headRefOid")?,
        head_owner: node["headRepositoryOwner"]["login"].as_str().map(str::to_owned),
        base_branch: str_field("baseRefName")?,
        review,
        checks,
    })
}

fn malformed(what: &str) -> ForgeError {
    ForgeError::Api(format!("unexpected GitHub answer: {what}"))
}

/// Index PRs by head branch so forge state can attach to local bookmarks.
/// Cross-fork PRs are excluded — a fork's branch name says nothing about
/// this repo's bookmarks (jjpr's fork rule). When several same-repo PRs
/// share a head branch (one branch targeting two bases), the first —
/// newest-updated, per the query's ordering — wins.
pub fn prs_by_branch<'a>(
    prs: &'a [PrSummary],
    owner: &str,
) -> HashMap<&'a str, &'a PrSummary> {
    let mut map: HashMap<&str, &PrSummary> = HashMap::new();
    for pr in prs {
        let same_repo = pr
            .head_owner
            .as_deref()
            .is_some_and(|head| head.eq_ignore_ascii_case(owner));
        if same_repo {
            map.entry(pr.head_branch.as_str()).or_insert(pr);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(number: u64, overrides: Value) -> Value {
        let mut base = json!({
            "number": number,
            "title": format!("PR {number}"),
            "url": format!("https://github.com/o/r/pull/{number}"),
            "isDraft": false,
            "state": "OPEN",
            "baseRefName": "main",
            "headRefName": format!("branch-{number}"),
            "headRefOid": "aaaabbbbccccddddeeeeffff0000111122223333",
            "headRepositoryOwner": { "login": "o" },
            "reviewDecision": null,
            "commits": { "nodes": [ { "commit": { "statusCheckRollup": null } } ] }
        });
        base.as_object_mut()
            .unwrap()
            .extend(overrides.as_object().unwrap().clone());
        base
    }

    fn data(nodes: Vec<Value>, has_next: bool) -> Value {
        json!({
            "repository": {
                "pullRequests": {
                    "pageInfo": { "hasNextPage": has_next },
                    "nodes": nodes
                }
            }
        })
    }

    #[test]
    fn parses_the_full_state_matrix() {
        let report = parse_open_prs(&data(
            vec![
                node(1, json!({ "reviewDecision": "APPROVED", "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "SUCCESS" } } } ] } })),
                node(2, json!({ "reviewDecision": "CHANGES_REQUESTED", "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "FAILURE" } } } ] } })),
                node(3, json!({ "reviewDecision": "REVIEW_REQUIRED", "isDraft": true, "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "PENDING" } } } ] } })),
                node(4, json!({ "commits": { "nodes": [ { "commit": { "statusCheckRollup": { "state": "ERROR" } } } ] } })),
                node(5, json!({ "commits": { "nodes": [] } })),
            ],
            false,
        ))
        .unwrap();

        assert_eq!(report.prs.len(), 5);
        assert!(!report.truncated);
        let p = &report.prs;
        assert_eq!((p[0].review, p[0].checks), (ReviewDecision::Approved, ChecksRollup::Passing));
        assert_eq!((p[1].review, p[1].checks), (ReviewDecision::ChangesRequested, ChecksRollup::Failing));
        assert_eq!((p[2].review, p[2].checks), (ReviewDecision::ReviewRequired, ChecksRollup::Pending));
        assert!(p[2].is_draft);
        assert_eq!((p[3].review, p[3].checks), (ReviewDecision::None, ChecksRollup::Failing));
        // No commits/rollup at all reads as "no checks configured".
        assert_eq!(p[4].checks, ChecksRollup::None);
        assert_eq!(p[0].head_branch, "branch-1");
        assert_eq!(p[0].base_branch, "main");
        assert_eq!(p[0].head_commit, "aaaabbbbccccddddeeeeffff0000111122223333");
    }

    #[test]
    fn truncation_and_missing_repository_surface() {
        let report = parse_open_prs(&data(vec![], true)).unwrap();
        assert!(report.truncated);
        assert!(report.prs.is_empty());

        let err = parse_open_prs(&json!({ "repository": null })).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn malformed_nodes_are_refused_not_skipped() {
        let mut broken = node(7, json!({}));
        broken.as_object_mut().unwrap().remove("headRefName");
        let err = parse_open_prs(&data(vec![broken], false)).unwrap_err();
        assert_eq!(err.code(), "api_failed");
        assert!(err.to_string().contains("#7"), "{err}");
    }

    #[test]
    fn branch_map_excludes_forks_and_keeps_first_per_branch() {
        let report = parse_open_prs(&data(
            vec![
                node(1, json!({ "headRefName": "feature" })),
                // Same branch, different base: the earlier (newest) PR wins.
                node(2, json!({ "headRefName": "feature", "baseRefName": "dev" })),
                // Cross-fork PR: excluded even though the branch name matches.
                node(3, json!({ "headRefName": "other", "headRepositoryOwner": { "login": "someone-else" } })),
                // Deleted fork: no head owner, excluded.
                node(4, json!({ "headRefName": "ghost", "headRepositoryOwner": null })),
                // Owner login comparison is case-insensitive.
                node(5, json!({ "headRefName": "cased", "headRepositoryOwner": { "login": "O" } })),
            ],
            false,
        ))
        .unwrap();

        let map = prs_by_branch(&report.prs, "o");
        assert_eq!(map.len(), 2);
        assert_eq!(map["feature"].number, 1);
        assert_eq!(map["cased"].number, 5);
        assert!(!map.contains_key("other"));
        assert!(!map.contains_key("ghost"));
    }

    #[test]
    fn repo_pr_state_carries_the_same_attachment_view() {
        let report = parse_open_prs(&data(
            vec![
                node(1, json!({ "headRefName": "feature" })),
                node(2, json!({ "headRefName": "fork-branch", "headRepositoryOwner": { "login": "someone-else" } })),
            ],
            false,
        ))
        .unwrap();

        let state = RepoPrState::new(report.clone(), "o");
        assert_eq!(state.report, report);
        assert_eq!(state.by_branch.len(), 1);
        assert_eq!(state.by_branch["feature"], report.prs[0]);
        assert!(!state.by_branch.contains_key("fork-branch"));
    }
}
