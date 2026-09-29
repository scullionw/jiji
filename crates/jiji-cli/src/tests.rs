use super::*;
use args::{LandArgs, Method, Rules, WatchArgs};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    snapshot: RepoSnapshot,
    prs: RepoPrState,
    stacks: Vec<PublishStack>,
}
fn fixture() -> Fixture {
    serde_json::from_str(include_str!(
        "../../jiji-forge/tests/fixtures/publish-stack.json"
    ))
    .unwrap()
}

#[test]
fn command_tree_has_no_conflicting_arguments() {
    Cli::command().debug_assert();
}

#[test]
fn options_work_before_and_after_the_subcommand() {
    let a = Cli::try_parse_from([
        "jiji",
        "--json",
        "-R",
        "/repo",
        "land",
        "topic",
        "--dry-run",
        "--method",
        "rebase",
        "--bypass",
    ])
    .unwrap();
    assert!(a.json);
    assert_eq!(a.repo, Path::new("/repo"));
    let Some(Command::Land(args)) = a.command else {
        panic!("land")
    };
    assert_eq!(args.write.head.as_deref(), Some("topic"));
    assert!(args.write.dry_run);
    assert_eq!(
        args.rules.apply(LandOptions::default()),
        LandOptions {
            method: Some(MergeMethod::Rebase),
            bypass: true
        }
    );
    let b = Cli::try_parse_from(["jiji", "stacks", "--json", "-R", "/repo", "--all"]).unwrap();
    assert!(b.json);
    assert_eq!(b.repo, Path::new("/repo"));
}

