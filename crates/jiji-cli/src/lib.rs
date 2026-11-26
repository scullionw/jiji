//! First-class terminal host for Jiji's shared workflow engine.
mod args;
mod output;
mod vcs;
mod watch;

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser};
use jiji_core::{JjBackend, RepoBackend, RepoSnapshot};
use jiji_forge::*;
use serde_json::json;

use args::{AuthCommand, Cli, Command, PrCommand, PublishArgs, WriteArgs};
use output::Output;
use vcs::LocalVcs;

#[derive(Debug)]
struct Error {
    code: &'static str,
    message: String,
    exit: i32,
}
type Result<T> = std::result::Result<T, Error>;

impl Error {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit: 1,
        }
    }
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_argument",
            message: message.into(),
            exit: 2,
        }
    }
}
impl From<ForgeError> for Error {
    fn from(error: ForgeError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}
impl From<jiji_core::BackendError> for Error {
    fn from(error: jiji_core::BackendError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::new("io_failed", error.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::new("json_failed", error.to_string())
    }
}

/// The desktop binary and standalone executable enter the same parser and host.
pub fn run(args: impl IntoIterator<Item = OsString>) -> i32 {
    let mut args: Vec<_> = args.into_iter().collect();
    // Preserve the installed launcher's `jiji .` / `jiji /path` interface.
    if args.len() == 2
        && !args[1].to_string_lossy().starts_with('-')
        && Cli::command().find_subcommand(&args[1]).is_none()
        && Path::new(&args[1]).exists()
    {
        args.insert(1, "open".into());
    }
    let json_errors = args.iter().any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            if json_errors && code != 0 {
                let _ = writeln!(
                    io::stderr(),
                    "{}",
                    json!({"error": {"code": "invalid_argument", "message": error.to_string()}})
                );
            } else {
                let _ = error.print();
            }
            return code;
        }
    };
    let mut output = Output { json: cli.json };
    match execute(cli, &mut output) {
        Ok(exit) => exit,
        Err(error) => {
            if output.json {
                let _ = writeln!(
                    io::stderr(),
                    "{}",
                    json!({"error": {"code": error.code, "message": error.message}})
                );
            } else {
                let _ = writeln!(io::stderr(), "jiji: {}", output::clean(&error.message));
            }
            error.exit
        }
    }
}

