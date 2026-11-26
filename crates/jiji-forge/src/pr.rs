//! PR state mapping: GitHub's API answers, reshaped into Jiji-owned DTOs.
//!
//! The parse targets the one batched GraphQL query in `github.rs` — open
//! PRs with review decision and CI check rollup — which is what keeps
//! workbench badges cheap enough to refresh on the background-fetch
//! cadence instead of per-PR REST polling.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ForgeError;

/// One pull request's review-relevant state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    /// The PR's description text — what the submit engine reconciles
    /// commit descriptions against. `None` when GitHub reports no body.
    pub body: Option<String>,
    pub review: ReviewDecision,
    pub checks: ChecksRollup,
    /// The commit GitHub recorded as this PR's merge: the squash or merge
    /// commit, the last replayed commit of a rebase merge, or the PR's own
    /// head when GitHub closed it as merged because the head turned up
    /// inside its base branch. Read through REST, so `None` on open PRs
    /// from the batched query.
    #[serde(default)]
    pub merge_commit: Option<String>,
    /// The PR's seat in a GitHub stack, when it is linked into one.
    #[serde(default)]
    pub stack: Option<StackSeat>,
    /// Login of whoever opened the PR, when GitHub says (an account that
    /// has since been deleted has none). It is how a plan names a pull
    /// request that is someone else's to push, retarget and merge.
    #[serde(default)]
    pub author: Option<String>,
}

impl PrSummary {
    /// "#12 by @sam", or "#12" when GitHub names no author: how plans,
    /// warnings and stack listings refer to a pull request that is not
    /// one of the stack's own.
    pub fn by_author(&self) -> String {
        match &self.author {
            Some(login) => format!("#{} by @{login}", self.number),
            None => format!("#{}", self.number),
        }
    }

    /// The branch this PR lands on. A PR in a GitHub stack lands on the
    /// stack's base however far up it sits, and its own base ref keeps
    /// naming the branch below even after it merged.
    pub fn landing_branch(&self) -> &str {
        self.stack
            .as_ref()
            .map_or(self.base_branch.as_str(), |seat| seat.base.as_str())
    }

    /// The branch a merged PR's commits went into, as far as GitHub's
    /// answer tells. A merge commit that is the PR's own head means
    /// GitHub closed it as merged because that head turned up inside its
    /// base branch, and for a PR in a GitHub stack that base is the branch
    /// below it, not the stack's.
    pub fn merged_into(&self) -> &str {
        if self.merge_commit.as_deref() == Some(self.head_commit.as_str()) {
            &self.base_branch
        } else {
            self.landing_branch()
        }
    }
}

/// One pull request's place in a GitHub stack (GitHub's own stacked pull
/// requests, as opposed to a plain chain of PRs based on one another).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackSeat {
    /// The stack's number. Stacks are numbered from the same counter as
    /// issues and pull requests.
    pub number: u64,
    /// 1 at the bottom. Merged pull requests keep their seats.
    pub position: u32,
    pub size: u32,
    /// The branch the whole stack lands on.
    pub base: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

/// GitHub's `reviewDecision`: `None` when the repo does not require reviews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    None,
}

/// CI state of the PR's head commit, GitHub's own rollup across check runs
/// and commit statuses. `None` when nothing is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChecksRollup {
    Passing,
    Failing,
    Pending,
    None,
}

/// The open-PR state of one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrStateReport {
    pub prs: Vec<PrSummary>,
    /// True when the repo has more open PRs than the one batched query
    /// returns (capped at 100, newest-updated first).
    pub truncated: bool,
}

/// The open-PR answer as the UI consumes it: the report plus its
/// bookmark-attachment view — head branch → PR, fork-filtered via
/// [`prs_by_branch`], so a local bookmark's badge is one map lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

    /// Whether `pr` is the one this repository's own branch of that name
    /// has. A pull request from a fork has a branch name too, and it may
    /// be the name of a bookmark here; it names nothing here. A plan
    /// matches a pull request to a bookmark, a segment or a branch pushed
    /// from here only when it passes this.
    pub fn of_this_repository(&self, pr: &PrSummary) -> bool {
        self.by_branch
            .get(&pr.head_branch)
            .is_some_and(|own| own.number == pr.number)
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
        body: node["body"].as_str().map(str::to_owned),
        review,
        checks,
        merge_commit: node["mergeCommit"]["oid"].as_str().map(str::to_owned),
        stack: parse_graphql_seat(node),
        author: node["author"]["login"].as_str().map(str::to_owned),
    })
}

