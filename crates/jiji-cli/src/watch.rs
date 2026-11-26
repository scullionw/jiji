use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jiji_forge::*;
use serde_json::json;

use crate::{
    args::WatchArgs, output, output::Output, planning_snapshot, require_bookmark, resolve_head,
    vcs::LocalVcs, Error, ForgeContext, MutationLock, Result,
};

pub fn default_state_file(repo: &Path) -> PathBuf {
    repo.join(".jj/jiji/cli-autoland.json")
}

/// Unlike app startup, an explicit CLI resume must report corrupt records.
/// Silently replacing them would lose the progress the user asked to resume.
pub fn read_records(path: &Path) -> Result<Vec<AutoLandRecord>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(err) => return Err(err.into()),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    match value["version"].as_u64() {
        Some(2) => serde_json::from_value(value["records"].clone()).map_err(Into::into),
        Some(1) => Ok(vec![serde_json::from_value(value)?]),
        _ => Err(Error::new(
            "invalid_state_file",
            format!("Unsupported auto-land record format in {}", path.display()),
        )),
    }
}

fn initial_state(
    records: &[AutoLandRecord],
    repo: &str,
    head: &str,
    args: &WatchArgs,
) -> Result<AutoLandState> {
    let mut state = if args.resume {
        let record = records
            .iter()
            .find(|r| r.is_for(repo, head))
            .ok_or_else(|| {
                Error::usage(format!(
                    "No saved watch for {head}; omit --resume to start one"
                ))
            })?;
        if matches!(record.state.phase, AutoLandPhase::Done) {
            return Err(Error::usage(
                "This saved stack already landed; omit --resume to start a new watch",
            ));
        }
        AutoLandState::resumed(record.state.clone())
    } else {
        AutoLandState::queued(head)
    };
    state.options = args.land.rules.apply(state.options);
    Ok(state)
}

pub fn run(vcs: &LocalVcs, args: WatchArgs, out: &mut Output) -> Result<i32> {
    let _lock = if args.land.write.dry_run {
        None
    } else {
        Some(MutationLock::acquire(&vcs.path)?)
    };
    let snapshot = planning_snapshot(vcs, args.land.write.dry_run)?;
    let forge = ForgeContext::connect(&snapshot)?;
    let head = require_bookmark(resolve_head(
        &snapshot,
        &forge.prs,
        args.land.write.head.as_deref(),
    )?)?;
    let path = args
        .state_file
        .clone()
        .unwrap_or_else(|| default_state_file(&vcs.path));
    if path.is_dir() {
        return Err(Error::usage(
            "--state-file must name a file, not a directory",
        ));
    }
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    let _records_lock = if args.land.write.dry_run {
        None
    } else {
        Some(MutationLock::file(Path::new(&lock_path))?)
    };
    let mut records = read_records(&path)?;
    let initial = initial_state(&records, &snapshot.repo_path, &head, &args)?;
    // Preview uses the saved options too, with no record or lock file writes.
    if args.land.write.dry_run {
        let plan = forge.land_plan(&snapshot, &head, vcs, &initial.options)?;
        let exit = if plan.blockers.iter().any(|b| !b.wait) {
            3
        } else {
            0
        };
        out.emit(
            &json!({"plan": plan, "stateFile": path, "resuming": args.resume}),
            output::land_plan(&plan),
        )?;
        return Ok(exit);
    }
    let stop = Arc::new(StopSignal::new());
    #[cfg(unix)]
    let signal_handle = {
        use signal_hook::{
            consts::{SIGINT, SIGTERM},
            iterator::Signals,
        };
        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let handle = signals.handle();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            if signals.forever().next().is_some() {
                stop.stop();
            }
        });
        handle
    };
    // EOF in a pipeline is not a stop request. Enter is a convenience for
    // interactive terminals; Ctrl-C/SIGTERM also interrupt the engine's wait.
    if io::stdin().is_terminal() {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut line = String::new();
            if io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
                stop.stop();
            }
        });
    }
    let mut failure = None;
    let mut publish = |state: &AutoLandState| {
        upsert_autoland_record(
            &mut records,
            AutoLandRecord {
                repo_path: snapshot.repo_path.clone(),
                state: state.clone(),
                saved_at_ms: unix_now_ms(),
            },
        );
        let result = save_autoland_records(&path, &records)
            .map_err(Error::from)
            .and_then(|_| out.event(state));
        if let Err(error) = result {
            if failure.is_none() {
                failure = Some(error);
            }
            stop.stop();
        }
    };
    // Fail before the engine starts if persistence cannot save the job.
    publish(&initial);
    let side = forge.land_side();
    let final_state = run_autoland(
        &forge.repo,
        initial,
        vcs,
        &side,
        &side,
        &AutoLandConfig::default(),
        &stop,
        &mut publish,
    );
    #[cfg(unix)]
    signal_handle.close();
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(match final_state.phase {
        AutoLandPhase::Done => 0,
        AutoLandPhase::Stopped => 130,
        _ => 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{Cli, Command};
    use clap::Parser;

    fn args(extra: &[&str]) -> WatchArgs {
        let parsed = Cli::try_parse_from(
            ["jiji", "watch", "topic"]
                .into_iter()
                .chain(extra.iter().copied()),
        )
        .unwrap();
        let Some(Command::Watch(args)) = parsed.command else {
            panic!("watch")
        };
        args
    }

    #[test]
    fn stopped_watch_resumes_progress_and_saved_rules() {
        let mut state = AutoLandState::queued("topic");
        state.phase = AutoLandPhase::Stopped;
        state.rounds = 3;
        state.options = LandOptions {
            method: Some(MergeMethod::Merge),
            bypass: true,
        };
        let record = AutoLandRecord {
            repo_path: "/repo".into(),
            state,
            saved_at_ms: 10,
        };
        let resumed =
            initial_state(&[record.clone()], "/repo", "topic", &args(&["--resume"])).unwrap();
        assert_eq!(resumed.rounds, 3);
        assert_eq!(resumed.options, record.state.options);
        assert!(matches!(resumed.phase, AutoLandPhase::Waiting { .. }));
        let resumed = initial_state(
            &[record],
            "/repo",
            "topic",
            &args(&["--resume", "--no-bypass", "--method", "auto"]),
        )
        .unwrap();
        assert_eq!(resumed.options, LandOptions::default());
    }

    #[test]
    fn resume_never_substitutes_a_different_repo_or_head() {
        let record = AutoLandRecord {
            repo_path: "/other".into(),
            state: AutoLandState::queued("topic"),
            saved_at_ms: 0,
        };
        assert!(initial_state(&[record], "/repo", "topic", &args(&["--resume"])).is_err());
        assert!(initial_state(&[], "/repo", "topic", &args(&["--resume"])).is_err());
    }

    #[test]
    fn invalid_records_are_reported_and_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        assert!(read_records(&path).unwrap().is_empty());
        for text in [
            "broken",
            "{\"version\":99,\"records\":[]}",
            "{\"version\":2,\"records\":[{}]}",
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(read_records(&path).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[test]
    fn records_round_trip_without_losing_other_stacks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let records: Vec<_> = ["a", "b"]
            .map(|head| AutoLandRecord {
                repo_path: "/repo".into(),
                state: AutoLandState::queued(head),
                saved_at_ms: 10,
            })
            .to_vec();
        save_autoland_records(&path, &records).unwrap();
        assert_eq!(read_records(&path).unwrap(), records);
    }
}