fn execute(cli: Cli, out: &mut Output) -> Result<i32> {
    let command = cli.command.unwrap_or(Command::Open { path: None });
    if let Command::Open { path } = command {
        return open_app(path.as_deref().unwrap_or(&cli.repo), out);
    }
    if let Command::Auth { command } = command {
        return auth(command, out);
    }
    let path = repo_root(&cli.repo)?;
    let vcs = LocalVcs {
        backend: JjBackend::default(),
        path,
    };
    match command {
        Command::Hide {
            revision,
            stack,
            dry_run,
        } => {
            let _lock = if dry_run {
                None
            } else {
                Some(MutationLock::acquire(&vcs.path)?)
            };
            let result = vcs.backend.hide(&vcs.path, &revision, stack, dry_run)?;
            let verb = if dry_run { "Would hide" } else { "Hidden" };
            let mut text = format!("{verb} {} change(s)\n", result.changes.len());
            for change in &result.changes {
                text.push_str(&format!(
                    "  {} {}\n",
                    &change.change_id[..12],
                    output::clean(&change.description)
                ));
            }
            text.push_str(&format!("Recover with: jiji unhide {}\n", &result.id[..12]));
            out.emit(&json!({"dryRun": dry_run, "entry": result}), text)?;
            Ok(0)
        }
        Command::Unhide { id, dry_run } => {
            let _lock = if dry_run {
                None
            } else {
                Some(MutationLock::acquire(&vcs.path)?)
            };
            let result = vcs.backend.unhide(&vcs.path, &id, dry_run)?;
            let verb = if dry_run { "Would restore" } else { "Restored" };
            out.emit(
                &json!({"dryRun": dry_run, "entry": result}),
                format!(
                    "{verb} {} change(s); use jj edit {} to work on them\n",
                    result.changes.len(),
                    &result.id[..12]
                ),
            )?;
            Ok(0)
        }
        Command::Hidden => {
            let entries = vcs.backend.hidden(&vcs.path)?;
            let mut text = String::new();
            for entry in &entries {
                text.push_str(&format!(
                    "{}  {} change(s)  {}\n",
                    &entry.id[..12],
                    entry.changes.len(),
                    output::clean(&entry.changes[0].description)
                ));
            }
            if entries.is_empty() {
                text.push_str("No hidden changes.\n");
            }
            out.emit(&json!({"entries": entries}), text)?;
            Ok(0)
        }
        Command::Stacks {
            all,
            search,
            offline,
        } => {
            let snapshot = vcs.snapshot()?;
            let forge = read_forge(&snapshot, offline)?;
            let mut stacks = discover_stacks(&snapshot, forge.as_ref().map(|f| &f.prs));
            let local_count = stacks.iter().filter(|s| s.pr_count == 0).count();
            let has_published = stacks.iter().any(|s| s.pr_count > 0);
            let query = search.as_deref().unwrap_or("").to_lowercase();
            stacks.retain(|s| {
                (all || !has_published || s.pr_count > 0 || !query.is_empty())
                    && s.search_text.contains(&query)
            });
            let hidden = if all || !query.is_empty() || !has_published {
                0
            } else {
                local_count
            };
            let warnings = snapshot_warnings(&snapshot, forge.as_ref());
            out.emit(
                &json!({"repoPath": snapshot.repo_path, "trunk": snapshot.trunk_bookmark,
                "stacks": stacks, "localHidden": hidden, "warnings": warnings}),
                output::stacks(&snapshot.repo_name, &stacks, hidden, &warnings),
            )?;
            Ok(0)
        }
        Command::Status { head, offline } => {
            let snapshot = vcs.snapshot()?;
            let forge = read_forge(&snapshot, offline)?;
            let stacks = discover_stacks(&snapshot, forge.as_ref().map(|f| &f.prs));
            let stack = select_stack(&stacks, head.as_deref())?;
            let plan = match (&forge, &stack.head_bookmark) {
                (Some(f), Some(head)) if stack.submit_head.is_some() && stack.pr_count > 0 => {
                    Some(f.land_plan(&snapshot, head, &vcs, &LandOptions::default())?)
                }
                _ => None,
            };
            let mut text = output::stack_detail(&stack);
            if let Some(plan) = &plan {
                text.push_str(&output::land_plan(plan));
            }
            let warnings = snapshot_warnings(&snapshot, forge.as_ref());
            for warning in &warnings {
                text.push_str(&format!("\nNote: {warning}\n"));
            }
            out.emit(
                &json!({"stack": stack, "plan": plan, "warnings": warnings}),
                text,
            )?;
            Ok(0)
        }
        Command::Publish(args) => publish(&vcs, args, out),
        Command::Ready(args) => readiness(&vcs, args, PrReadiness::ReadyForReview, out),
        Command::Draft(args) => readiness(&vcs, args, PrReadiness::Draft, out),
        Command::Land(args) => {
            let _lock = if args.write.dry_run {
                None
            } else {
                Some(MutationLock::acquire(&vcs.path)?)
            };
            let snapshot = planning_snapshot(&vcs, args.write.dry_run)?;
            let forge = ForgeContext::connect(&snapshot)?;
            let head = resolve_head(&snapshot, &forge.prs, args.write.head.as_deref())?;
            let head = require_bookmark(head)?;
            let plan = forge.land_plan(
                &snapshot,
                &head,
                &vcs,
                &args.rules.apply(LandOptions::default()),
            )?;
            out.preflight(&output::land_plan(&plan))?;
            let side = forge.land_side();
            let outcome = execute_if_ready(args.write.dry_run, !plan.blockers.is_empty(), || {
                execute_land(&plan, &vcs, &side)
            })?;
            let mut text = String::new();
            if let Some(outcome) = &outcome {
                text.push_str(&output::land_outcome(outcome));
            }
            let exit = outcome
                .as_ref()
                .map(|o| if o.failed { 1 } else { 0 })
                .unwrap_or(if plan.blockers.is_empty() { 0 } else { 3 });
            out.emit(&json!({"plan": plan, "outcome": outcome}), text)?;
            Ok(exit)
        }
        Command::Watch(args) => watch::run(&vcs, args, out),
        Command::Ship(args) => ship(&vcs, args, out),
        Command::Jobs { state_file } => {
            let state_file = state_file.unwrap_or_else(|| watch::default_state_file(&vcs.path));
            let records: Vec<_> = watch::read_records(&state_file)?
                .into_iter()
                .filter(|r| Path::new(&r.repo_path) == vcs.path)
                .collect();
            let mut text = format!("Saved auto-land jobs · {}\n", state_file.display());
            if records.is_empty() {
                text.push_str("  No saved jobs.\n");
            }
            for record in &records {
                text.push_str(&output::job(&record.state));
            }
            out.emit(&json!({"stateFile": state_file, "records": records}), text)?;
            Ok(0)
        }
        Command::Pr { command } => pr(&vcs, command, out),
        Command::Open { .. } | Command::Auth { .. } => unreachable!(),
    }
}

