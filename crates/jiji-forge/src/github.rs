//! The GitHub API client.
//!
//! Deliberately synchronous (like jjpr's engine): no async runtime to host,
//! so the same client serves the desktop app's background tasks, the CLI,
//! and the auto-land loop's thread. Callers must not invoke it on the UI
//! thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::error::ForgeError;
use crate::land::{parse_pr_land_state, MergeMethod, PrLandState};
use crate::pr::{parse_open_prs, parse_rest_pr, PrState, PrStateReport, PrSummary};
use crate::readiness::PrDraftState;
use crate::remote::ForgeRepo;

/// One batched query per repo: open PRs with review decision and check
/// rollup, newest-updated first. This single request is what keeps PR
/// badges cheap enough to refresh on the background-fetch cadence.
const OPEN_PRS_QUERY: &str = "\
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 100, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage }
      nodes {
        number title url isDraft state body
        baseRefName headRefName headRefOid
        headRepositoryOwner { login }
        author { login }
        reviewDecision
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
        #stack
      }
    }
  }
}";

/// A pull request's seat in a GitHub stack, spliced into the queries above
/// and below where they say `#stack`. A host whose schema has no stacks
/// (an older GitHub Enterprise Server) refuses the fields by name; the
/// client then asks again without them and stops sending them.
const STACK_FIELDS: &str = "stack { number size baseRefName } stackEntry { position }";

/// The land flow's per-PR question: is this PR ready to merge right now,
/// and what landing automation does the repo offer? One query per landing
/// candidate — mergeability is computed lazily by GitHub, so asking it for
/// all 100 open PRs in the batched query would be wasteful and slow.
const PR_LAND_QUERY: &str = "\
query($owner: String!, $name: String!, $number: Int!, $base: String!) {
  repository(owner: $owner, name: $name) {
    autoMergeAllowed
    squashMergeAllowed
    mergeCommitAllowed
    rebaseMergeAllowed
    mergeQueue(branch: $base) {
      id
      configuration { mergeMethod }
    }
    pullRequest(number: $number) {
      id state isDraft mergeable mergeStateStatus reviewDecision isInMergeQueue
      autoMergeRequest { enabledAt }
      baseRefName headRefOid
      mergeCommit { oid }
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      #stack
    }
  }
}";

const ENABLE_AUTO_MERGE_MUTATION: &str = "\
mutation(
  $id: ID!
  $method: PullRequestMergeMethod!
  $expectedHead: GitObjectID!
  $commitHeadline: String
  $commitBody: String
) {
  enablePullRequestAutoMerge(input: {
    pullRequestId: $id
    mergeMethod: $method
    expectedHeadOid: $expectedHead
    commitHeadline: $commitHeadline
    commitBody: $commitBody
  }) {
    clientMutationId
  }
}";

const ENQUEUE_PR_MUTATION: &str = "\
mutation($id: ID!, $expectedHead: GitObjectID!) {
  enqueuePullRequest(input: {
    pullRequestId: $id
    expectedHeadOid: $expectedHead
  }) { clientMutationId }
}";

const MARK_READY_MUTATION: &str = "\
mutation($id: ID!) {
  markPullRequestReadyForReview(input: { pullRequestId: $id }) { clientMutationId }
}";

const CONVERT_TO_DRAFT_MUTATION: &str = "\
mutation($id: ID!) {
  convertPullRequestToDraft(input: { pullRequestId: $id }) { clientMutationId }
}";

/// The readiness change's just-in-time check before flipping a PR's
/// draft flag: still open, and still in the state the plan read?
const PR_DRAFT_QUERY: &str = "\
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) { id state isDraft }
  }
}";

/// The REST API version that documents the stacks and asynchronous merge
/// endpoints. Sent on those requests only; everything else stays on the
/// version the client has always asked for.
const STACKS_API_VERSION: &str = "2026-03-10";

/// How long a merge GitHub runs in the background may take before the
/// client stops waiting for it. GitHub's own figure is "a few minutes" for
/// a long stack.
const ASYNC_MERGE_PATIENCE: Duration = Duration::from_secs(300);

/// How many pages of a hundred `open_prs_based_on` reads before it says
/// that there are too many to check.
const OPEN_PRS_BY_BASE_MAX_PAGES: u32 = 10;

/// How long GitHub gets to rebase the rest of a stack after part of it
/// merged, or to say that it cannot, before landing stops waiting.
const RESTACK_PATIENCE: Duration = Duration::from_secs(30);

/// How long a "conflicting" answer has to stand, the branch unmoved,
/// before it counts as GitHub refusing to rebase. On its way to a rebase
/// that succeeds, GitHub answers "dirty" for about a second: it has
/// already pointed the pull request at trunk, the branch still carries
/// the merged commits, and the two do conflict (seen on the sandbox when
/// the pull request above edits a line the merged one added).
const RESTACK_REFUSAL_HOLD: Duration = Duration::from_secs(10);

/// Where a merge requested through the asynchronous endpoint ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsyncMerge {
    /// Merged; the commit GitHub names as the result.
    Merged { commit: String },
    /// Added to the base branch's merge queue. GitHub lands it from there.
    Enqueued,
}

/// What GitHub did with the pull request left above a merge in a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restack {
    /// Its branch moved: GitHub rebased it (or it is no longer open).
    Rebased,
    /// GitHub reports that it conflicts with its new base and left the
    /// branch alone. It will not rebase it.
    Conflicting,
    /// Nothing either way for the whole wait.
    Unmoved,
}

