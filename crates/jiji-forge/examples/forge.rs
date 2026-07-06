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
//!   cargo run -p jiji-forge --example forge -- land <repo-path> <head-bookmark>
//!   cargo run -p jiji-forge --example forge -- merged <owner>/<name> <branch>
//!   cargo run -p jiji-forge --example forge -- landstate <owner>/<name> <number> <base>
//!   cargo run -p jiji-forge --example forge -- pr <owner>/<name> <number>
//!   cargo run -p jiji-forge --example forge -- rerun <owner>/<name> <head-sha>
//!
//! `status` reports where a token would come from (no network). `whoami`
//! verifies the resolved token against the API. `login` validates a token
//! and stores it in the system keychain; `logout` removes it. `plan`
//! snapshots a real repo through `jiji-core`, fetches the detected GitHub
//! repo's open-PR state (and the repo's PR template from the trunk tree),
//! and prints the submission plan for the stack under the bookmark —
//! read-only: nothing pushes, nothing posts. `land` prints the landing
//! plan the same way (also read-only); `merged` probes the per-bookmark
//! merged-PR recognition query, `landstate` the per-PR land-readiness
//! query, `pr` the by-number lookup behind the review flow, and `rerun`
//! re-runs the failed Actions runs on a commit (the one write here).

use jiji_forge::{
    detect_github_repo, no_github_remote, plan_land, plan_submit, resolve_token, ForgeAuth,
    ForgeError, GitHubClient, KeychainTokenStore, LandRepoForge, RepoPrState, TokenSource,
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
        Some("land") => land(args.get(1).map(String::as_str), args.get(2).map(String::as_str)),
        Some("merged") => merged(
            args.get(1).map(String::as_str),
            args.get(2).map(String::as_str),
        ),
        Some("landstate") => landstate(
            args.get(1).map(String::as_str),
            args.get(2).map(String::as_str),
            args.get(3).map(String::as_str),
        ),
        Some("pr") => pr(
            args.get(1).map(String::as_str),
            args.get(2).map(String::as_str),
        ),
        Some("rerun") => rerun(
            args.get(1).map(String::as_str),
            args.get(2).map(String::as_str),
        ),
        _ => {
            eprintln!(
                "usage: forge -- status | whoami | prs <owner>/<name> | detect <url>... \
                 | login <token> | logout | plan <repo-path> <head-bookmark> \
                 | land <repo-path> <head-bookmark> | merged <owner>/<name> <branch> \
                 | landstate <owner>/<name> <number> <base> | pr <owner>/<name> <number> \
                 | rerun <owner>/<name> <head-sha>"
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
    // printed plan matches what the app would show — and the repo's PR
    // template from the trunk tree, like the app's submit commands.
    let template = jiji_core::JjBackend::default()
        .trunk_text_file(
            std::path::Path::new(repo_path),
            &jiji_forge::pr_template_candidates(),
        )
        .ok()
        .flatten()
        .map(|(path, text)| jiji_forge::PrTemplate { path, text });
    let forge_side = jiji_forge::RepoForge { client: &client, repo: &repo };
    let plan = plan_submit(&snapshot, &prs, &repo, head, &forge_side, template.as_ref())?;
    println!("{}", serde_json::to_string_pretty(&plan).expect("plan serializes"));
    Ok(())
}

fn pr(slug: Option<&str>, number: Option<&str>) -> Result<(), ForgeError> {
    let (Some((owner, name)), Some(number)) = (
        slug.and_then(|s| s.split_once('/')),
        number.and_then(|n| n.parse::<u64>().ok()),
    ) else {
        eprintln!("usage: forge -- pr <owner>/<name> <number>");
        std::process::exit(2);
    };
    let (client, _) = resolved_client()?;
    let answer = client.pr_by_number(owner, name, number)?;
    println!("{}", serde_json::to_string_pretty(&answer).expect("answer serializes"));
    Ok(())
}

fn rerun(slug: Option<&str>, head_sha: Option<&str>) -> Result<(), ForgeError> {
    let (Some((owner, name)), Some(head_sha)) = (slug.and_then(|s| s.split_once('/')), head_sha)
    else {
        eprintln!("usage: forge -- rerun <owner>/<name> <head-sha>");
        std::process::exit(2);
    };
    let (client, _) = resolved_client()?;
    let repo = jiji_forge::ForgeRepo {
        provider: jiji_forge::ForgeProvider::GitHub,
        host: "github.com".into(),
        owner: owner.to_owned(),
        name: name.to_owned(),
        remote: "origin".into(),
    };
    let report = jiji_forge::rerun_failed_ci(&client, &repo, head_sha)?;
    println!("{}", serde_json::to_string_pretty(&report).expect("report serializes"));
    Ok(())
}

fn land(repo_path: Option<&str>, head: Option<&str>) -> Result<(), ForgeError> {
    let (Some(repo_path), Some(head)) = (repo_path, head) else {
        eprintln!("usage: forge -- land <repo-path> <head-bookmark>");
        std::process::exit(2);
    };
    use jiji_core::RepoBackend as _;
    let snapshot = jiji_core::JjBackend::default()
        .open(std::path::Path::new(repo_path))
        .map_err(|err| ForgeError::Land(format!("could not snapshot the repo: {err}")))?;
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
    let forge_side = LandRepoForge { client: &client, repo: &repo };
    let plan = plan_land(&snapshot, &prs, &repo, head, &forge_side)?;
    println!("{}", serde_json::to_string_pretty(&plan).expect("plan serializes"));
    Ok(())
}

fn merged(slug: Option<&str>, branch: Option<&str>) -> Result<(), ForgeError> {
    let (Some((owner, name)), Some(branch)) = (slug.and_then(|s| s.split_once('/')), branch)
    else {
        eprintln!("usage: forge -- merged <owner>/<name> <branch>");
        std::process::exit(2);
    };
    let (client, _) = resolved_client()?;
    let answer = client.find_merged_pr(owner, name, branch)?;
    println!("{}", serde_json::to_string_pretty(&answer).expect("answer serializes"));
    Ok(())
}

fn landstate(
    slug: Option<&str>,
    number: Option<&str>,
    base: Option<&str>,
) -> Result<(), ForgeError> {
    let (Some((owner, name)), Some(number), Some(base)) = (
        slug.and_then(|s| s.split_once('/')),
        number.and_then(|n| n.parse::<u64>().ok()),
        base,
    ) else {
        eprintln!("usage: forge -- landstate <owner>/<name> <number> <base>");
        std::process::exit(2);
    };
    let (client, _) = resolved_client()?;
    let state = client.pr_land_state(owner, name, number, base)?;
    println!("{state:#?}");
    Ok(())
}
