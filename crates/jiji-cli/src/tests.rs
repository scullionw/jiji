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

/// The setting that links new stacks as GitHub stacks is the app's
/// preference, read from its file; `--stack` and `--no-stack` are this
/// publish's own choice and win over it in both directions.
#[test]
fn publish_follows_the_setting_unless_this_publish_was_told() {
    let flags = |args: &[&str]| {
        let mut line = vec!["jiji", "publish"];
        line.extend_from_slice(args);
        let Some(Command::Publish(args)) = Cli::try_parse_from(line).unwrap().command else {
            panic!("publish")
        };
        (args.stack, args.no_stack)
    };
    let told = |args: &[&str], setting: bool| {
        let (stack, no_stack) = flags(args);
        publish_options(stack, no_stack, setting).stack
    };
    // Not told: as before with the setting off, new stacks linked with it on.
    assert_eq!(told(&[], false), StackLink::Follow);
    assert_eq!(told(&[], true), StackLink::LinkNew);
    // Told: the same either way.
    for setting in [false, true] {
        assert_eq!(told(&["--stack"], setting), StackLink::Link);
        assert_eq!(told(&["--no-stack"], setting), StackLink::Unlink);
    }
    assert!(Cli::try_parse_from(["jiji", "publish", "--stack", "--no-stack"]).is_err());

    // The setting is read from the app's preferences file, wherever the
    // app's data directory is. No file, as on a machine without the app,
    // is the setting off.
    let dir = tempfile::tempdir().unwrap();
    assert!(!prefs::link_new_stacks(dir.path()));
    std::fs::write(
        dir.path().join("prefs.json"),
        r#"{"publish.linkNewStacks": true}"#,
    )
    .unwrap();
    assert!(prefs::link_new_stacks(dir.path()));
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

/// A pull request someone else opened on the fixture's stack: `head` into
/// `base`, by `author`.
fn their_pr(f: &Fixture, number: u64, head: &str, base: &str, author: &str) -> PrSummary {
    PrSummary {
        number,
        title: format!("{head}, by {author}"),
        url: format!("https://github.com/org/repo/pull/{number}"),
        head_branch: head.into(),
        base_branch: base.into(),
        author: Some(author.into()),
        ..f.prs.report.prs[0].clone()
    }
}

/// `jiji stacks` and `jiji status` say that a pull request by someone else
/// sits on the stack, by number and author, apart from the stack's own
/// landing order.
#[test]
fn status_and_stacks_name_someone_elses_pull_requests_on_the_stack() {
    let f = fixture();
    let mut report = f.prs.report.clone();
    report.prs.push(their_pr(&f, 900, "their-top", "ws/tu3-emission", "kim"));
    report.prs.push(their_pr(&f, 901, "their-mid", "ws/tu2", "sam"));
    let prs = RepoPrState::new(report, "org");
    let stacks = discover_stacks(&f.snapshot, Some(&prs));

    let listing = output::stacks("repo", &stacks, 0, &[]);
    assert!(
        listing.contains(
            "    + #901 by @sam on ws/tu2, #900 by @kim on ws/tu3-emission (someone else's)\n\n"
        ),
        "{listing}"
    );

    let detail = output::stack_detail(&select_stack(&stacks, Some("ws/tu3-emission")).unwrap());
    // The landing order is the stack's own five, as before.
    assert!(detail.contains("  5. #757"), "{detail}");
    assert!(!detail.contains("  6. "), "{detail}");
    let theirs = detail
        .split_once("On this stack, by someone else (not a bookmark here)\n")
        .expect("a section for someone else's pull requests")
        .1;
    assert!(
        theirs.starts_with(
            "  #901 by @sam  their-mid → ws/tu2\n     https://github.com/org/repo/pull/901\n  \
             #900 by @kim  their-top → ws/tu3-emission\n"
        ),
        "{theirs}"
    );
    assert!(
        theirs.contains("Publish and Land never merge these and never push to their branches"),
        "{theirs}"
    );
    // And how to make one the user's to finish, by the name of the command.
    assert!(
        theirs.contains(
            "  To push one over the line yourself, take it over: jiji take-over NUMBER\n  \
             makes its branch a bookmark here and its pull request a part of this stack,\n  \
             to publish and to land. Nothing is pushed until you publish.\n"
        ),
        "{theirs}"
    );
    // The fixture's changes are mutable: nothing to say about jj.
    assert!(!theirs.contains("immutable"), "{theirs}");
    // Once a fetch has brought their branches, jj calls the changes under
    // them immutable, and the status says why and what to do about it.
    let mut held = select_stack(&stacks, Some("ws/tu3-emission")).unwrap();
    held.segments[0].changes[0].held = true;
    let detail = output::stack_detail(&held);
    assert!(
        detail.contains(
            "  jj calls the changes under them immutable while their branches build on\n  \
             them. Publish and Land work as usual. Taking the pull request over makes\n  \
             them yours to edit again.\n"
        ),
        "{detail}"
    );

    // A prefix of the stack names only what rests on that prefix.
    let lower = output::stack_detail(&select_stack(&stacks, Some("ws/tu2")).unwrap());
    assert!(lower.contains("#901 by @sam") && !lower.contains("#900"), "{lower}");
    // And a stack nobody else is on says nothing of the kind.
    let plain = output::stack_detail(&select_stack(&f.stacks, Some("ws/tu2")).unwrap());
    assert!(!plain.contains("someone else"), "{plain}");
}

/// The steps a plan takes for someone else's pull request are told by
/// number and author, and a ship tells what it does to each pull request.
#[test]
fn plans_name_each_pull_request_they_touch() {
    let land = LandPlan {
        head_bookmark: "upper".into(),
        remote: "origin".into(),
        base_branch: "main".into(),
        options: LandOptions::default(),
        segments: vec![],
        actions: vec![
            LandAction::RetargetOther {
                number: 9,
                author: Some("kim".into()),
                branch: "their-top".into(),
                from_base: "upper".into(),
                to_base: "main".into(),
            },
            // An account that has since been deleted has no name.
            LandAction::RetargetOther {
                number: 10,
                author: None,
                branch: "ghost".into(),
                from_base: "upper".into(),
                to_base: "main".into(),
            },
        ],
        blockers: vec![],
        warnings: vec![],
    };
    let text = output::land_plan(&land);
    assert!(
        text.contains(
            "  1. Retarget #9 by @kim (their-top) to main: its base upper lands and is deleted\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("  2. Retarget #10 (ghost) to main: its base upper lands and is deleted\n"),
        "{text}"
    );
    // Nothing in a land plan rebases or pushes a branch of theirs.
    assert!(!text.contains("their-top (#9") && !text.contains("Rebase ghost"), "{text}");

    let ship = ShipPlan {
        head_change: "b2".into(),
        head_title: "feat: top".into(),
        trunk_bookmark: "main".into(),
        remote: "origin".into(),
        change_ids: vec!["b1".into(), "b2".into()],
        actions: vec![
            ShipAction::PushBranches {
                bookmarks: vec!["bottom".into(), "top".into()],
                remote: "origin".into(),
            },
            ShipAction::Unstack {
                stack: 90,
                members: vec![7, 8],
            },
            ShipAction::RetargetPr {
                number: 8,
                bookmark: "top".into(),
                from_base: "bottom".into(),
                to_base: "main".into(),
            },
            ShipAction::PushTrunk {
                bookmark: "main".into(),
                remote: "origin".into(),
            },
            ShipAction::ConfirmMerged { numbers: vec![7, 8] },
            ShipAction::RetargetAbove {
                number: 9,
                author: Some("kim".into()),
                theirs: true,
                branch: "their-top".into(),
                from_base: "top".into(),
                to_base: "main".into(),
            },
            ShipAction::RetargetAbove {
                number: 10,
                author: Some("me".into()),
                theirs: false,
                branch: "next".into(),
                from_base: "top".into(),
                to_base: "main".into(),
            },
        ],
        blockers: vec![],
        warnings: vec![],
    };
    let text = output::ship_plan(&ship);
    for line in [
        "  1. Push bottom, top to origin, so each pull request has the commit that ships\n",
        "  2. Dissolve GitHub stack 90 (#7 and #8): its pull requests cannot be retargeted inside it\n",
        "  3. Retarget #8 (top) to main, so the push merges it\n",
        "  4. Push main to origin\n",
        "  5. Check that GitHub marked #7 and #8 merged\n",
        "  6. Retarget #9 by @kim (their-top) to main; it stays open\n",
        // One's own next pull request needs no author.
        "  7. Retarget #10 (next) to main; it stays open\n",
    ] {
        assert!(text.contains(line), "{line:?} in {text}");
    }
    // The same plan as JSON carries each pull request by number.
    let json = serde_json::to_value(&ship).unwrap();
    assert_eq!(json["actions"][2]["kind"], "retargetPr");
    assert_eq!(json["actions"][2]["number"], 8);
    assert_eq!(json["actions"][4]["numbers"], serde_json::json!([7, 8]));
    assert_eq!(json["actions"][5]["theirs"], true);
}

/// "Take over" is a command of its own, found in the help by those
/// words, and it takes a pull request's number or its branch. So does
/// the way back.
#[test]
fn take_over_and_give_back_are_commands_found_by_their_names() {
    let help = Cli::command().render_help().to_string();
    assert!(
        help.contains("take-over  Take over someone else's pull request"),
        "{help}"
    );
    assert!(
        help.contains("give-back  Give back a pull request you took over"),
        "{help}"
    );
    for (args, giving_back, target, dry_run) in [
        (vec!["jiji", "take-over", "9"], false, "9", false),
        (vec!["jiji", "take-over", "#9", "--dry-run"], false, "#9", true),
        (vec!["jiji", "take-over", "kim/fix"], false, "kim/fix", false),
        (vec!["jiji", "give-back", "9", "--dry-run"], true, "9", true),
        (vec!["jiji", "give-back", "kim/fix"], true, "kim/fix", false),
    ] {
        let parsed = Cli::try_parse_from(args).unwrap();
        let args = match parsed.command {
            Some(Command::TakeOver(args)) if !giving_back => args,
            Some(Command::GiveBack(args)) if giving_back => args,
            other => panic!("{other:?}"),
        };
        assert_eq!((args.target.as_str(), args.dry_run), (target, dry_run));
    }
    // Which pull request is never guessed.
    assert!(Cli::try_parse_from(["jiji", "take-over"]).is_err());
    assert!(Cli::try_parse_from(["jiji", "give-back"]).is_err());
    assert!(Cli::try_parse_from(["jiji", "take-over", "9", "--force"]).is_err());
}

/// What `take-over` and `give-back` print before anything runs, and
/// after: the steps, what they mean, and what stands in the way.
#[test]
fn take_over_prints_what_it_will_do_and_what_it_means() {
    let plan = TakeOverPlan {
        giving_back: false,
        number: 9,
        author: Some("kim".into()),
        title: "A helping change".into(),
        branch: "kim/fix".into(),
        base_branch: Some("profile".into()),
        remote: "origin".into(),
        actions: vec![
            TakeOverAction::Fetch {
                remote: "origin".into(),
            },
            TakeOverAction::TakeOver {
                bookmark: "kim/fix".into(),
                remote: "origin".into(),
                number: 9,
                author: Some("kim".into()),
            },
        ],
        blockers: vec![],
        warnings: vec!["Nothing is pushed now.".into()],
    };
    assert_eq!(
        output::take_over_plan(&plan),
        "Take over #9 by @kim · kim/fix → profile\n  \
         1. Fetch origin\n  \
         2. Make kim/fix a bookmark here, at what origin has for #9 now\n  \
         Note: Nothing is pushed now.\n"
    );
    let refused = TakeOverPlan {
        blockers: vec!["#9 by @kim is from a fork".into()],
        ..plan.clone()
    };
    assert!(
        output::take_over_plan(&refused).contains("  Blocked: #9 by @kim is from a fork\n"),
        "{}",
        output::take_over_plan(&refused)
    );
    let outcome = TakeOverOutcome {
        steps: vec![
            TakeOverStep {
                action: plan.actions[0].clone(),
                status: SubmitStepStatus::Done,
                detail: Some("Fetched 1 bookmark update from origin".into()),
            },
            TakeOverStep {
                action: plan.actions[1].clone(),
                status: SubmitStepStatus::Failed,
                detail: Some("there is no remote bookmark kim/fix@origin".into()),
            },
        ],
        failed: true,
    };
    let text = output::take_over_outcome(&outcome);
    assert!(text.starts_with("\nResults\n"), "{text}");
    assert!(
        text.contains("Fetch origin: Fetched 1 bookmark update from origin\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "Make kim/fix a bookmark here, at what origin has for #9 now: there is no remote \
             bookmark kim/fix@origin\n"
        ),
        "{text}"
    );

    let back = TakeOverPlan {
        giving_back: true,
        title: String::new(),
        base_branch: None,
        actions: vec![TakeOverAction::GiveBack {
            bookmark: "kim/fix".into(),
            remote: "origin".into(),
            number: 9,
            author: Some("kim".into()),
        }],
        warnings: vec![],
        ..plan
    };
    assert_eq!(
        output::take_over_plan(&back),
        "Give back #9 by @kim · kim/fix\n  \
         1. Remove the bookmark kim/fix here and stop following kim/fix@origin; the branch \
         stays as it is\n"
    );
    // As JSON a plan carries the pull request and each step by kind.
    let json = serde_json::to_value(&back).unwrap();
    assert_eq!(json["givingBack"], true);
    assert_eq!(json["number"], 9);
    assert_eq!(json["actions"][0]["kind"], "giveBack");
    assert_eq!(json["actions"][0]["bookmark"], "kim/fix");
}

/// A pull request that was taken over is a segment of the stack in
/// `stacks` and `status`, marked with whose it was and how to hand it
/// back; and a publish names it where it pushes its branch.
#[test]
fn a_taken_over_pull_request_is_marked_wherever_it_is_listed_or_pushed() {
    let f = fixture();
    let mut snapshot = f.snapshot.clone();
    snapshot.taken_over = vec![jiji_core::snapshot::TakenOver {
        bookmark: "ws/tu3-emission".into(),
        remote: "origin".into(),
        number: 757,
        author: Some("kim".into()),
    }];
    let stacks = discover_stacks(&snapshot, Some(&f.prs));
    let listing = output::stacks("repo", &stacks, 0, &[]);
    assert!(listing.contains("    with #757 by @kim (taken over)\n\n"), "{listing}");
    let detail = output::stack_detail(&select_stack(&stacks, Some("ws/tu3-emission")).unwrap());
    assert!(
        detail.contains(
            "     taken over from @kim: pushed and landed with this stack; give-back 757 hands \
             it back\n"
        ),
        "{detail}"
    );
    // A stack with nothing taken over says nothing of it.
    let plain = output::stacks("repo", &f.stacks, 0, &[]);
    assert!(!plain.contains("taken over"), "{plain}");

    let taken = stacks[0].segments[4].taken_over.clone();
    assert!(taken.is_some());
    let segment = |bookmark: &str, taken_over| SubmitSegment {
        bookmark: bookmark.into(),
        base: "develop".into(),
        change_ids: vec![],
        title: String::new(),
        pr: None,
        taken_over,
    };
    let plan = SubmitPlan {
        head_bookmark: "ws/tu3-emission".into(),
        remote: "origin".into(),
        base_branch: "develop".into(),
        options: SubmitOptions::default(),
        segments: vec![
            segment("ws/tu3-identity", None),
            segment("ws/tu3-emission", taken),
        ],
        actions: vec![
            SubmitAction::Push {
                bookmark: "ws/tu3-identity".into(),
                create: false,
            },
            SubmitAction::Push {
                bookmark: "ws/tu3-emission".into(),
                create: false,
            },
            SubmitAction::LinkStack {
                bookmarks: vec!["ws/tu3-identity".into(), "ws/tu3-emission".into()],
                onto: None,
                above: vec![900],
            },
            SubmitAction::LinkStack {
                bookmarks: vec![],
                onto: Some(90),
                above: vec![900],
            },
        ],
        blockers: vec![],
        warnings: vec![],
        stack_comment_preview: None,
        pr_template_path: None,
    };
    let text = output::submit_plan(&plan);
    for line in [
        "  1. Push ws/tu3-identity\n",
        "  2. Push ws/tu3-emission (#757 by @kim, taken over)\n",
        // Someone else's pull request that a link takes into the stack.
        "  3. Link as a GitHub stack: ws/tu3-identity, ws/tu3-emission, #900\n",
        "  4. Add to GitHub stack 90: #900\n",
    ] {
        assert!(text.contains(line), "{line:?} in {text}");
    }
    let outcome = SubmitOutcome {
        steps: vec![SubmitStep {
            action: plan.actions[1].clone(),
            status: SubmitStepStatus::Failed,
            detail: Some("moved on the remote".into()),
            pr: None,
        }],
        failed: true,
    };
    assert!(
        output::submit_outcome(&plan, &outcome)
            .contains("Push ws/tu3-emission (#757 by @kim, taken over): moved on the remote\n"),
        "{}",
        output::submit_outcome(&plan, &outcome)
    );
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