pub struct GitHubClient {
    http: Client,
    api_root: String,
    graphql_url: String,
    /// Whether the host's GraphQL schema has the stack fields; cleared the
    /// first time it refuses them.
    stack_fields: AtomicBool,
}

impl GitHubClient {
    /// A client for the host a detected repo lives on.
    pub fn for_repo(repo: &ForgeRepo, token: &str) -> Result<Self, ForgeError> {
        Self::new(repo.api_root(), repo.graphql_url(), token)
    }

    /// A client for github.com without a detected repo (token validation).
    pub fn for_github_com(token: &str) -> Result<Self, ForgeError> {
        Self::new(
            "https://api.github.com/".to_owned(),
            "https://api.github.com/graphql".to_owned(),
            token,
        )
    }

    fn new(api_root: String, graphql_url: String, token: &str) -> Result<Self, ForgeError> {
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ForgeError::AuthFailed("the token contains invalid characters".into()))?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github+json"));
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );
        let http = Client::builder()
            .user_agent(concat!("jiji/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        Ok(Self {
            http,
            api_root,
            graphql_url,
            stack_fields: AtomicBool::new(true),
        })
    }

    /// The login the token authenticates as (`GET /user`) — the validation
    /// step behind connect and status verification.
    pub fn viewer(&self) -> Result<String, ForgeError> {
        let body = self.get("user")?;
        body["login"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ForgeError::Api("user answer carried no login".to_owned()))
    }

    /// The repo's open-PR state via the batched GraphQL query.
    pub fn open_prs(&self, owner: &str, name: &str) -> Result<PrStateReport, ForgeError> {
        let data =
            self.graphql_with_stacks(OPEN_PRS_QUERY, json!({ "owner": owner, "name": name }))?;
        parse_open_prs(&data)
    }

    /// Open a pull request (`POST /repos/{owner}/{name}/pulls`). `head` is
    /// the pushed branch, `base` what it merges into; a draft does not
    /// request reviews until it is marked ready.
    #[allow(clippy::too_many_arguments)]
    pub fn create_pr(
        &self,
        owner: &str,
        name: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
        draft: bool,
    ) -> Result<PrSummary, ForgeError> {
        let answer = self.post(
            &format!("repos/{owner}/{name}/pulls"),
            &json!({ "title": title, "body": body, "head": head, "base": base, "draft": draft }),
        )?;
        crate::pr::parse_rest_pr(&answer)
    }

    /// Retarget an existing PR's base branch
    /// (`PATCH /repos/{owner}/{name}/pulls/{number}`).
    pub fn update_pr_base(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        base: &str,
    ) -> Result<(), ForgeError> {
        self.patch(
            &format!("repos/{owner}/{name}/pulls/{number}"),
            &json!({ "base": base }),
        )?;
        Ok(())
    }

    /// Rewrite an existing PR's body — and its title, when given — in one
    /// call (`PATCH /repos/{owner}/{name}/pulls/{number}`).
    pub fn update_pr_text(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        title: Option<&str>,
        body: &str,
    ) -> Result<(), ForgeError> {
        let mut payload = json!({ "body": body });
        if let Some(title) = title {
            payload["title"] = json!(title);
        }
        self.patch(&format!("repos/{owner}/{name}/pulls/{number}"), &payload)?;
        Ok(())
    }

    /// A PR's issue comments as `(id, body)` pairs
    /// (`GET /repos/{owner}/{name}/issues/{number}/comments`). One page of
    /// 100 — the stack comment is posted early in a PR's life, so it is
    /// practically always in the first page.
    pub fn list_comments(
        &self,
        owner: &str,
        name: &str,
        number: u64,
    ) -> Result<Vec<(u64, String)>, ForgeError> {
        let answer = self.get(&format!(
            "repos/{owner}/{name}/issues/{number}/comments?per_page=100"
        ))?;
        let comments = answer
            .as_array()
            .ok_or_else(|| ForgeError::Api("comments answer was not a list".to_owned()))?;
        Ok(comments
            .iter()
            .filter_map(|comment| {
                let id = comment["id"].as_u64()?;
                let body = comment["body"].as_str().unwrap_or_default().to_owned();
                Some((id, body))
            })
            .collect())
    }

    /// Post an issue comment on a PR
    /// (`POST /repos/{owner}/{name}/issues/{number}/comments`).
    pub fn create_comment(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.post(
            &format!("repos/{owner}/{name}/issues/{number}/comments"),
            &json!({ "body": body }),
        )?;
        Ok(())
    }

    /// Edit an existing issue comment
    /// (`PATCH /repos/{owner}/{name}/issues/comments/{comment_id}`).
    pub fn update_comment(
        &self,
        owner: &str,
        name: &str,
        comment_id: u64,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.patch(
            &format!("repos/{owner}/{name}/issues/comments/{comment_id}"),
            &json!({ "body": body }),
        )?;
        Ok(())
    }

    /// One PR by number (`GET /repos/{owner}/{name}/pulls/{number}`) —
    /// the review flow's by-number lookup, reaching PRs the batched
    /// open-PR query cannot see (past the 100 cap, or closed).
    pub fn pr_by_number(
        &self,
        owner: &str,
        name: &str,
        number: u64,
    ) -> Result<PrSummary, ForgeError> {
        let answer = self.get(&format!("repos/{owner}/{name}/pulls/{number}"))?;
        parse_rest_pr(&answer)
    }

    /// GitHub Actions workflow runs for one commit
    /// (`GET /repos/{owner}/{name}/actions/runs?head_sha=…`), newest
    /// first — what the re-run-failed-CI helper filters.
    pub fn workflow_runs(
        &self,
        owner: &str,
        name: &str,
        head_sha: &str,
    ) -> Result<Vec<crate::ci::WorkflowRun>, ForgeError> {
        let sha: String = url::form_urlencoded::byte_serialize(head_sha.as_bytes()).collect();
        let answer = self.get(&format!(
            "repos/{owner}/{name}/actions/runs?head_sha={sha}&per_page=100"
        ))?;
        crate::ci::parse_workflow_runs(&answer)
    }

    /// Re-run only the failed jobs of one workflow run
    /// (`POST /repos/{owner}/{name}/actions/runs/{run_id}/rerun-failed-jobs`).
    /// Passing jobs keep their results.
    pub fn rerun_failed_jobs(
        &self,
        owner: &str,
        name: &str,
        run_id: u64,
    ) -> Result<(), ForgeError> {
        self.post(
            &format!("repos/{owner}/{name}/actions/runs/{run_id}/rerun-failed-jobs"),
            &json!({}),
        )?;
        Ok(())
    }

    /// The merged PR a branch once headed, when there is one — the
    /// per-bookmark question the batched open-PR query cannot answer
    /// (it fetches open PRs only). REST, jjpr's shape: closed PRs
    /// filtered by head `owner:branch` — which scopes to this repo's own
    /// branches, so the fork rule comes free — newest first, the first
    /// merged one wins. `None` means the branch never merged a PR.
    pub fn find_merged_pr(
        &self,
        owner: &str,
        name: &str,
        branch: &str,
    ) -> Result<Option<PrSummary>, ForgeError> {
        let head: String =
            url::form_urlencoded::byte_serialize(format!("{owner}:{branch}").as_bytes()).collect();
        let answer = self.get(&format!(
            "repos/{owner}/{name}/pulls?head={head}&state=closed&per_page=30"
        ))?;
        let prs = answer
            .as_array()
            .ok_or_else(|| ForgeError::Api("closed-PR answer was not a list".to_owned()))?;
        for pr in prs {
            let parsed = parse_rest_pr(pr)?;
            if parsed.state == PrState::Merged {
                return Ok(Some(parsed));
            }
        }
        Ok(None)
    }

    /// Every open pull request whose base branch is `base` (`GET
    /// …/pulls?state=open&base=…`, page after page). The batched open-PR
    /// query stops at the hundred most recently updated in the whole
    /// repository; a publish that moves a branch has to know every pull
    /// request based on it, because the push can close one. When the
    /// tenth page comes back full the answer is an error and not a list
    /// that may be partial.
    pub fn open_prs_based_on(
        &self,
        owner: &str,
        name: &str,
        base: &str,
    ) -> Result<Vec<PrSummary>, ForgeError> {
        let encoded: String = url::form_urlencoded::byte_serialize(base.as_bytes()).collect();
        // Every page: the reason for asking is that no pull request based
        // on this branch may be missing.
        let mut found: Vec<PrSummary> = Vec::new();
        for page in 1..=OPEN_PRS_BY_BASE_MAX_PAGES {
            let answer = self.get(&format!(
                "repos/{owner}/{name}/pulls?base={encoded}&state=open&per_page=100&page={page}"
            ))?;
            let listed = answer
                .as_array()
                .ok_or_else(|| ForgeError::Api("open-PR answer was not a list".to_owned()))?;
            for pr in listed {
                found.push(parse_rest_pr(pr)?);
            }
            if listed.len() < 100 {
                return Ok(found);
            }
        }
        // The last page read was full, so there may be more or exactly
        // this many: either way the list cannot be called complete.
        Err(ForgeError::Api(format!(
            "{} or more open pull requests are based on \u{201c}{base}\u{201d}; Jiji reads no \
             more than that, and cannot be sure it saw them all",
            OPEN_PRS_BY_BASE_MAX_PAGES * 100
        )))
    }

    /// The commit a branch is at (`GET …/branches/{branch}`).
    pub fn branch_head(&self, owner: &str, name: &str, branch: &str) -> Result<String, ForgeError> {
        let encoded: String = url::form_urlencoded::byte_serialize(branch.as_bytes()).collect();
        let answer = self.get(&format!("repos/{owner}/{name}/branches/{encoded}"))?;
        answer["commit"]["sha"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ForgeError::Api(format!("no commit in the answer for branch {branch}")))
    }

    /// One PR's land readiness plus the repo's landing capabilities, via
    /// [`PR_LAND_QUERY`]. `base` is the branch the merge queue check runs
    /// against — the trunk branch the PR is expected to land on.
    pub fn pr_land_state(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        base: &str,
    ) -> Result<PrLandState, ForgeError> {
        let data = self.graphql_with_stacks(
            PR_LAND_QUERY,
            json!({ "owner": owner, "name": name, "number": number, "base": base }),
        )?;
        parse_pr_land_state(&data, number)
    }

    /// Merge a pull request (`PUT /repos/{owner}/{name}/pulls/{number}/merge`).
    /// `expected_head` rides along as GitHub's own lease: the merge is
    /// refused if the PR's head moved since the plan read it. The commit
    /// title and message shape the trunk commit (a squash landing) —
    /// `None` leaves GitHub's own default composition.
    #[allow(clippy::too_many_arguments)]
    pub fn merge_pr(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
    ) -> Result<(), ForgeError> {
        let mut body = json!({ "merge_method": method.rest_name(), "sha": expected_head });
        if let Some(title) = commit_title {
            body["commit_title"] = json!(title);
        }
        if let Some(message) = commit_message {
            body["commit_message"] = json!(message);
        }
        self.put(
            &format!("repos/{owner}/{name}/pulls/{number}/merge"),
            &body,
        )?;
        Ok(())
    }

    /// Mark a draft pull request ready for review — a draft cannot merge,
    /// enqueue, or arm auto-merge. Takes the PR's GraphQL node id.
    pub fn mark_ready(&self, node_id: &str) -> Result<(), ForgeError> {
        self.graphql(MARK_READY_MUTATION, json!({ "id": node_id }))?;
        Ok(())
    }

    /// Convert an open pull request back to a draft. Takes the PR's
    /// GraphQL node id — like marking ready, this has no REST surface.
    pub fn convert_to_draft(&self, node_id: &str) -> Result<(), ForgeError> {
        self.graphql(CONVERT_TO_DRAFT_MUTATION, json!({ "id": node_id }))?;
        Ok(())
    }

    /// One PR's node id and draft flag via [`PR_DRAFT_QUERY`].
    pub fn pr_draft_state(
        &self,
        owner: &str,
        name: &str,
        number: u64,
    ) -> Result<PrDraftState, ForgeError> {
        let data = self.graphql(
            PR_DRAFT_QUERY,
            json!({ "owner": owner, "name": name, "number": number }),
        )?;
        let pr = &data["repository"]["pullRequest"];
        if pr.is_null() {
            return Err(ForgeError::NotFound(format!("pull request #{number}")));
        }
        Ok(PrDraftState {
            node_id: pr["id"]
                .as_str()
                .ok_or_else(|| {
                    ForgeError::Api(format!("pull request #{number} answered without an id"))
                })?
                .to_owned(),
            open: pr["state"].as_str() == Some("OPEN"),
            is_draft: pr["isDraft"].as_bool().unwrap_or(false),
        })
    }

    /// Enable GitHub's auto-merge on a PR: GitHub merges it itself once
    /// the repo's requirements (checks, reviews) are met. The expected
    /// head is GitHub's mutation-time lease; squash title/body preserve
    /// the same commit composition as a direct merge. Takes the PR's
    /// GraphQL node id — auto-merge has no REST surface.
    pub fn enable_auto_merge(
        &self,
        node_id: &str,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
    ) -> Result<(), ForgeError> {
        self.graphql(
            ENABLE_AUTO_MERGE_MUTATION,
            json!({
                "id": node_id,
                "method": method.graphql_name(),
                "expectedHead": expected_head,
                "commitHeadline": commit_title,
                "commitBody": commit_message,
            }),
        )?;
        Ok(())
    }

    /// Add a PR to its base branch's merge queue — the only way to land
    /// on a queue-protected branch. The queue owns the merge method, but
    /// the expected head still leases the exact revision being enqueued.
    pub fn enqueue_pr(&self, node_id: &str, expected_head: &str) -> Result<(), ForgeError> {
        self.graphql(
            ENQUEUE_PR_MUTATION,
            json!({ "id": node_id, "expectedHead": expected_head }),
        )?;
        Ok(())
    }

    /// Link pull requests into a GitHub stack (`POST
    /// /repos/{owner}/{name}/stacks`), bottom first. GitHub wants at least
    /// two, each based on the branch of the one before. Answers the new
    /// stack's number.
    pub fn create_stack(
        &self,
        owner: &str,
        name: &str,
        pull_requests: &[u64],
    ) -> Result<u64, ForgeError> {
        let answer = self.stacks_request(
            reqwest::Method::POST,
            &format!("repos/{owner}/{name}/stacks"),
            Some(&json!({ "pull_requests": pull_requests })),
        )?;
        answer["number"]
            .as_u64()
            .ok_or_else(|| ForgeError::Api("the new stack answered without a number".to_owned()))
    }

    /// Put pull requests on top of an existing stack (`POST
    /// …/stacks/{stack}/add`), bottom first; the first one's base must be
    /// the branch of the stack's current top.
    pub fn add_to_stack(
        &self,
        owner: &str,
        name: &str,
        stack: u64,
        pull_requests: &[u64],
    ) -> Result<(), ForgeError> {
        self.stacks_request(
            reqwest::Method::POST,
            &format!("repos/{owner}/{name}/stacks/{stack}/add"),
            Some(&json!({ "pull_requests": pull_requests })),
        )?;
        Ok(())
    }

    /// Take every unmerged pull request out of a stack (`POST
    /// …/stacks/{stack}/unstack`). They keep their bases and go back to
    /// being a plain chain; merged ones stay in the stack, which then
    /// reads as closed. The stacks API has no reorder, insert or remove:
    /// this, followed by linking again, is how a stack changes shape.
    ///
    /// Answers the open pull requests GitHub left in the stack. Its
    /// documentation names one case: a pull request queued to merge cannot
    /// leave. Usually there are none.
    pub fn unstack(&self, owner: &str, name: &str, stack: u64) -> Result<Vec<u64>, ForgeError> {
        let answer = self.stacks_request(
            reqwest::Method::POST,
            &format!("repos/{owner}/{name}/stacks/{stack}/unstack"),
            None,
        )?;
        Ok(still_stacked(&answer))
    }

    fn stacks_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ForgeError> {
        let url = format!("{}{}", self.api_root, path);
        let verb = method.to_string();
        let mut request = self
            .http
            .request(method, &url)
            .header("X-GitHub-Api-Version", STACKS_API_VERSION);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        Self::read_json(&verb, path, response)
    }

    /// Merge through the asynchronous endpoint (`PUT
    /// …/pulls/{number}/merge-async`) and wait for the result. This is the
    /// only way to merge a pull request in a GitHub stack, and it lands
    /// every open pull request below it in the stack in the same request:
    /// all of them or none. `expected_head` is the lease on the requested
    /// pull request's head; the title and message shape that pull
    /// request's own commit and nothing below it. With `queue`, the pull
    /// request joins its base branch's merge queue instead and the method
    /// is the queue's.
    #[allow(clippy::too_many_arguments)]
    pub fn merge_async(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        method: MergeMethod,
        expected_head: &str,
        commit_title: Option<&str>,
        commit_message: Option<&str>,
        bypass: bool,
        queue: bool,
    ) -> Result<AsyncMerge, ForgeError> {
        let path = format!("repos/{owner}/{name}/pulls/{number}/merge-async");
        let mut body = json!({ "sha": expected_head });
        if queue {
            body["merge_action"] = json!("merge_queue");
        } else {
            body["merge_action"] = json!("direct_merge");
            body["merge_method"] = json!(method.rest_name());
            if let Some(title) = commit_title {
                body["commit_title"] = json!(title);
            }
            if let Some(message) = commit_message {
                body["commit_message"] = json!(message);
            }
        }
        if bypass {
            body["bypass_rules"] = json!(true);
        }
        let url = format!("{}{}", self.api_root, path);
        let response = self
            .http
            .put(&url)
            .header("X-GitHub-Api-Version", STACKS_API_VERSION)
            .json(&body)
            .send()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        // 200, 202 and 400 all answer the request's own status. A 409
        // answers another request, already pending on this pull request,
        // with the options it was made with. One made for the same head
        // in the same way is this request (sent before, its answer lost,
        // or a run that was interrupted) and is the one to wait for. Any
        // other is somebody else's merge, and waiting on it would report
        // their result as this run's.
        if answer["status"].is_null() {
            return Err(classify_http_failure(status, &text));
        }
        if status == StatusCode::CONFLICT
            && !same_merge_request(&answer["details"], expected_head, method, bypass, queue)
        {
            return Err(ForgeError::Api(format!(
                "another merge of #{number} is already running on GitHub, asked for with \
                 other options or for another commit — land again when it has finished"
            )));
        }
        let deadline = Instant::now() + ASYNC_MERGE_PATIENCE;
        let mut answer = answer;
        let mut polls = 0u32;
        loop {
            match async_merge_outcome(&answer, number)? {
                Some(outcome) => return Ok(outcome),
                None if Instant::now() >= deadline => {
                    return Err(ForgeError::Api(format!(
                        "GitHub is still merging #{number} after {} minutes; run Land \
                         again to pick up the result",
                        ASYNC_MERGE_PATIENCE.as_secs() / 60
                    )))
                }
                None => {}
            }
            let uuid = answer["details"]["uuid"].as_str().ok_or_else(|| {
                ForgeError::Api(format!(
                    "GitHub left #{number}'s merge pending without a request id"
                ))
            })?;
            std::thread::sleep(poll_pause(polls));
            polls += 1;
            answer = self.stacks_request(
                reqwest::Method::GET,
                &format!("{path}/{uuid}"),
                None,
            )?;
        }
    }

    /// Wait for GitHub to rebase and push the rest of a stack after part
    /// of it merged: it retargets the next pull request at once, and
    /// rewrites the remaining branches a moment after the merge is
    /// reported. When the rebase would conflict it leaves the branch as it
    /// is and the pull request reads as conflicting instead (seen on the
    /// sandbox: `mergeable: false`, `mergeable_state: "dirty"`). That
    /// answer also shows up briefly before a rebase that goes through, so
    /// it counts only once it has stood for [`RESTACK_REFUSAL_HOLD`].
    /// Answers which of the two happened to pull request `number`, whose
    /// head was `from_head`, or that neither did for the whole wait.
    pub fn await_restack(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        from_head: &str,
    ) -> Result<Restack, ForgeError> {
        let deadline = Instant::now() + RESTACK_PATIENCE;
        let mut watch = RestackWatch::default();
        let mut polls = 0u32;
        loop {
            let pr = self.get(&format!("repos/{owner}/{name}/pulls/{number}"))?;
            if let Some(answer) = watch.see(restack_look(&pr, from_head), Instant::now()) {
                return Ok(answer);
            }
            if Instant::now() >= deadline {
                return Ok(Restack::Unmoved);
            }
            std::thread::sleep(poll_pause(polls));
            polls += 1;
        }
    }

    fn get(&self, path: &str) -> Result<Value, ForgeError> {
        let url = format!("{}{}", self.api_root, path);
        let response = self
            .http
            .get(&url)
            .send()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        Self::read_json("GET", path, response)
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, ForgeError> {
        let url = format!("{}{}", self.api_root, path);
        let response = self
            .http
            .post(&url)
            .json(body)
            .send()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        Self::read_json("POST", path, response)
    }

    fn put(&self, path: &str, body: &Value) -> Result<Value, ForgeError> {
        let url = format!("{}{}", self.api_root, path);
        let response = self
            .http
            .put(&url)
            .json(body)
            .send()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        Self::read_json("PUT", path, response)
    }

    fn patch(&self, path: &str, body: &Value) -> Result<Value, ForgeError> {
        let url = format!("{}{}", self.api_root, path);
        let response = self
            .http
            .patch(&url)
            .json(body)
            .send()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        Self::read_json("PATCH", path, response)
    }

    fn read_json(
        method: &str,
        path: &str,
        response: reqwest::blocking::Response,
    ) -> Result<Value, ForgeError> {
        let status = response.status();
        let text = response
            .text()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        if !status.is_success() {
            return Err(classify_http_failure(status, &text));
        }
        // Some write endpoints answer 201/204 with an empty body (the
        // Actions re-run endpoints do); that is success, not malformed
        // JSON.
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|err| {
            ForgeError::Api(format!("{method} {path} answered malformed JSON: {err}"))
        })
    }

    /// Run a query that has a `#stack` splice point: with the stack fields
    /// while the host knows them, without from the first refusal on.
    fn graphql_with_stacks(&self, query: &str, variables: Value) -> Result<Value, ForgeError> {
        if self.stack_fields.load(Ordering::Relaxed) {
            match self.graphql(&query.replace("#stack", STACK_FIELDS), variables.clone()) {
                Err(ForgeError::Api(message)) if message.contains(UNDEFINED_FIELD) => {
                    self.stack_fields.store(false, Ordering::Relaxed);
                }
                answer => return answer,
            }
        }
        self.graphql(&query.replace("#stack", ""), variables)
    }

    /// POST a GraphQL query and unwrap the `data` object; GraphQL-level
    /// errors map to the same forge errors HTTP failures do.
    fn graphql(&self, query: &str, variables: Value) -> Result<Value, ForgeError> {
        let response = self
            .http
            .post(&self.graphql_url)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        if !status.is_success() {
            return Err(classify_http_failure(status, &text));
        }
        let body: Value = serde_json::from_str(&text)
            .map_err(|err| ForgeError::Api(format!("GraphQL answered malformed JSON: {err}")))?;
        if let Some(err) = graphql_failure(&body) {
            return Err(err);
        }
        Ok(body["data"].clone())
    }
}

