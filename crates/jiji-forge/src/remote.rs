//! Detecting the forge repository behind a git remote.
//!
//! Pure URL parsing: the host hands in the remote list (the snapshot carries
//! it), and detection picks the preferred GitHub remote the same way jj's
//! default `trunk()` ranks remotes — origin first, then upstream, then the
//! rest by name.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ForgeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ForgeProvider {
    GitHub,
}

/// The forge-side identity of a repository, derived from one git remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ForgeRepo {
    pub provider: ForgeProvider,
    /// Name of the git remote this was derived from (e.g. "origin").
    pub remote: String,
    /// Forge host (e.g. "github.com", or a GitHub Enterprise host).
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl ForgeRepo {
    /// REST API root for this repo's host.
    pub fn api_root(&self) -> String {
        if self.host == "github.com" {
            "https://api.github.com/".to_owned()
        } else {
            // GitHub Enterprise Server convention. GHE cloud (ghe.com data
            // residency) uses api.<subdomain>.ghe.com instead — revisit if a
            // user actually runs one.
            format!("https://{}/api/v3/", self.host)
        }
    }

    /// GraphQL endpoint for this repo's host.
    pub fn graphql_url(&self) -> String {
        if self.host == "github.com" {
            "https://api.github.com/graphql".to_owned()
        } else {
            format!("https://{}/api/graphql", self.host)
        }
    }
}

/// Parse a GitHub remote URL into a repo identity.
///
/// Supports HTTPS (`https://github.com/owner/repo.git`),
/// SCP-style SSH (`git@github.com:owner/repo.git`),
/// `ssh://git@github.com[:port]/owner/repo.git`,
/// and GitHub Enterprise subdomains (`company.github.com`).
pub fn parse_github_url(remote_name: &str, url: &str) -> Option<ForgeRepo> {
    let (host, path) = split_host_and_path(url)?;
    if !is_github_host(host) {
        return None;
    }
    let (owner, name) = parse_owner_name(path)?;
    Some(ForgeRepo {
        provider: ForgeProvider::GitHub,
        remote: remote_name.to_owned(),
        host: host.to_owned(),
        owner,
        name,
    })
}

/// Pick the GitHub repo Jiji should talk to from a repo's git remotes,
/// ranked like jj ranks remotes for `trunk()`: origin, then upstream, then
/// the rest in name order. Non-GitHub remotes are skipped.
pub fn detect_github_repo<'a>(
    remotes: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<ForgeRepo> {
    let mut candidates: Vec<ForgeRepo> = remotes
        .into_iter()
        .filter_map(|(name, url)| parse_github_url(name, url))
        .collect();
    candidates.sort_by(|a, b| {
        (remote_rank(&a.remote), a.remote.as_str()).cmp(&(remote_rank(&b.remote), b.remote.as_str()))
    });
    candidates.into_iter().next()
}

/// The host has no GitHub remote to act on. A helper so every caller words
/// the refusal identically.
pub fn no_github_remote() -> ForgeError {
    ForgeError::NotFound(
        "this repository has no GitHub remote; add one with `jj git remote add`".to_owned(),
    )
}

fn remote_rank(name: &str) -> usize {
    match name {
        "origin" => 0,
        "upstream" => 1,
        _ => 2,
    }
}

fn is_github_host(host: &str) -> bool {
    host == "github.com" || host.ends_with(".github.com")
}

fn split_host_and_path(url: &str) -> Option<(&str, &str)> {
    // SCP-style SSH: git@host:path
    if let Some(rest) = url.strip_prefix("git@") {
        return rest.split_once(':');
    }
    // ssh://git@host[:port]/path
    if let Some(rest) = url.strip_prefix("ssh://git@") {
        let (host, path) = rest.split_once('/')?;
        return Some((strip_ssh_port(host), path));
    }
    for prefix in ["https://", "http://"] {
        if let Some(rest) = url.strip_prefix(prefix) {
            return rest.split_once('/');
        }
    }
    None
}

fn strip_ssh_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port)) if !name.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    }
}

fn parse_owner_name(path: &str) -> Option<(String, String)> {
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, rest) = path.split_once('/')?;
    let name = rest.split('/').next()?.trim();
    let owner = owner.trim();
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_owned(), name.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(url: &str) -> Option<ForgeRepo> {
        parse_github_url("origin", url)
    }

    #[test]
    fn parses_https_urls() {
        let r = repo("https://github.com/scullionw/jiji.git").unwrap();
        assert_eq!(r.host, "github.com");
        assert_eq!(r.owner, "scullionw");
        assert_eq!(r.name, "jiji");
        // No .git suffix.
        assert_eq!(repo("https://github.com/o/r").unwrap().name, "r");
        // Trailing path segments beyond owner/name are ignored.
        assert_eq!(repo("https://github.com/o/r/tree/main").unwrap().name, "r");
    }

    #[test]
    fn parses_ssh_urls() {
        assert_eq!(repo("git@github.com:o/r.git").unwrap().owner, "o");
        assert_eq!(repo("ssh://git@github.com/o/r.git").unwrap().name, "r");
        let with_port = repo("ssh://git@github.com:22/o/r.git").unwrap();
        assert_eq!(with_port.host, "github.com");
        assert_eq!(with_port.owner, "o");
    }

    #[test]
    fn parses_enterprise_subdomains() {
        let r = repo("https://company.github.com/team/tool.git").unwrap();
        assert_eq!(r.host, "company.github.com");
        assert_eq!(r.api_root(), "https://company.github.com/api/v3/");
        assert_eq!(r.graphql_url(), "https://company.github.com/api/graphql");
    }

    #[test]
    fn github_com_api_endpoints() {
        let r = repo("https://github.com/o/r.git").unwrap();
        assert_eq!(r.api_root(), "https://api.github.com/");
        assert_eq!(r.graphql_url(), "https://api.github.com/graphql");
    }

    #[test]
    fn rejects_non_github_hosts_and_junk() {
        assert!(repo("https://gitlab.com/o/r.git").is_none());
        assert!(repo("git@gitlab.com:o/r.git").is_none());
        assert!(repo("https://notgithub.com/o/r.git").is_none());
        assert!(repo("").is_none());
        assert!(repo("https://github.com/only-owner").is_none());
        assert!(repo("https://github.com//r.git").is_none());
    }

    #[test]
    fn detection_prefers_origin_then_upstream_then_name_order() {
        let remotes = [
            ("zeta", "https://github.com/zeta/r.git"),
            ("upstream", "https://github.com/up/r.git"),
            ("origin", "https://github.com/or/r.git"),
        ];
        let picked = detect_github_repo(remotes.iter().map(|(n, u)| (*n, *u))).unwrap();
        assert_eq!(picked.remote, "origin");

        let remotes = [
            ("zeta", "https://github.com/zeta/r.git"),
            ("alpha", "https://github.com/alpha/r.git"),
        ];
        let picked = detect_github_repo(remotes.iter().map(|(n, u)| (*n, *u))).unwrap();
        assert_eq!(picked.remote, "alpha");
    }

    #[test]
    fn detection_skips_non_github_remotes() {
        let remotes = [
            ("origin", "https://gitlab.com/o/r.git"),
            ("mirror", "https://github.com/o/r.git"),
        ];
        let picked = detect_github_repo(remotes.iter().map(|(n, u)| (*n, *u))).unwrap();
        assert_eq!(picked.remote, "mirror");

        assert!(detect_github_repo([("origin", "https://example.com/o/r.git")]).is_none());
        assert!(detect_github_repo([]).is_none());
    }
}
