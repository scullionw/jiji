use std::process::Command;

#[test]
fn standalone_help_exposes_the_public_workflow_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "hide", "unhide", "hidden", "stacks", "status", "publish", "ready", "draft", "land",
        "watch", "ship", "jobs", "pr", "auth", "open",
    ] {
        assert!(help.contains(command), "missing {command}");
    }
}

#[test]
fn invalid_flags_have_a_nonzero_exit_and_machine_readable_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["land", "--json", "--method", "explode"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_argument");
}

#[test]
fn offline_failure_does_not_create_a_repo_or_lock_files() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(["stacks", "--offline", "--json", "-R"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "not_a_jj_repo");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn command_names_take_precedence_over_same_named_directories() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("stacks")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .current_dir(dir.path())
        .arg("stacks")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("not inside a jj repository"));
}
