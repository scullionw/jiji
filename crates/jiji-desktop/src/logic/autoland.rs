//! The auto-land job's chip and gating models: how a job status reads in
//! the shell's activity chip, and when a derived land plan may be queued.
//! The job itself is `jiji-forge`'s; this only phrases it — including the
//! record that survived a restart, which arrives `live: false` and reads
//! as interrupted until resumed or dismissed.

use jiji_core::snapshot::BookmarkState;
use jiji_forge::{AutoLandPhase, AutoLandStatus, LandPlan};

use super::inspect::Tone;

/// A non-terminal record no thread is driving: the job was watching or
/// mid-round when the app closed (or its thread died). Resumable.
pub fn is_interrupted(status: &AutoLandStatus) -> bool {
    !status.live && !status.record.state.phase.is_terminal()
}

/// The open repo's job records in render order: the live job first, then
/// interrupted records (they want a decision), then terminal ones. A job
/// record belongs to the repo it was queued in — it renders only while
/// that repo is open.
pub fn visible_jobs<'a>(
    jobs: &'a [AutoLandStatus],
    repo_path: Option<&str>,
) -> Vec<&'a AutoLandStatus> {
    let Some(repo_path) = repo_path else {
        return Vec::new();
    };
    let rank = |status: &AutoLandStatus| {
        if status.live {
            0
        } else if is_interrupted(status) {
            1
        } else {
            2
        }
    };
    let mut visible: Vec<&AutoLandStatus> = jobs
        .iter()
        .filter(|status| status.record.repo_path == repo_path)
        .collect();
    // Stable: ties keep the host's record order.
    visible.sort_by_key(|status| rank(status));
    visible
}

/// The one record a thread is driving right now, whichever repo it
/// belongs to — queueing is gated on it repo-wide.
pub fn live_job(jobs: &[AutoLandStatus]) -> Option<&AutoLandStatus> {
    jobs.iter().find(|status| status.live)
}

/// Why an interrupted job cannot resume — the head bookmark is gone from
/// the snapshot — or `None` when it can.
pub fn resume_blocker(status: &AutoLandStatus, bookmarks: &[BookmarkState]) -> Option<String> {
    let name = &status.record.state.head_bookmark;
    if bookmarks.iter().any(|b| &b.name == name && b.is_local) {
        None
    } else {
        Some(format!(
            "\u{201c}{name}\u{201d} is no longer a bookmark here — the stack moved or was \
             cleaned up while the job was away"
        ))
    }
}

