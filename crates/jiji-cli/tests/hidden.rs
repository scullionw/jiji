use std::path::Path;
use std::process::{Command, Output};

use jiji_core::{JjBackend, RepoBackend, UserConfigSource};
use jj_lib::{config::StackedConfig, settings::UserSettings, workspace::Workspace};
use serde_json::Value;

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["--json", "-R"])
        .arg(repo)
        .args(args)
        .env("JJ_CONFIG", repo.join(".jj/repo/config.toml"))
        .output()
        .unwrap()
}

fn json(repo: &Path, args: &[&str]) -> Value {
    let output = run(repo, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "successful JSON commands must not print diagnostics"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn hide_list_and_unhide_work_through_the_standalone_cli_without_a_forge() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let settings = UserSettings::from_config(StackedConfig::with_defaults()).unwrap();
    pollster::block_on(Workspace::init_colocated_git(&settings, path, gix::hash::Kind::Sha1)).unwrap();
    std::fs::write(
        path.join(".jj/repo/config.toml"),
        "user.name = 'Test'\nuser.email = 'test@example.com'\n",
    )
    .unwrap();
    let backend = JjBackend::with_user_config(UserConfigSource::None);
    std::fs::write(path.join("idea"), "original attempt").unwrap();
    let change = backend.refresh(path).unwrap().working_copy;
    backend
        .describe(path, &change, "private experiment")
        .unwrap();
    let preview = json(path, &["hide", "--stack", "--dry-run"]);
    assert_eq!(preview["dryRun"], true);
    assert_eq!(preview["entry"]["changes"].as_array().unwrap().len(), 1);
    assert!(!path.join(".jj/repo/jiji").exists());
    assert!(!path.join(".jj/jiji").exists());

    let hidden = json(path, &["hide", &change, "--stack"]);
    assert_eq!(hidden["dryRun"], false);
    assert!(!path.join("idea").exists());
    assert_eq!(
        json(path, &["hidden"])["entries"].as_array().unwrap().len(),
        1
    );
    let original_commit = hidden["entry"]["changes"][0]["commitId"].as_str().unwrap();
    let preview = json(path, &["unhide", original_commit, "--dry-run"]);
    assert_eq!(preview["entry"]["id"], hidden["entry"]["id"]);
    assert_eq!(
        json(path, &["hidden"])["entries"].as_array().unwrap().len(),
        1
    );
    let restored = json(path, &["unhide", &change]);
    assert_eq!(restored["entry"]["id"], hidden["entry"]["id"]);
    assert!(json(path, &["hidden"])["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    backend.edit_change(path, &change).unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join("idea")).unwrap(),
        "original attempt"
    );

    let failure = run(path, &["unhide", &change]);
    assert_eq!(failure.status.code(), Some(1));
    assert!(failure.stdout.is_empty());
    let error: Value = serde_json::from_slice(&failure.stderr).unwrap();
    assert_eq!(error["error"]["code"], "mutation_failed");
}