/// Map an HTTP failure status to the forge error the UI should explain.
fn classify_http_failure(status: StatusCode, body: &str) -> ForgeError {
    let message = api_message(body);
    match status.as_u16() {
        401 => ForgeError::AuthFailed(message),
        403 | 429 if message.to_lowercase().contains("rate limit") => {
            ForgeError::RateLimited(message)
        }
        // GitHub answers 403 for a merge through the endpoint that cannot
        // merge stacked pull requests. That is not about the token.
        403 if message.contains("not supported") => ForgeError::Api(message),
        // Missing scopes, SSO enforcement, or org policy — an access
        // problem, not a missing resource.
        403 => ForgeError::AuthFailed(message),
        404 => ForgeError::NotFound(message),
        _ => ForgeError::Api(format!("HTTP {}: {message}", status.as_u16())),
    }
}

/// GitHub error bodies carry a `message` field; fall back to the raw body.
fn api_message(body: &str) -> String {
    let text = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.trim().to_owned());
    let mut message = text;
    if message.is_empty() {
        message = "(empty answer)".to_owned();
    }
    if message.len() > 300 {
        let mut end = 300;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    message
}

/// A 200 GraphQL answer can still fail per-field; surface those errors like
/// their HTTP equivalents. `NOT_FOUND` is the visibility case the UI cares
/// about (bad owner/name, or a token without access).
fn graphql_failure(body: &Value) -> Option<ForgeError> {
    let errors = body["errors"].as_array().filter(|e| !e.is_empty())?;
    let messages: Vec<&str> = errors
        .iter()
        .filter_map(|e| e["message"].as_str())
        .collect();
    let joined = if messages.is_empty() {
        "unspecified GraphQL error".to_owned()
    } else {
        messages.join("; ")
    };
    if errors
        .iter()
        .any(|e| e["type"].as_str() == Some("NOT_FOUND"))
    {
        Some(ForgeError::NotFound(joined))
    } else if errors
        .iter()
        .any(|e| e["type"].as_str() == Some("RATE_LIMITED"))
    {
        Some(ForgeError::RateLimited(joined))
    } else if errors
        .iter()
        .any(|e| e["extensions"]["code"].as_str() == Some("undefinedField"))
    {
        Some(ForgeError::Api(format!("{UNDEFINED_FIELD}: {joined}")))
    } else {
        Some(ForgeError::Api(joined))
    }
}