fn repo_root(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .map_err(|err| Error::new("path_missing", format!("{}: {err}", path.display())))?;
    let directory = if path.is_file() {
        path.parent().unwrap()
    } else {
        &path
    };
    directory
        .ancestors()
        .find(|p| p.join(".jj").is_dir())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Error::new(
                "not_a_jj_repo",
                format!("{} is not inside a jj repository", path.display()),
            )
        })
}

/// Only write commands snapshot the working copy. A dry run never imports,
/// fetches, or records local changes as a side effect of reading its plan.
fn planning_snapshot(vcs: &LocalVcs, dry_run: bool) -> Result<RepoSnapshot> {
    Ok(if dry_run {
        vcs.snapshot()?
    } else {
        vcs.backend.refresh(&vcs.path)?
    })
}

struct ForgeContext {
    repo: ForgeRepo,
    client: GitHubClient,
    prs: RepoPrState,
}
impl ForgeContext {
    fn connect(snapshot: &RepoSnapshot) -> Result<Self> {
        let repo = detect_github_repo(
            snapshot
                .git_remotes
                .iter()
                .map(|r| (r.name.as_str(), r.url.as_str())),
        )
        .ok_or_else(no_github_remote)?;
        let token =
            resolve_token(&KeychainTokenStore::new(&repo.host))?.ok_or(ForgeError::NoToken)?;
        let client = GitHubClient::for_repo(&repo, &token.token)?;
        let prs = RepoPrState::new(client.open_prs(&repo.owner, &repo.name)?, &repo.owner);
        Ok(Self { repo, client, prs })
    }
    fn land_side(&self) -> LandRepoForge<'_> {
        LandRepoForge {
            repo: &self.repo,
            client: &self.client,
        }
    }
    fn land_plan(
        &self,
        snapshot: &RepoSnapshot,
        head: &str,
        vcs: &LocalVcs,
        options: &LandOptions,
    ) -> Result<LandPlan> {
        Ok(plan_land(
            snapshot,
            &self.prs,
            &self.repo,
            head,
            &self.land_side(),
            vcs,
            options,
        )?)
    }
}

fn read_forge(snapshot: &RepoSnapshot, offline: bool) -> Result<Option<ForgeContext>> {
    if offline
        || detect_github_repo(
            snapshot
                .git_remotes
                .iter()
                .map(|r| (r.name.as_str(), r.url.as_str())),
        )
        .is_none()
    {
        return Ok(None);
    }
    ForgeContext::connect(snapshot).map(Some)
}

fn snapshot_warnings(snapshot: &RepoSnapshot, forge: Option<&ForgeContext>) -> Vec<String> {
    let mut warnings = Vec::new();
    if snapshot.truncated {
        warnings.push("Local history is truncated; some stacks may be incomplete.".into());
    }
    if forge.is_some_and(|f| f.prs.report.truncated) {
        warnings.push(
            "GitHub returned its 100 most recently updated PRs; older stacks may be incomplete."
                .into(),
        );
    }
    warnings
}

