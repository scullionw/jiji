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
//!
//! `status` reports where a token would come from (no network). `whoami`
//! verifies the resolved token against the API. `login` validates a token
//! and stores it in the system keychain; `logout` removes it.

use jiji_forge::{
    detect_github_repo, resolve_token, ForgeAuth, ForgeError, GitHubClient, KeychainTokenStore,
    TokenSource,
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
        _ => {
            eprintln!("usage: forge -- status | whoami | prs <owner>/<name> | detect <url>... | login <token> | logout");
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