/// How a query for a field the host's schema does not have reads once it
/// is a forge error: what `graphql_with_stacks` looks for.
const UNDEFINED_FIELD: &str = "the host's API has no such field";

/// One look at the pull request above a merge in a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestackLook {
    /// Its head is no longer the one it had, or it is no longer open.
    Moved,
    /// The old head, and GitHub says it conflicts with its base.
    Conflicting,
    /// The old head, and GitHub says anything else (it answers "unknown"
    /// while it works mergeability out).
    Waiting,
}

fn restack_look(pr: &Value, from_head: &str) -> RestackLook {
    if pr["state"].as_str() != Some("open") || pr["head"]["sha"].as_str() != Some(from_head) {
        return RestackLook::Moved;
    }
    if pr["mergeable"].as_bool() == Some(false) && pr["mergeable_state"].as_str() == Some("dirty")
    {
        RestackLook::Conflicting
    } else {
        RestackLook::Waiting
    }
}

/// What the looks add up to. A moved branch settles it at once. A
/// conflict settles it only after standing unbroken for
/// [`RESTACK_REFUSAL_HOLD`]: any other answer in between starts the count
/// again.
#[derive(Default)]
struct RestackWatch {
    conflicting_since: Option<Instant>,
}

impl RestackWatch {
    fn see(&mut self, look: RestackLook, now: Instant) -> Option<Restack> {
        match look {
            RestackLook::Moved => Some(Restack::Rebased),
            RestackLook::Conflicting => {
                let since = *self.conflicting_since.get_or_insert(now);
                (now.duration_since(since) >= RESTACK_REFUSAL_HOLD).then_some(Restack::Conflicting)
            }
            RestackLook::Waiting => {
                self.conflicting_since = None;
                None
            }
        }
    }
}