/// A PR's stack seat from the GraphQL `stack` and `stackEntry` fields.
/// Absent fields (a host without stacks, a PR outside any) read as none.
pub(crate) fn parse_graphql_seat(node: &Value) -> Option<StackSeat> {
    let stack = node.get("stack").filter(|v| !v.is_null())?;
    Some(StackSeat {
        number: stack["number"].as_u64()?,
        position: node["stackEntry"]["position"].as_u64()? as u32,
        size: stack["size"].as_u64().unwrap_or(0) as u32,
        base: stack["baseRefName"].as_str()?.to_owned(),
    })
}

/// Reshape a REST pull-request answer (`POST /pulls`, the create-PR
/// response) into the same summary the GraphQL parse produces. REST
/// carries no review decision or check rollup — a just-created PR has
/// neither, so both read as "none".
pub fn parse_rest_pr(pr: &Value) -> Result<PrSummary, ForgeError> {
    let number = pr["number"]
        .as_u64()
        .ok_or_else(|| malformed("pull request without a number"))?;
    let str_at = |path: &[&str]| -> Result<String, ForgeError> {
        let mut value = pr;
        for key in path {
            value = &value[*key];
        }
        value.as_str().map(str::to_owned).ok_or_else(|| {
            malformed(&format!("pull request #{number} missing {}", path.join(".")))
        })
    };
    let state = match pr["state"].as_str() {
        Some("open") => PrState::Open,
        Some("closed") if pr["merged_at"].is_string() => PrState::Merged,
        Some("closed") => PrState::Closed,
        other => {
            return Err(malformed(&format!(
                "pull request #{number} has unexpected state {other:?}"
            )))
        }
    };
    Ok(PrSummary {
        number,
        title: str_at(&["title"])?,
        url: str_at(&["html_url"])?,
        state,
        is_draft: pr["draft"].as_bool().unwrap_or(false),
        head_branch: str_at(&["head", "ref"])?,
        head_commit: str_at(&["head", "sha"])?,
        head_owner: pr["head"]["repo"]["owner"]["login"]
            .as_str()
            .map(str::to_owned),
        base_branch: str_at(&["base", "ref"])?,
        body: pr["body"].as_str().map(str::to_owned),
        review: ReviewDecision::None,
        checks: ChecksRollup::None,
        // On an open PR this field is GitHub's throwaway test merge, which
        // is not where anything landed.
        merge_commit: (state == PrState::Merged)
            .then(|| pr["merge_commit_sha"].as_str().map(str::to_owned))
            .flatten(),
        stack: pr.get("stack").filter(|v| !v.is_null()).and_then(|stack| {
            Some(StackSeat {
                number: stack["number"].as_u64()?,
                position: stack["position"].as_u64()? as u32,
                size: stack["size"].as_u64().unwrap_or(0) as u32,
                base: stack["base"]["ref"].as_str()?.to_owned(),
            })
        }),
        author: pr["user"]["login"].as_str().map(str::to_owned),
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

/// The open pull requests that rest on `branches` and are someone else's
/// to push, retarget and merge. Each is based on one of `branches`, or on
/// the branch of another one in the answer, and its own branch is no
/// bookmark here (`snapshot`'s local bookmarks say which are). It may be
/// a teammate's, or the same person's from another clone: what tells it
/// apart is only that nothing here names its branch. A base comes before
/// what rests on it, and pull requests on one base come in number order.
pub fn others_on<'a>(
    snapshot: &jiji_core::snapshot::RepoSnapshot,
    prs: &'a RepoPrState,
    branches: &[&str],
) -> Vec<&'a PrSummary> {
    // A fork's branch names nothing here, whatever it is called.
    let listed = |pr: &PrSummary| prs.of_this_repository(pr);
    let ours = |pr: &PrSummary| {
        listed(pr)
            && snapshot
                .bookmarks
                .iter()
                .any(|b| b.is_local && !b.is_trunk && b.name == pr.head_branch)
    };
    let mut bases: Vec<&str> = branches.to_vec();
    let mut found: Vec<&PrSummary> = Vec::new();
    let mut at = 0;
    while at < bases.len() {
        let base = bases[at];
        at += 1;
        let mut on_it: Vec<&PrSummary> = prs
            .report
            .prs
            .iter()
            .filter(|pr| pr.base_branch == base && !ours(pr))
            .filter(|pr| !found.iter().any(|seen| seen.number == pr.number))
            .collect();
        on_it.sort_by_key(|pr| pr.number);
        for pr in on_it {
            if listed(pr) && !bases.contains(&pr.head_branch.as_str()) {
                bases.push(&pr.head_branch);
            }
            found.push(pr);
        }
    }
    found
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
    fn the_author_is_read_where_github_names_one() {
        let report = parse_open_prs(&data(
            vec![
                node(1, json!({ "author": { "login": "sam" } })),
                // An account that has since been deleted.
                node(2, json!({ "author": null })),
            ],
            false,
        ))
        .unwrap();
        assert_eq!(report.prs[0].author.as_deref(), Some("sam"));
        assert_eq!(report.prs[0].by_author(), "#1 by @sam");
        assert_eq!(report.prs[1].author, None);
        assert_eq!(report.prs[1].by_author(), "#2");

        let rest = parse_rest_pr(&json!({
            "number": 9, "title": "t", "html_url": "u", "state": "open", "draft": false,
            "head": { "ref": "theirs", "sha": "abc", "repo": { "owner": { "login": "o" } } },
            "base": { "ref": "upper" },
            "user": { "login": "sam" },
        }))
        .unwrap();
        assert_eq!(rest.by_author(), "#9 by @sam");
    }

    /// A snapshot with nothing but these local bookmarks (`main` is trunk).
    fn local_bookmarks(names: &[&str]) -> jiji_core::snapshot::RepoSnapshot {
        use jiji_core::snapshot::{BookmarkState, RepoSnapshot, SnapshotView, SyncState};
        RepoSnapshot {
            repo_path: "/tmp/repo".into(),
            repo_name: "repo".into(),
            backend: "test".into(),
            view: SnapshotView::default(),
            truncated: false,
            trunk_bookmark: "main".into(),
            working_copy: String::new(),
            workspaces: vec![],
            workstreams: vec![],
            nodes: vec![],
            bookmarks: names
                .iter()
                .map(|name| BookmarkState {
                    name: (*name).into(),
                    target: format!("at-{name}"),
                    remote: Some("origin".into()),
                    sync: SyncState::Synced,
                    is_trunk: *name == "main",
                    is_local: true,
                    tracked: true,
                    is_merged: false,
                    target_description: String::new(),
                    target_author: String::new(),
                    target_timestamp: String::new(),
                })
                .collect(),
            git_remotes: vec![],
            conflicts: vec![],
            operations: vec![],
            resolve_tool: None,
        }
    }

    #[test]
    fn someone_elses_pull_requests_are_those_on_our_branches_with_no_bookmark_here() {
        let pr = |number: u64, head: &str, base: &str, owner: &str| {
            node(
                number,
                json!({
                    "headRefName": head,
                    "baseRefName": base,
                    "headRepositoryOwner": { "login": owner },
                    "author": { "login": "sam" },
                }),
            )
        };
        let report = parse_open_prs(&data(
            vec![
                // The stack's own: lower into main, upper on lower.
                pr(1, "lower", "main", "o"),
                pr(2, "upper", "lower", "o"),
                // Someone else's: on the top, on the middle, and one more
                // on top of theirs.
                pr(5, "their-top", "upper", "o"),
                pr(4, "their-mid", "lower", "o"),
                pr(6, "their-next", "their-top", "o"),
                // A branch of the user's own, off to the side: a bookmark
                // here, so not someone else's.
                pr(7, "side", "lower", "o"),
                // From a fork, based on the top: someone else's whatever
                // its branch is called, and no base for anything here.
                pr(8, "upper", "upper", "a-fork"),
                pr(10, "fork-topic", "upper", "a-fork"),
                // Based on a branch of this repository that happens to
                // have the name of the fork's: it rests on that branch,
                // not on the fork's pull request, and not on the stack.
                pr(9, "on-a-fork-name", "fork-topic", "o"),
                // Nothing to do with the stack.
                pr(3, "unrelated", "main", "o"),
            ],
            false,
        ))
        .unwrap();
        let prs = RepoPrState::new(report, "o");
        let snapshot = local_bookmarks(&["main", "lower", "upper", "side"]);
        let numbers = |branches: &[&str]| -> Vec<u64> {
            others_on(&snapshot, &prs, branches)
                .iter()
                .map(|pr| pr.number)
                .collect()
        };
        // Bases first, then what rests on them; number order on one base.
        assert_eq!(numbers(&["lower", "upper"]), [4, 5, 8, 10, 6]);
        assert_eq!(numbers(&["upper"]), [5, 8, 10, 6]);
        assert_eq!(numbers(&["lower"]), [4]);
        assert!(numbers(&["side"]).is_empty());

        // The same person's second clone is the same case: with no
        // bookmark here for `upper`, its pull request is not this
        // clone's either.
        let elsewhere = local_bookmarks(&["main", "lower"]);
        let numbers: Vec<u64> = others_on(&elsewhere, &prs, &["lower"])
            .iter()
            .map(|pr| pr.number)
            .collect();
        assert_eq!(numbers, [2, 4, 7, 5, 8, 10, 6]);
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