fn select_stack(stacks: &[PublishStack], head: Option<&str>) -> Result<PublishStack> {
    if let Some(head) = head.filter(|h| *h != "@") {
        if let Some(stack) = stacks.iter().find(|s| {
            s.key == head
                || s.head_bookmark.as_deref() == Some(head)
                || s.head_change.as_deref() == Some(head)
        }) {
            return Ok(stack.clone());
        }
        if let Some(stack) = stacks.iter().find_map(|s| s.through_bookmark(head)) {
            return Ok(stack);
        }
        let matches: Vec<_> = stacks
            .iter()
            .filter(|s| {
                s.head_change
                    .as_deref()
                    .is_some_and(|id| id.starts_with(head))
            })
            .collect();
        if matches.len() == 1 {
            return Ok(matches[0].clone());
        }
        return Err(Error::usage(format!(
            "No unique stack matches {head:?}. Run `jiji stacks --all` and specify its head."
        )));
    }
    let active: Vec<_> = stacks.iter().filter(|s| s.is_active).collect();
    if active.len() == 1 {
        return Ok(active[0].clone());
    }
    if stacks.len() == 1 {
        return Ok(stacks[0].clone());
    }
    Err(Error::usage(
        "No unique current stack. Run `jiji stacks --all` and specify a head bookmark.",
    ))
}

fn resolve_head(
    snapshot: &RepoSnapshot,
    prs: &RepoPrState,
    requested: Option<&str>,
) -> Result<SubmitHead> {
    if let Some(head) = requested.filter(|h| *h != "@") {
        if snapshot
            .bookmarks
            .iter()
            .any(|b| b.name == head && b.is_local && !b.is_trunk)
        {
            return Ok(SubmitHead::Bookmark { name: head.into() });
        }
        let matches: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|n| n.id.starts_with(head) || n.change_id.starts_with(head))
            .collect();
        if matches.len() == 1 {
            return Ok(SubmitHead::Change {
                change: matches[0].id.clone(),
            });
        }
        return Err(Error::usage(format!(
            "No unique local bookmark or change matches {head:?}. Run `jiji stacks --all`."
        )));
    }
    select_stack(&discover_stacks(snapshot, Some(prs)), None)?
        .submit_head
        .clone()
        .ok_or_else(|| {
            Error::usage(
                "This stack is only on GitHub. Fetch its head with `jiji pr fetch NUMBER` first.",
            )
        })
}

fn require_bookmark(head: SubmitHead) -> Result<String> {
    match head {
        SubmitHead::Bookmark { name } => Ok(name),
        SubmitHead::Change { .. } => Err(Error::usage("Landing needs a published bookmark. Run `jiji publish` first, or specify its bookmark.")),
    }
}

fn head_change(snapshot: &RepoSnapshot, head: &SubmitHead) -> Result<String> {
    match head {
        SubmitHead::Change { change } => Ok(change.clone()),
        SubmitHead::Bookmark { name } => snapshot
            .bookmarks
            .iter()
            .find(|b| b.name == *name && b.is_local)
            .map(|b| b.target.clone())
            .ok_or_else(|| Error::usage(format!("Bookmark {name} is no longer local"))),
    }
}

/// Central execution gate used by all plan commands: blocked and dry-run
/// plans can never reach a writer, including when they contain actions.
fn execute_if_ready<T>(
    dry_run: bool,
    blocked: bool,
    execute: impl FnOnce() -> std::result::Result<T, ForgeError>,
) -> Result<Option<T>> {
    if dry_run || blocked {
        Ok(None)
    } else {
        Ok(Some(execute()?))
    }
}