#[test]
fn unsafe_or_unknown_flags_are_never_silently_ignored() {
    for args in [
        vec!["jiji", "land", "--method", "invalid"],
        vec!["jiji", "watch", "--bypass", "--no-bypass"],
        vec!["jiji", "watch", "--state-file"],
        vec!["jiji", "land", "--wat"],
        vec!["jiji", "publish", "--bypass"],
        vec!["jiji", "ready", "--method", "squash"],
        vec!["jiji", "draft", "--bypass"],
        vec!["jiji", "ready", "--draft"],
        vec!["jiji", "pr", "show", "0"],
        vec!["jiji", "auth", "login", "a-token-on-the-command-line"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn readiness_is_its_own_command_and_a_publish_option() {
    let parsed = Cli::try_parse_from(["jiji", "publish", "topic", "--draft", "--dry-run"]).unwrap();
    let Some(Command::Publish(args)) = parsed.command else {
        panic!("publish")
    };
    assert!(args.draft && args.write.dry_run);
    assert_eq!(args.write.head.as_deref(), Some("topic"));
    let parsed = Cli::try_parse_from(["jiji", "submit"]).unwrap();
    assert!(matches!(
        parsed.command,
        Some(Command::Publish(PublishArgs { draft: false, .. }))
    ));
    let parsed = Cli::try_parse_from(["jiji", "draft", "ws/tu2", "--json"]).unwrap();
    assert!(parsed.json);
    let Some(Command::Draft(args)) = parsed.command else {
        panic!("draft")
    };
    assert_eq!(args.head.as_deref(), Some("ws/tu2"));
    assert!(matches!(
        Cli::try_parse_from(["jiji", "ready", "--dry-run"])
            .unwrap()
            .command,
        Some(Command::Ready(WriteArgs {
            head: None,
            dry_run: true
        }))
    ));
}

#[test]
fn readiness_plans_name_each_pr_and_those_already_there() {
    let f = fixture();
    let stack = select_stack(&f.stacks, Some("ws/tu2")).unwrap();
    let text = output::readiness_plan(&plan_readiness(&stack, PrReadiness::ReadyForReview));
    assert!(text.starts_with("Mark ws/tu2 ready for review\n"), "{text}");
    assert!(
        text.contains("1. Mark #753 (ws/tu1) ready for review"),
        "{text}"
    );
    assert!(
        text.contains("2. Mark #754 (ws/tu2) ready for review"),
        "{text}"
    );
    let text = output::readiness_plan(&plan_readiness(&stack, PrReadiness::Draft));
    assert!(text.contains("No actions"), "{text}");
    assert!(text.contains("Already drafts: #753, #754\n"), "{text}");
}

#[test]
fn dry_runs_and_blocked_plans_never_call_the_executor() {
    for (dry, blocked) in [(true, false), (false, true), (true, true)] {
        let result: Option<()> =
            execute_if_ready(dry, blocked, || panic!("must not execute")).unwrap();
        assert!(result.is_none());
    }
    assert_eq!(execute_if_ready(false, false, || Ok(42)).unwrap(), Some(42));
}

#[test]
fn executor_failures_are_preserved() {
    let result: Result<Option<()>> =
        execute_if_ready(false, false, || Err(ForgeError::Network("offline".into())));
    assert_eq!(result.unwrap_err().code, "network_failed");
}

#[test]
fn current_head_uses_the_same_stack_as_the_desktop_and_can_cap_at_a_pr() {
    let f = fixture();
    assert_eq!(
        resolve_head(&f.snapshot, &f.prs, None).unwrap(),
        SubmitHead::Bookmark {
            name: "ws/tu3-emission".into()
        }
    );
    assert_eq!(
        resolve_head(&f.snapshot, &f.prs, Some("ws/tu2")).unwrap(),
        SubmitHead::Bookmark {
            name: "ws/tu2".into()
        }
    );
    let stack = select_stack(&f.stacks, Some("ws/tu2")).unwrap();
    assert_eq!(stack.pr_count, 2);
    assert_eq!(stack.head_bookmark.as_deref(), Some("ws/tu2"));
}

#[test]
fn ambiguous_heads_never_pick_an_arbitrary_stack() {
    let mut f = fixture();
    for stack in &mut f.stacks {
        stack.is_active = false;
    }
    assert!(select_stack(&f.stacks, None).is_err());
    assert!(resolve_head(&f.snapshot, &f.prs, Some("c")).is_err());
    assert!(select_stack(&f.stacks, Some("not-a-head")).is_err());
}

#[test]
fn merge_rules_preserve_saved_values_until_explicitly_overridden() {
    let saved = LandOptions {
        method: Some(MergeMethod::Merge),
        bypass: true,
    };
    assert_eq!(Rules::default().apply(saved), saved);
    let reset = Rules {
        method: Some(Method::Auto),
        no_bypass: true,
        ..Rules::default()
    };
    assert_eq!(reset.apply(saved), LandOptions::default());
}

#[test]
fn repository_detection_accepts_nested_files_and_paths_with_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("a repo");
    std::fs::create_dir_all(repo.join(".jj")).unwrap();
    std::fs::create_dir_all(repo.join("src/nested")).unwrap();
    std::fs::write(repo.join("src/nested/a.rs"), "").unwrap();
    assert_eq!(
        repo_root(&repo.join("src/nested/a.rs")).unwrap(),
        repo.canonicalize().unwrap()
    );
    assert!(repo_root(dir.path()).is_err());
}

#[test]
fn only_one_cli_writer_can_own_a_repository_and_drop_releases_it() {
    let dir = tempfile::tempdir().unwrap();
    let lock = MutationLock::acquire(dir.path()).unwrap();
    assert!(MutationLock::acquire(dir.path()).is_err());
    drop(lock);
    assert!(MutationLock::acquire(dir.path()).is_ok());
}

#[test]
fn human_output_shows_the_five_prs_in_order_and_removes_terminal_controls() {
    let f = fixture();
    let text = output::stack_detail(&f.stacks[0]);
    let positions: Vec<_> = (753..=757)
        .map(|n| text.find(&format!("#{n}")).unwrap())
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(text.contains("develop"));
    assert_eq!(output::clean("title\u{1b}[2J\nnext"), "title [2J next");
}

#[test]
fn watch_accepts_resume_and_state_file_alongside_merge_rules() {
    let parsed = Cli::try_parse_from([
        "jiji",
        "watch",
        "topic",
        "--resume",
        "--state-file",
        "/tmp/my job.json",
        "--method",
        "squash",
        "--no-bypass",
        "--dry-run",
    ])
    .unwrap();
    let Some(Command::Watch(WatchArgs {
        resume,
        state_file,
        land: LandArgs { write, rules },
    })) = parsed.command
    else {
        panic!("watch")
    };
    assert!(resume && write.dry_run);
    assert_eq!(state_file.unwrap(), Path::new("/tmp/my job.json"));
    assert!(
        !rules
            .apply(LandOptions {
                method: None,
                bypass: true
            })
            .bypass
    );
}