/// The open pull requests an unstack answer still lists: a 204 has no
/// body and lists none; a 200 is the stack as it stands, merged pull
/// requests included.
fn still_stacked(answer: &Value) -> Vec<u64> {
    answer["pull_requests"]
        .as_array()
        .map(|prs| {
            prs.iter()
                .filter(|pr| pr["state"].as_str() == Some("open"))
                .filter_map(|pr| pr["number"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a pending asynchronous merge, as a 409 describes it, is the
/// request being made: the same head, the same action and method, the
/// same word on bypassing rules.
fn same_merge_request(
    details: &Value,
    expected_head: &str,
    method: MergeMethod,
    bypass: bool,
    queue: bool,
) -> bool {
    let action = if queue { "merge_queue" } else { "direct_merge" };
    details["expected_head_sha"].as_str() == Some(expected_head)
        && details["merge_action"].as_str() == Some(action)
        && (queue || details["merge_method"].as_str() == Some(method.rest_name()))
        && details["bypass_rules"].as_bool().unwrap_or(false) == bypass
}

/// The pause before the next look at something GitHub is still doing:
/// a second at first, then three.
fn poll_pause(polls: u32) -> Duration {
    Duration::from_secs(if polls < 10 { 1 } else { 3 })
}

/// Read one asynchronous merge answer: `Some` when it is over, `None`
/// while it is pending, the failure as GitHub words it otherwise.
fn async_merge_outcome(answer: &Value, number: u64) -> Result<Option<AsyncMerge>, ForgeError> {
    let message = || {
        answer["details"]["message"]
            .as_str()
            .unwrap_or("no reason given")
            .to_owned()
    };
    match answer["status"].as_str() {
        Some("merged") => answer["details"]["sha"]
            .as_str()
            .map(|commit| {
                Some(AsyncMerge::Merged {
                    commit: commit.to_owned(),
                })
            })
            .ok_or_else(|| {
                ForgeError::Api(format!("GitHub merged #{number} without naming the commit"))
            }),
        Some("enqueued") => Ok(Some(AsyncMerge::Enqueued)),
        Some("pending") => Ok(None),
        Some("failed") => Err(ForgeError::Api(format!(
            "GitHub did not merge #{number}: {}",
            message()
        ))),
        other => Err(ForgeError::Api(format!(
            "unexpected merge status {other:?} for #{number}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn http_failures_classify_by_status_and_message() {
        let unauthorized =
            classify_http_failure(StatusCode::UNAUTHORIZED, r#"{"message":"Bad credentials"}"#);
        assert_eq!(unauthorized.code(), "auth_failed");
        assert!(unauthorized.to_string().contains("Bad credentials"));

        let limited = classify_http_failure(
            StatusCode::FORBIDDEN,
            r#"{"message":"API rate limit exceeded for user"}"#,
        );
        assert_eq!(limited.code(), "rate_limited");

        let forbidden = classify_http_failure(
            StatusCode::FORBIDDEN,
            r#"{"message":"Resource not accessible by personal access token"}"#,
        );
        assert_eq!(forbidden.code(), "auth_failed");

        assert_eq!(
            classify_http_failure(StatusCode::NOT_FOUND, r#"{"message":"Not Found"}"#).code(),
            "not_found"
        );
        assert_eq!(
            classify_http_failure(StatusCode::BAD_GATEWAY, "oops").code(),
            "api_failed"
        );
    }

    #[test]
    fn a_pending_merge_is_adopted_only_when_it_is_the_same_request() {
        let pending = json!({
            "uuid": "u",
            "message": "pending",
            "merge_method": "squash",
            "merge_action": "direct_merge",
            "expected_head_sha": "abc",
        });
        assert!(same_merge_request(&pending, "abc", MergeMethod::Squash, false, false));
        // Another head, another method, a queue request, or one that
        // bypasses rules is somebody else's merge.
        assert!(!same_merge_request(&pending, "def", MergeMethod::Squash, false, false));
        assert!(!same_merge_request(&pending, "abc", MergeMethod::Rebase, false, false));
        assert!(!same_merge_request(&pending, "abc", MergeMethod::Squash, true, false));
        assert!(!same_merge_request(&pending, "abc", MergeMethod::Squash, false, true));
        let queued = json!({
            "merge_method": "default",
            "merge_action": "merge_queue",
            "expected_head_sha": "abc",
            "bypass_rules": false,
        });
        assert!(same_merge_request(&queued, "abc", MergeMethod::Squash, false, true));
        assert!(!same_merge_request(&queued, "abc", MergeMethod::Squash, false, false));
    }

    #[test]
    fn a_restack_reads_as_moved_conflicting_or_still_open() {
        let open = |sha: &str, mergeable: Value, state: &str| {
            json!({ "state": "open", "head": { "sha": sha }, "mergeable": mergeable, "mergeable_state": state })
        };
        // Still the old head and nothing known yet: keep waiting.
        assert_eq!(restack_look(&open("old", Value::Null, "unknown"), "old"), RestackLook::Waiting);
        assert_eq!(restack_look(&open("old", json!(true), "clean"), "old"), RestackLook::Waiting);
        // The branch moved, or the pull request is no longer open.
        assert_eq!(restack_look(&open("new", Value::Null, "unknown"), "old"), RestackLook::Moved);
        assert_eq!(
            restack_look(&json!({ "state": "closed", "head": { "sha": "old" } }), "old"),
            RestackLook::Moved
        );
        assert_eq!(
            restack_look(&open("old", json!(false), "dirty"), "old"),
            RestackLook::Conflicting
        );
    }

    #[test]
    fn a_conflicting_answer_counts_as_a_refusal_only_once_it_has_stood() {
        let start = Instant::now();
        let at = |seconds: f32| start + Duration::from_secs_f32(seconds);
        // Seen on the sandbox (O1b), where the pull request above edits a
        // line the merged one added: unknown, dirty for about a second,
        // unknown again, and then GitHub's own rebase arrives.
        let mut watch = RestackWatch::default();
        assert_eq!(watch.see(RestackLook::Waiting, at(5.5)), None);
        assert_eq!(watch.see(RestackLook::Conflicting, at(6.0)), None);
        assert_eq!(watch.see(RestackLook::Waiting, at(6.7)), None);
        assert_eq!(watch.see(RestackLook::Moved, at(7.2)), Some(Restack::Rebased));

        // Seen on the sandbox (R4b), where trunk itself conflicts with the
        // pull request: dirty, and it stays dirty.
        let mut watch = RestackWatch::default();
        assert_eq!(watch.see(RestackLook::Conflicting, at(3.0)), None);
        assert_eq!(watch.see(RestackLook::Conflicting, at(8.0)), None);
        assert_eq!(
            watch.see(RestackLook::Conflicting, at(13.0)),
            Some(Restack::Conflicting)
        );

        // An answer in between starts the count again.
        let mut watch = RestackWatch::default();
        assert_eq!(watch.see(RestackLook::Conflicting, at(1.0)), None);
        assert_eq!(watch.see(RestackLook::Waiting, at(9.0)), None);
        assert_eq!(watch.see(RestackLook::Conflicting, at(12.0)), None);
        assert_eq!(watch.see(RestackLook::Conflicting, at(21.0)), None);
        assert_eq!(
            watch.see(RestackLook::Conflicting, at(22.0)),
            Some(Restack::Conflicting)
        );
    }

    #[test]
    fn an_unstack_answer_names_the_open_pull_requests_left_in_the_stack() {
        // 204: dissolved, no body.
        assert_eq!(still_stacked(&Value::Null), Vec::<u64>::new());
        // 200 with only merged ones left, as seen on the sandbox.
        let merged_left = json!({ "number": 20, "open": false, "pull_requests": [
            { "number": 17, "state": "closed", "merged_at": "2026-11-26T00:00:00Z" }
        ]});
        assert_eq!(still_stacked(&merged_left), Vec::<u64>::new());
        // 200 with an open one GitHub would not let go (documented for a
        // pull request queued to merge; not seen on the sandbox).
        let queued_left = json!({ "number": 20, "open": true, "pull_requests": [
            { "number": 17, "state": "closed", "merged_at": "2026-11-26T00:00:00Z" },
            { "number": 18, "state": "open", "merged_at": null }
        ]});
        assert_eq!(still_stacked(&queued_left), vec![18]);
    }

    #[test]
    fn api_message_prefers_json_message_and_truncates() {
        assert_eq!(api_message(r#"{"message":"Bad credentials"}"#), "Bad credentials");
        assert_eq!(api_message("plain text"), "plain text");
        assert_eq!(api_message(""), "(empty answer)");
        let long = api_message(&format!(r#"{{"message":"{}"}}"#, "x".repeat(500)));
        assert!(long.len() < 320);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn graphql_failures_map_not_found_and_rate_limit() {
        let not_found = graphql_failure(&json!({
            "data": { "repository": null },
            "errors": [ { "type": "NOT_FOUND", "message": "Could not resolve to a Repository" } ]
        }))
        .unwrap();
        assert_eq!(not_found.code(), "not_found");

        let limited = graphql_failure(&json!({
            "errors": [ { "type": "RATE_LIMITED", "message": "API rate limit exceeded" } ]
        }))
        .unwrap();
        assert_eq!(limited.code(), "rate_limited");

        let generic = graphql_failure(&json!({
            "errors": [ { "message": "Something went wrong" } ]
        }))
        .unwrap();
        assert_eq!(generic.code(), "api_failed");
        assert!(generic.to_string().contains("Something went wrong"));

        assert!(graphql_failure(&json!({ "data": {} })).is_none());
        assert!(graphql_failure(&json!({ "data": {}, "errors": [] })).is_none());
    }
}