fn publish(vcs: &LocalVcs, args: PublishArgs, out: &mut Output) -> Result<i32> {
    let PublishArgs {
        write: args,
        draft,
        stack,
        no_stack,
    } = args;
    let options = publish_options(
        stack,
        no_stack,
        prefs::link_new_stacks(&prefs::data_dir()),
    );
    let _lock = if args.dry_run {
        None
    } else {
        Some(MutationLock::acquire(&vcs.path)?)
    };
    let snapshot = planning_snapshot(vcs, args.dry_run)?;
    let forge = ForgeContext::connect(&snapshot)?;
    let head = resolve_head(&snapshot, &forge.prs, args.head.as_deref())?;
    let template = vcs
        .backend
        .trunk_text_file(&vcs.path, &pr_template_candidates())?
        .map(|(path, text)| PrTemplate { path, text });
    let side = RepoForge {
        client: &forge.client,
        repo: &forge.repo,
    };
    let mut plan = plan_submit(
        &snapshot,
        &forge.prs,
        &forge.repo,
        &head,
        &side,
        template.as_ref(),
        &options,
    )?;
    if draft {
        plan.open_new_prs_as_drafts();
    }
    out.preflight(&output::submit_plan(&plan))?;
    let outcome = execute_if_ready(args.dry_run, !plan.blockers.is_empty(), || {
        execute_submit(&plan, vcs, &side)
    })?;
    let mut text = String::new();
    if let Some(outcome) = &outcome {
        text.push_str(&output::submit_outcome(outcome));
    }
    let exit = outcome
        .as_ref()
        .map(|o| if o.failed { 1 } else { 0 })
        .unwrap_or(if plan.blockers.is_empty() { 0 } else { 3 });
    out.emit(&json!({"plan": plan, "outcome": outcome}), text)?;
    Ok(exit)
}

/// What `--stack` and `--no-stack` ask of a publish, under the setting
/// that links new stacks as GitHub stacks (the app's Settings keep it, and
/// the command reads it from the app's preferences). Either flag is this
/// publish's own choice and wins over the setting; without one, the
/// setting decides.
fn publish_options(stack: bool, no_stack: bool, link_new_stacks: bool) -> SubmitOptions {
    SubmitOptions {
        stack: match (stack, no_stack) {
            (true, _) => StackLink::Link,
            (_, true) => StackLink::Unlink,
            _ => StackLink::Follow,
        },
    }
    .under_setting(link_new_stacks)
}

/// Flip the draft flag on a stack's PRs. Only GitHub changes: no snapshot
/// refresh, push, retarget, or text update.
fn readiness(
    vcs: &LocalVcs,
    args: WriteArgs,
    target: PrReadiness,
    out: &mut Output,
) -> Result<i32> {
    let _lock = if args.dry_run {
        None
    } else {
        Some(MutationLock::acquire(&vcs.path)?)
    };
    let snapshot = vcs.snapshot()?;
    let forge = ForgeContext::connect(&snapshot)?;
    let stacks = discover_stacks(&snapshot, Some(&forge.prs));
    let plan = plan_readiness(&select_stack(&stacks, args.head.as_deref())?, target);
    out.preflight(&output::readiness_plan(&plan))?;
    let side = RepoForge {
        client: &forge.client,
        repo: &forge.repo,
    };
    let outcome = execute_if_ready(args.dry_run, !plan.blockers.is_empty(), || {
        execute_readiness(&plan, &side)
    })?;
    let mut text = String::new();
    if let Some(outcome) = &outcome {
        text.push_str(&output::readiness_outcome(outcome));
    }
    let exit = outcome
        .as_ref()
        .map(|o| if o.failed { 1 } else { 0 })
        .unwrap_or(if plan.blockers.is_empty() { 0 } else { 3 });
    out.emit(&json!({"plan": plan, "outcome": outcome}), text)?;
    Ok(exit)
}