/// Whether a derived plan may be queued for auto-land: there is either
/// work to run right now, or only conditions waiting clears by itself.
/// Needs-user blockers refuse, the same way Land does.
pub fn can_queue(plan: &LandPlan) -> bool {
    !plan.actions.is_empty() || plan.blockers.iter().all(|blocker| blocker.wait)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chip {
    pub label: String,
    pub tone: ChipTone,
    /// The chip pulses while the job is actively working or watching.
    pub pulse: bool,
    /// Terminal and interrupted states grow a dismiss affordance.
    pub dismissable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipTone {
    Accent,
    Tone(Tone),
}

/// The status-bar activity chip for a job status. Labels stay short — the
/// tooltip ([`story`]) carries the full story.
pub fn chip(status: &AutoLandStatus) -> Chip {
    let state = &status.record.state;
    let name = &state.head_bookmark;
    let make = |what: &str, tone: ChipTone, pulse: bool, dismissable: bool| Chip {
        label: format!("Auto-land {name}: {what}"),
        tone,
        pulse,
        dismissable,
    };
    if is_interrupted(status) {
        // Warn-toned: the user may believe the job is still watching, and
        // it is not.
        return make("interrupted", ChipTone::Tone(Tone::Warn), false, true);
    }
    match &state.phase {
        AutoLandPhase::Waiting {
            attention: true, ..
        } => make("needs you", ChipTone::Tone(Tone::Warn), false, false),
        AutoLandPhase::Waiting { .. } => make("watching", ChipTone::Accent, true, false),
        AutoLandPhase::Round => make("landing…", ChipTone::Accent, true, false),
        AutoLandPhase::Done => make("landed", ChipTone::Tone(Tone::Ok), false, true),
        AutoLandPhase::Failed { .. } => make("gave up", ChipTone::Tone(Tone::Danger), false, true),
        AutoLandPhase::Stopped => make("stopped", ChipTone::Tone(Tone::Muted), false, true),
    }
}

/// The full story behind the chip, for its tooltip and the job card's
/// phase line.
pub fn story(status: &AutoLandStatus) -> String {
    let state = &status.record.state;
    let numbers = || {
        state
            .merged
            .iter()
            .map(|merged| format!("#{}", merged.number))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let landed = if state.merged.is_empty() {
        String::new()
    } else {
        format!(" Landed so far: {}.", numbers())
    };
    if is_interrupted(status) {
        let doing = if matches!(state.phase, AutoLandPhase::Round) {
            "mid-round"
        } else {
            "watching"
        };
        return format!(
            "This job was interrupted while {doing} — the app closed or the repo was switched \
             away. Resume it from Publish; a fresh check picks up where it left off.{landed}"
        );
    }
    match &state.phase {
        AutoLandPhase::Waiting { reasons, .. } => format!("{}.{landed}", reasons.join("; ")),
        AutoLandPhase::Round => format!("Running a landing round now.{landed}"),
        AutoLandPhase::Done => {
            let rounds = format!(
                "{} round{}",
                state.rounds,
                if state.rounds == 1 { "" } else { "s" }
            );
            if state.merged.is_empty() {
                format!("The whole stack landed in {rounds}.")
            } else {
                format!("The whole stack landed: {} in {rounds}.", numbers())
            }
        }
        AutoLandPhase::Failed { message } => format!("{message}.{landed}"),
        AutoLandPhase::Stopped => format!("Stopped on request.{landed}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_forge::{AutoLandMerged, AutoLandRecord, AutoLandState};

    fn status(repo: &str, head: &str, phase: AutoLandPhase, live: bool) -> AutoLandStatus {
        let mut state = AutoLandState::queued(head);
        state.phase = phase;
        AutoLandStatus {
            record: AutoLandRecord {
                repo_path: repo.into(),
                state,
                saved_at_ms: 0,
            },
            live,
        }
    }

    #[test]
    fn jobs_render_live_first_then_interrupted_then_terminal() {
        let jobs = vec![
            status("/r", "done", AutoLandPhase::Done, false),
            status("/r", "paused", AutoLandPhase::Round, false),
            status("/other", "elsewhere", AutoLandPhase::Round, true),
            status("/r", "live", AutoLandPhase::Round, true),
        ];
        let order: Vec<&str> = visible_jobs(&jobs, Some("/r"))
            .iter()
            .map(|status| status.record.state.head_bookmark.as_str())
            .collect();
        assert_eq!(order, ["live", "paused", "done"]);
        assert!(visible_jobs(&jobs, None).is_empty());
        assert_eq!(
            live_job(&jobs).unwrap().record.state.head_bookmark,
            "elsewhere"
        );
    }

    #[test]
    fn chips_phrase_each_state() {
        let watching = status(
            "/r",
            "feat",
            AutoLandPhase::Waiting {
                attention: false,
                reasons: vec!["Checks running on #4".into()],
            },
            true,
        );
        let chip_ = chip(&watching);
        assert_eq!(chip_.label, "Auto-land feat: watching");
        assert!(chip_.pulse && !chip_.dismissable);
        assert_eq!(story(&watching), "Checks running on #4.");

        // The same record with no thread behind it reads interrupted.
        let mut orphan = watching.clone();
        orphan.live = false;
        assert!(is_interrupted(&orphan));
        let chip_ = chip(&orphan);
        assert_eq!(chip_.tone, ChipTone::Tone(Tone::Warn));
        assert!(chip_.dismissable);

        let mut done = status("/r", "feat", AutoLandPhase::Done, false);
        done.record.state.rounds = 2;
        done.record.state.merged = vec![AutoLandMerged {
            number: 4,
            url: String::new(),
            bookmark: "feat".into(),
        }];
        assert_eq!(story(&done), "The whole stack landed: #4 in 2 rounds.");
        assert!(!is_interrupted(&done));
    }

    // ── Ported from the Tauri app's autoland.test.ts ──

    use jiji_core::snapshot::{BookmarkState, SyncState};
    use jiji_forge::{LandAction, LandBlocker, LandOptions};

    fn waiting(reasons: &[&str], attention: bool) -> AutoLandPhase {
        AutoLandPhase::Waiting {
            attention,
            reasons: reasons.iter().map(|r| (*r).to_owned()).collect(),
        }
    }

    fn profile(phase: AutoLandPhase, live: bool) -> AutoLandStatus {
        status("/tmp/repo", "profile", phase, live)
    }

    fn merged(number: u64, bookmark: &str) -> AutoLandMerged {
        AutoLandMerged {
            number,
            url: format!("https://x/{number}"),
            bookmark: bookmark.into(),
        }
    }

    #[test]
    fn a_saved_bypass_and_method_are_spelled_out() {
        let story = crate::logic::publish::autoland_options_story(&LandOptions {
            method: Some(jiji_forge::MergeMethod::Rebase),
            bypass: true,
        });
        assert!(story.contains("Admin bypass is on") && story.contains("rebase merge method"));
        assert!(
            crate::logic::publish::autoland_options_story(&LandOptions::default())
                .contains("remain enforced")
        );
    }

    #[test]
    fn done_failed_and_stopped_are_over_and_nothing_else_is() {
        assert!(AutoLandPhase::Done.is_terminal());
        assert!(AutoLandPhase::Failed {
            message: "x".into()
        }
        .is_terminal());
        assert!(AutoLandPhase::Stopped.is_terminal());
        assert!(!waiting(&[], false).is_terminal());
        assert!(!AutoLandPhase::Round.is_terminal());
    }

    #[test]
    fn a_record_no_thread_drives_is_interrupted_unless_it_is_over() {
        assert!(is_interrupted(&profile(
            waiting(&["checks running"], false),
            false
        )));
        assert!(is_interrupted(&profile(AutoLandPhase::Round, false)));
        assert!(!is_interrupted(&profile(
            waiting(&["checks running"], false),
            true
        )));
        assert!(!is_interrupted(&profile(AutoLandPhase::Done, false)));
        assert!(!is_interrupted(&profile(
            AutoLandPhase::Failed {
                message: "x".into()
            },
            false
        )));
    }

    #[test]
    fn records_of_one_rank_keep_the_host_s_order() {
        let jobs = [
            status("/tmp/repo", "a", waiting(&[], false), false),
            status("/tmp/repo", "b", waiting(&[], false), false),
        ];
        let order: Vec<&str> = visible_jobs(&jobs, Some("/tmp/repo"))
            .iter()
            .map(|status| status.record.state.head_bookmark.as_str())
            .collect();
        assert_eq!(order, ["a", "b"]);
    }

    #[test]
    fn the_live_job_is_the_one_a_thread_drives_or_none() {
        let interrupted = profile(waiting(&[], false), false);
        let elsewhere = status("/somewhere/else", "far", waiting(&[], false), true);
        assert_eq!(
            live_job(&[interrupted.clone(), elsewhere.clone()]),
            Some(&elsewhere)
        );
        assert_eq!(live_job(&[interrupted]), None);
        assert_eq!(live_job(&[]), None);
    }

    fn bookmark(name: &str, is_local: bool) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: "abc".into(),
            remote: (!is_local).then(|| "origin".into()),
            sync: SyncState::Synced,
            is_trunk: false,
            is_local,
            tracked: true,
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        }
    }

    #[test]
    fn a_job_resumes_only_while_its_bookmark_is_still_here() {
        let job = profile(waiting(&[], false), false);
        assert_eq!(
            resume_blocker(&job, &[bookmark("profile", true), bookmark("main", true)]),
            None
        );
        for gone in [
            vec![bookmark("main", true)],
            vec![bookmark("profile", false)],
        ] {
            assert!(resume_blocker(&job, &gone)
                .unwrap()
                .contains("no longer a bookmark here"));
        }
    }

    fn plan(actions: Vec<LandAction>, blockers: &[(&str, bool)]) -> LandPlan {
        LandPlan {
            head_bookmark: "profile".into(),
            remote: "origin".into(),
            base_branch: "main".into(),
            options: LandOptions::default(),
            segments: vec![],
            actions,
            blockers: blockers
                .iter()
                .map(|(message, wait)| LandBlocker {
                    message: (*message).into(),
                    wait: *wait,
                    fetch: false,
                })
                .collect(),
            warnings: vec![],
        }
    }

    #[test]
    fn a_plan_queues_unless_the_user_must_act_first() {
        let fetch = LandAction::FetchRemote {
            remote: "origin".into(),
        };
        assert!(can_queue(&plan(vec![fetch], &[])));
        // Waiting out what GitHub clears by itself is the job's point.
        assert!(can_queue(&plan(
            vec![],
            &[("#1's checks are still running", true)]
        )));
        // Nothing to run and nothing in the way: the job supervises.
        assert!(can_queue(&plan(vec![], &[])));
        assert!(!can_queue(&plan(
            vec![],
            &[
                ("#1's checks are still running", true),
                ("changes were requested on #1", false)
            ]
        )));
    }

    #[test]
    fn the_chip_says_when_the_job_needs_you_and_when_it_is_over() {
        let needs = chip(&profile(waiting(&["fix CI"], true), true));
        assert_eq!(needs.label, "Auto-land profile: needs you");
        assert_eq!(needs.tone, ChipTone::Tone(Tone::Warn));
        assert!(!needs.dismissable);
        for (phase, tone) in [
            (AutoLandPhase::Done, Tone::Ok),
            (
                AutoLandPhase::Failed {
                    message: "x".into(),
                },
                Tone::Danger,
            ),
            (AutoLandPhase::Stopped, Tone::Muted),
        ] {
            let over = chip(&profile(phase, true));
            assert_eq!(over.tone, ChipTone::Tone(tone));
            assert!(over.dismissable);
        }
        let round = chip(&profile(AutoLandPhase::Round, true));
        assert!(round.label.contains("landing…") && round.pulse);
        let interrupted = chip(&profile(waiting(&["checks running"], false), false));
        assert_eq!(interrupted.label, "Auto-land profile: interrupted");
        assert!(!interrupted.pulse && interrupted.dismissable);
    }

    #[test]
    fn the_story_tells_what_the_job_waits_on_and_what_has_landed() {
        let mut job = profile(waiting(&["checks running", "review pending"], false), true);
        job.record.state.merged = vec![merged(1, "auth")];
        let told = story(&job);
        assert!(told.contains("checks running; review pending"), "{told}");
        assert!(told.contains("Landed so far: #1"), "{told}");
        assert!(story(&profile(
            AutoLandPhase::Failed {
                message: "GitHub is unreachable".into()
            },
            false
        ))
        .contains("GitHub is unreachable"));
    }

    #[test]
    fn an_interrupted_story_keeps_its_progress() {
        let mut job = profile(waiting(&["checks running"], false), false);
        job.record.state.merged = vec![merged(1, "auth")];
        let told = story(&job);
        assert!(told.contains("interrupted while watching"), "{told}");
        assert!(told.contains("the app closed or the repo was switched away"));
        assert!(told.contains("Landed so far: #1"));
        assert!(story(&profile(AutoLandPhase::Round, false)).contains("mid-round"));
    }
}
