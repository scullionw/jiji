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
use crate::pr::{parse_open_prs, PrStateReport};
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
        number title url isDraft state
        baseRefName headRefName headRefOid
        headRepositoryOwner { login }
        reviewDecision
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
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

    fn get(&self, path: &str) -> Result<Value, ForgeError> {
        let url = format!("{}{}", self.api_root, path);
        let response = self
            .http
            .get(&url)
            .send()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|err| ForgeError::Network(err.to_string()))?;
        if !status.is_success() {
            return Err(classify_http_failure(status, &text));
        }
        serde_json::from_str(&text)
            .map_err(|err| ForgeError::Api(format!("GET {path} answered malformed JSON: {err}")))
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