fn ship(vcs: &LocalVcs, args: WriteArgs, out: &mut Output) -> Result<i32> {
    let _lock = if args.dry_run {
        None
    } else {
        Some(MutationLock::acquire(&vcs.path)?)
    };
    let mut snapshot = planning_snapshot(vcs, args.dry_run)?;
    let forge = match read_forge(&snapshot, false) {
        Err(error) if error.code == "no_token" => None,
        result => result?,
    };
    let empty = RepoPrState::new(
        PrStateReport {
            prs: vec![],
            truncated: false,
        },
        "",
    );
    let head = resolve_head(
        &snapshot,
        forge.as_ref().map(|f| &f.prs).unwrap_or(&empty),
        args.head.as_deref(),
    )?;
    let change = head_change(&snapshot, &head)?;
    let remote = forge
        .as_ref()
        .map(|f| f.repo.remote.clone())
        .or_else(|| {
            snapshot
                .git_remotes
                .iter()
                .find(|r| r.name == "origin")
                .or_else(|| snapshot.git_remotes.first())
                .map(|r| r.name.clone())
        })
        .ok_or_else(|| Error::usage("This repository has no git remote to ship to"))?;
    if !args.dry_run {
        LandVcs::git_fetch(vcs, &remote)?;
        snapshot = vcs.snapshot()?;
    }
    let mut plan = plan_ship(&snapshot, forge.as_ref().map(|f| &f.prs), &remote, &change)?;
    if args.dry_run {
        plan.warnings.push(
            "Dry run uses the last fetched trunk. Execution fetches and replans before shipping."
                .into(),
        );
    }
    if forge.is_none() {
        plan.warnings.push(jiji_forge::SHIPPED_WITHOUT_GITHUB.into());
    }
    out.preflight(&output::ship_plan(&plan))?;
    // GitHub, for a plan that finishes pull requests. A plan made without
    // it has none to finish.
    let side = forge.as_ref().map(|f| RepoForge {
        client: &f.client,
        repo: &f.repo,
    });
    let outcome = execute_if_ready(args.dry_run, !plan.blockers.is_empty(), || {
        execute_ship(&plan, vcs, side.as_ref().map(|side| side as &dyn ShipForge))
    })?;
    let mut text = String::new();
    if let Some(outcome) = &outcome {
        text.push_str(&output::ship_outcome(outcome));
    }
    let exit = outcome
        .as_ref()
        .map(|o| if o.failed { 1 } else { 0 })
        .unwrap_or(if plan.blockers.is_empty() { 0 } else { 3 });
    out.emit(&json!({"plan": plan, "outcome": outcome}), text)?;
    Ok(exit)
}

fn pr(vcs: &LocalVcs, command: PrCommand, out: &mut Output) -> Result<i32> {
    let snapshot = vcs.snapshot()?;
    let forge = ForgeContext::connect(&snapshot)?;
    let number = match &command {
        PrCommand::List => None,
        PrCommand::Show { number }
        | PrCommand::Fetch { number, .. }
        | PrCommand::Rerun { number, .. } => Some(*number),
    };
    if number == Some(0) {
        return Err(Error::usage("PR numbers must be positive"));
    }
    if let PrCommand::List = command {
        let mut text = format!(
            "{}/{} · open pull requests\n",
            forge.repo.owner, forge.repo.name
        );
        for pr in &forge.prs.report.prs {
            text.push_str(&output::pr_row(pr));
        }
        out.emit(&forge.prs.report, text)?;
        return Ok(0);
    }
    let number = number.unwrap();
    let pr = forge
        .client
        .pr_by_number(&forge.repo.owner, &forge.repo.name, number)?;
    match command {
        PrCommand::Show { .. } => out.emit(
            &pr,
            format!(
                "{}  {} → {}\n  {}\n",
                output::pr_row(&pr),
                pr.head_branch,
                pr.base_branch,
                pr.url
            ),
        )?,
        PrCommand::Fetch {
            bookmark, dry_run, ..
        } => {
            let bookmark = bookmark.unwrap_or_else(|| format!("review/pr-{number}"));
            if snapshot
                .bookmarks
                .iter()
                .any(|b| b.name == bookmark && b.is_local)
            {
                return Err(Error::usage(format!(
                    "Local bookmark {bookmark} already exists; choose --bookmark NAME"
                )));
            }
            let outcome = if dry_run {
                None
            } else {
                let _lock = MutationLock::acquire(&vcs.path)?;
                Some(
                    vcs.backend
                        .fetch_pr_head(&vcs.path, &forge.repo.remote, number, &bookmark)?,
                )
            };
            let text = outcome
                .as_ref()
                .map(|o| format!("{}\n", o.summary))
                .unwrap_or_else(|| {
                    format!(
                        "Fetch #{} into {} from {}\n",
                        number, bookmark, forge.repo.remote
                    )
                });
            out.emit(
                &json!({"pr": pr, "bookmark": bookmark, "outcome": outcome}),
                text,
            )?;
        }
        PrCommand::Rerun { dry_run, .. } => {
            let report = if dry_run {
                None
            } else {
                Some(rerun_failed_ci(
                    &forge.client,
                    &forge.repo,
                    &pr.head_commit,
                )?)
            };
            let text = match &report {
                Some(r) => {
                    let mut text = format!(
                        "#{} · {} workflow runs restarted\n",
                        pr.number,
                        r.rerun.len()
                    );
                    for run in &r.rerun {
                        text.push_str(&format!("  ✓ {}\n", output::clean(run)));
                    }
                    for refused in &r.refused {
                        text.push_str(&format!("  Refused: {}\n", output::clean(refused)));
                    }
                    text
                }
                None => format!(
                    "Rerun failed Actions runs for #{} at {}\n",
                    pr.number, pr.head_commit
                ),
            };
            let exit = if report.as_ref().is_some_and(|r| !r.refused.is_empty()) {
                1
            } else {
                0
            };
            out.emit(&json!({"pr": pr, "report": report}), text)?;
            return Ok(exit);
        }
        PrCommand::List => unreachable!(),
    }
    Ok(0)
}

