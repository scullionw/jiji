//! Headless driver for the forge engine, the `snapshot` example's sibling:
//! exercise auth, the API client, and PR state mapping against real GitHub
//! from a terminal.
//!
//! Usage:
//!   cargo run -p jiji-forge --example forge -- status
//!   cargo run -p jiji-forge --example forge -- whoami
//!   cargo run -p jiji-forge --example forge -- prs <owner>/<name>
//!   cargo run -p jiji-forge --example forge -- detect <remote-url>...
//!   cargo run -p jiji-forge --example forge -- login <token>
//!   cargo run -p jiji-forge --example forge -- logout
//!   cargo run -p jiji-forge --example forge -- plan <repo-path> <head-bookmark>
//!
//! `status` reports where a token would come from (no network). `whoami`
//! verifies the resolved token against the API. `login` validates a token
//! and stores it in the system keychain; `logout` removes it. `plan`
//! snapshots a real repo through `jiji-core`, fetches the detected GitHub
//! repo's open-PR state, and prints the submission plan for the stack
//! under the bookmark — read-only: nothing pushes, nothing posts.

use jiji_forge::{
    detect_github_repo, no_github_remote, plan_submit, resolve_token, ForgeAuth, ForgeError,
    GitHubClient, KeychainTokenStore, RepoPrState, TokenSource,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("status") => status(),
        Some("whoami") => whoami(),
        Some("prs") => prs(args.get(1).map(String::as_str)),
        Some("detect") => detect(&args[1..]),
        Some("login") => login(args.get(1).map(String::as_str)),
        Some("logout") => logout(),
        Some("plan") => plan(args.get(1).map(String::as_str), args.get(2).map(String::as_str)),
        _ => {
            eprintln!(
                "usage: forge -- status | whoami | prs <owner>/<name> | detect <url>... \
                 | login <token> | logout | plan <repo-path> <head-bookmark>"
            );
            std::process::exit(2);
        }
    };
    if let Err(err) = result {
        eprintln!("error [{}]: {err}", err.code());
        std::process::exit(1);
    }
}

fn store() -> KeychainTokenStore {
    KeychainTokenStore::new("github.com")
}

fn status() -> Result<(), ForgeError> {
    let auth = ForgeAuth {
        source: resolve_token(&store())?.map(|t| t.source),
        login: None,
    };
    println!("{}", serde_json::to_string_pretty(&auth).expect("auth serializes"));
    Ok(())
}

fn resolved_client() -> Result<(GitHubClient, TokenSource), ForgeError> {
    let resolved = resolve_token(&store())?.ok_or(ForgeError::NoToken)?;
    Ok((
        GitHubClient::for_github_com(&resolved.token)?,
        resolved.source,
    ))
}

fn whoami() -> Result<(), ForgeError> {
    let (client, source) = resolved_client()?;
    let login = client.viewer()?;
    let auth = ForgeAuth {
        source: Some(source),
        login: Some(login),
    };
    println!("{}", serde_json::to_string_pretty(&auth).expect("auth serializes"));
    Ok(())
}

fn prs(slug: Option<&str>) -> Result<(), ForgeError> {
    let Some((owner, name)) = slug.and_then(|s| s.split_once('/')) else {
        eprintln!("usage: forge -- prs <owner>/<name>");
        std::process::exit(2);
    };
    let (client, _) = resolved_client()?;
    let report = client.open_prs(owner, name)?;
    println!("{}", serde_json::to_string_pretty(&report).expect("report serializes"));
    Ok(())
}

fn detect(urls: &[String]) -> Result<(), ForgeError> {
    // Fabricated remote names: first URL plays origin, the rest are numbered.
    let named: Vec<(String, &str)> = urls
        .iter()
        .enumerate()
        .map(|(i, url)| {
            let name = if i == 0 { "origin".to_owned() } else { format!("remote{i}") };
            (name, url.as_str())
        })
        .collect();
    let detected = detect_github_repo(named.iter().map(|(n, u)| (n.as_str(), *u)));
    println!("{}", serde_json::to_string_pretty(&detected).expect("repo serializes"));
    Ok(())
}

fn login(token: Option<&str>) -> Result<(), ForgeError> {
    let Some(token) = token else {
        eprintln!("usage: forge -- login <token>");
        std::process::exit(2);
    };
    let client = GitHubClient::for_github_com(token)?;
    let login = client.viewer()?;
    use jiji_forge::TokenStore as _;
    store().set(token)?;
    println!("stored keychain token for {login}");
    Ok(())
}

fn logout() -> Result<(), ForgeError> {
    use jiji_forge::TokenStore as _;
    store().delete()?;
    println!("keychain token removed");
    Ok(())
}

fn plan(repo_path: Option<&str>, head: Option<&str>) -> Result<(), ForgeError> {
    let (Some(repo_path), Some(head)) = (repo_path, head) else {
        eprintln!("usage: forge -- plan <repo-path> <head-bookmark>");
        std::process::exit(2);
    };
    use jiji_core::RepoBackend as _;
    let snapshot = jiji_core::JjBackend::default()
        .open(std::path::Path::new(repo_path))
        .map_err(|err| ForgeError::Plan(format!("could not snapshot the repo: {err}")))?;
    let repo = detect_github_repo(
        snapshot
            .git_remotes
            .iter()
            .map(|r| (r.name.as_str(), r.url.as_str())),
    )
    .ok_or_else(no_github_remote)?;
    let resolved = resolve_token(&KeychainTokenStore::new(&repo.host))?.ok_or(ForgeError::NoToken)?;
    let client = GitHubClient::for_repo(&repo, &resolved.token)?;
    let prs = RepoPrState::new(client.open_prs(&repo.owner, &repo.name)?, &repo.owner);
    // Planning reads existing stack comments (still read-only) so the
    // printed plan matches what the app would show.
    let forge_side = jiji_forge::RepoForge { client: &client, repo: &repo };
    let plan = plan_submit(&snapshot, &prs, &repo, head, &forge_side)?;
    println!("{}", serde_json::to_string_pretty(&plan).expect("plan serializes"));
    Ok(())
}
