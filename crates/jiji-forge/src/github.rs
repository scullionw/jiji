//! The GitHub API client.
//!
//! Deliberately synchronous (like jjpr's engine): no async runtime to host,
//! so the same client serves Tauri commands (thread-pooled via
//! `#[tauri::command(async)]` on sync fns), the future CLI, and the M5
//! watch loop's background thread. Callers must not invoke it on the UI
//! thread or inside an async task.

use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::error::ForgeError;
use crate::land::{parse_pr_land_state, MergeMethod, PrLandState};
use crate::pr::{parse_open_prs, parse_rest_pr, PrState, PrStateReport, PrSummary};
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
        reviewDecision
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
}";

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
    mergeQueue(branch: $base) { id }
    pullRequest(number: $number) {
      id state isDraft mergeable reviewDecision isInMergeQueue
      autoMergeRequest { enabledAt }
      baseRefName headRefOid
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
    }
  }
}";

const ENABLE_AUTO_MERGE_MUTATION: &str = "\
mutation($id: ID!, $method: PullRequestMergeMethod!) {
  enablePullRequestAutoMerge(input: { pullRequestId: $id, mergeMethod: $method }) {
    clientMutationId
  }
}";

const ENQUEUE_PR_MUTATION: &str = "\
mutation($id: ID!) {
  enqueuePullRequest(input: { pullRequestId: $id }) { clientMutationId }
}";

pub struct GitHubClient {
    http: Client,
    api_root: String,
    graphql_url: String,
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
        let data = self.graphql(OPEN_PRS_QUERY, json!({ "owner": owner, "name": name }))?;
        parse_open_prs(&data)
    }

    /// Open a pull request (`POST /repos/{owner}/{name}/pulls`). `head` is
    /// the pushed branch, `base` what it merges into. Created ready (not
    /// draft) — draft handling belongs to a later slice.
    pub fn create_pr(
        &self,
        owner: &str,
        name: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<PrSummary, ForgeError> {
        let answer = self.post(
            &format!("repos/{owner}/{name}/pulls"),
            &json!({ "title": title, "body": body, "head": head, "base": base, "draft": false }),
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
        let data = self.graphql(
            PR_LAND_QUERY,
            json!({ "owner": owner, "name": name, "number": number, "base": base }),
        )?;
        parse_pr_land_state(&data, number)
    }

    /// Merge a pull request (`PUT /repos/{owner}/{name}/pulls/{number}/merge`).
    /// `expected_head` rides along as GitHub's own lease: the merge is
    /// refused if the PR's head moved since the plan read it.
    pub fn merge_pr(
        &self,
        owner: &str,
        name: &str,
        number: u64,
        method: MergeMethod,
        expected_head: &str,
    ) -> Result<(), ForgeError> {
        self.put(
            &format!("repos/{owner}/{name}/pulls/{number}/merge"),
            &json!({ "merge_method": method.rest_name(), "sha": expected_head }),
        )?;
        Ok(())
    }

    /// Enable GitHub's auto-merge on a PR: GitHub merges it itself once
    /// the repo's requirements (checks, reviews) are met. Takes the PR's
    /// GraphQL node id — auto-merge has no REST surface.
    pub fn enable_auto_merge(
        &self,
        node_id: &str,
        method: MergeMethod,
    ) -> Result<(), ForgeError> {
        self.graphql(
            ENABLE_AUTO_MERGE_MUTATION,
            json!({ "id": node_id, "method": method.graphql_name() }),
        )?;
        Ok(())
    }

    /// Add a PR to its base branch's merge queue — the only way to land
    /// on a queue-protected branch.
    pub fn enqueue_pr(&self, node_id: &str) -> Result<(), ForgeError> {
        self.graphql(ENQUEUE_PR_MUTATION, json!({ "id": node_id }))?;
        Ok(())
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
    } else {
        Some(ForgeError::Api(joined))
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