fn auth(command: AuthCommand, out: &mut Output) -> Result<i32> {
    let store = KeychainTokenStore::new("github.com");
    match command {
        AuthCommand::Status => {
            let Some(token) = resolve_token(&store)? else {
                out.emit(
                    &ForgeAuth {
                        source: None,
                        login: None,
                    },
                    "Not connected. Run `gh auth login` or `jiji auth login --with-token`.\n",
                )?;
                return Ok(1);
            };
            let login = GitHubClient::for_github_com(&token.token)?.viewer()?;
            out.emit(
                &ForgeAuth {
                    source: Some(token.source),
                    login: Some(login.clone()),
                },
                format!(
                    "Connected as @{login} via {}\n",
                    match token.source {
                        TokenSource::Keychain => "the system keychain",
                        TokenSource::Environment => "the environment",
                        TokenSource::GhCli => "the gh CLI",
                    }
                ),
            )?;
        }
        AuthCommand::Login { .. } => {
            if io::stdin().is_terminal() {
                return Err(Error::usage("Pass the token on stdin from a pipe or file. Use `gh auth login` for interactive sign-in."));
            }
            let mut token = String::new();
            io::stdin().take(65_537).read_to_string(&mut token)?;
            if token.trim().is_empty() || token.len() > 65_536 {
                return Err(Error::usage("Expected a GitHub token on stdin"));
            }
            let login = GitHubClient::for_github_com(token.trim())?.viewer()?;
            store.set(token.trim())?;
            out.emit(
                &ForgeAuth {
                    source: Some(TokenSource::Keychain),
                    login: Some(login.clone()),
                },
                format!("Connected as @{login}; token stored in the system keychain.\n"),
            )?;
        }
        AuthCommand::Logout => {
            store.delete()?;
            out.emit(&json!({"removed": "keychain"}), "Removed Jiji's keychain token. Environment and gh CLI tokens are managed externally.\n")?;
        }
    }
    Ok(0)
}

fn open_app(path: &Path, out: &mut Output) -> Result<i32> {
    let path = path.canonicalize()?;
    let directory = if path.is_file() {
        path.parent().unwrap()
    } else {
        &path
    };
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .args(["-b", "com.lauf.jiji"])
            .arg(directory)
            .status()?;
        if !status.success() {
            return Err(Error::new(
                "open_failed",
                "Could not open Jiji.app. Install the desktop app first.",
            ));
        }
        if out.json {
            out.emit(&json!({"opened": directory}), "")?;
        }
        Ok(0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (directory, out);
        Err(Error::new(
            "unsupported_platform",
            "Desktop launching is supported on macOS; use `jiji --help` for headless commands.",
        ))
    }
}

/// Prevent overlapping CLI writers. OS locks release automatically on exit,
/// including crashes; the file itself can safely remain after a stopped watch.
struct MutationLock(File);
impl MutationLock {
    fn acquire(repo: &Path) -> Result<Self> {
        let path = repo.join(".jj/jiji/cli.lock");
        Self::file(&path)
    }
    fn file(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        file.try_lock().map_err(|e| {
            Error::new(
                "cli_busy",
                format!(
                    "Another CLI workflow owns {}, or its lock is unavailable: {e}",
                    path.display()
                ),
            )
        })?;
        Ok(Self(file))
    }
}
impl Drop for MutationLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests;
