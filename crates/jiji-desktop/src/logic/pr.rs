//! PR badge presentation: how an open pull request reads as a compact
//! badge on graph rows and in the change header.

use jiji_core::snapshot::BookmarkState;
use jiji_forge::{ChecksRollup, PrState, PrSummary, RepoPrState, ReviewDecision};

use super::inspect::Tone;

/// The PR attached to a bookmark, when there is one. Local non-trunk
/// bookmarks only — trunk is what PRs merge into — matched by name against
/// the fork-filtered head-branch map.
pub fn pr_for_bookmark<'a>(
    bookmark: &BookmarkState,
    prs: Option<&'a RepoPrState>,
) -> Option<&'a PrSummary> {
    if !bookmark.is_local || bookmark.is_trunk {
        return None;
    }
    prs?.by_branch.get(&bookmark.name)
}

/// Badge tone, which doubles as its plain state word. Draft is a state of
/// its own here: an open-but-draft PR should read muted, not ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrTone {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrTone {
    pub fn word(self) -> &'static str {
        match self {
            PrTone::Open => "open",
            PrTone::Draft => "draft",
            PrTone::Merged => "merged",
            PrTone::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrGlyph {
    pub glyph: &'static str,
    pub tone: Tone,
    pub label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrBadge {
    /// "#213"
    pub label: String,
    pub tone: PrTone,
    /// Review state, glyph-sized; `None` when the repo requires no review
    /// or the PR is no longer open (the tone already says everything).
    pub review: Option<PrGlyph>,
    /// Same for the CI rollup; `None` when no checks are configured.
    pub checks: Option<PrGlyph>,
    /// The full story in plain language, for tooltips.
    pub title: String,
}

pub fn review_glyph(review: ReviewDecision) -> Option<PrGlyph> {
    match review {
        ReviewDecision::Approved => Some(PrGlyph {
            glyph: "✓",
            tone: Tone::Ok,
            label: "approved",
        }),
        ReviewDecision::ChangesRequested => Some(PrGlyph {
            glyph: "±",
            tone: Tone::Danger,
            label: "changes requested",
        }),
        ReviewDecision::ReviewRequired => Some(PrGlyph {
            glyph: "◦",
            tone: Tone::Warn,
            label: "review required",
        }),
        ReviewDecision::None => None,
    }
}

pub fn checks_glyph(checks: ChecksRollup) -> Option<PrGlyph> {
    match checks {
        ChecksRollup::Passing => Some(PrGlyph {
            glyph: "✓",
            tone: Tone::Ok,
            label: "checks passing",
        }),
        ChecksRollup::Failing => Some(PrGlyph {
            glyph: "×",
            tone: Tone::Danger,
            label: "checks failing",
        }),
        ChecksRollup::Pending => Some(PrGlyph {
            glyph: "…",
            tone: Tone::Warn,
            label: "checks running",
        }),
        ChecksRollup::None => None,
    }
}

pub fn pr_badge(pr: &PrSummary) -> PrBadge {
    let tone = match pr.state {
        PrState::Open if pr.is_draft => PrTone::Draft,
        PrState::Open => PrTone::Open,
        PrState::Merged => PrTone::Merged,
        PrState::Closed => PrTone::Closed,
    };
    // Review/CI cues matter while the PR can still land; once merged or
    // closed the tone is the whole story.
    let active = pr.state == PrState::Open;
    let review = active.then(|| review_glyph(pr.review)).flatten();
    let checks = active.then(|| checks_glyph(pr.checks)).flatten();
    let facts: Vec<&str> = [
        Some(tone.word()),
        review.map(|glyph| glyph.label),
        checks.map(|glyph| glyph.label),
    ]
    .into_iter()
    .flatten()
    .collect();
    PrBadge {
        label: format!("#{}", pr.number),
        tone,
        review,
        checks,
        title: format!(
            "#{} \u{201c}{}\u{201d} — {} · into {}",
            pr.number,
            pr.title,
            facts.join(" · "),
            pr.base_branch
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(state: PrState, draft: bool) -> PrSummary {
        PrSummary {
            number: 213,
            title: "Lane colors".into(),
            url: String::new(),
            state,
            is_draft: draft,
            head_branch: "feat".into(),
            head_commit: String::new(),
            head_owner: None,
            base_branch: "main".into(),
            body: None,
            review: ReviewDecision::Approved,
            checks: ChecksRollup::Failing,
            merge_commit: None,
            stack: None,
            author: None,
        }
    }

    #[test]
    fn open_prs_carry_review_and_ci_cues() {
        let badge = pr_badge(&pr(PrState::Open, false));
        assert_eq!(badge.label, "#213");
        assert_eq!(badge.tone, PrTone::Open);
        assert_eq!(badge.review.unwrap().glyph, "✓");
        assert_eq!(badge.checks.unwrap().tone, Tone::Danger);
        assert_eq!(
            badge.title,
            "#213 \u{201c}Lane colors\u{201d} — open · approved · checks failing · into main"
        );
    }

    #[test]
    fn drafts_read_muted_and_finished_prs_drop_the_cues() {
        assert_eq!(pr_badge(&pr(PrState::Open, true)).tone, PrTone::Draft);
        let merged = pr_badge(&pr(PrState::Merged, false));
        assert_eq!(merged.tone, PrTone::Merged);
        assert!(merged.review.is_none() && merged.checks.is_none());
    }

    // ── Ported from the Tauri app's pr.test.ts ──

    fn watcher(edit: impl FnOnce(&mut PrSummary)) -> PrSummary {
        let mut pr = PrSummary {
            number: 213,
            title: "Add the watcher".into(),
            url: "https://github.com/o/r/pull/213".into(),
            state: PrState::Open,
            is_draft: false,
            head_branch: "watcher-fix".into(),
            head_commit: "a".repeat(40),
            head_owner: Some("o".into()),
            base_branch: "main".into(),
            body: None,
            review: ReviewDecision::None,
            checks: ChecksRollup::None,
            merge_commit: None,
            stack: None,
            author: None,
        };
        edit(&mut pr);
        pr
    }

    fn local(name: &str, is_local: bool, is_trunk: bool) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: "x".into(),
            remote: None,
            sync: jiji_core::snapshot::SyncState::LocalOnly,
            is_trunk,
            is_local,
            tracked: false,
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        }
    }

    #[test]
    fn a_local_bookmark_wears_the_pull_request_its_name_heads() {
        let prs = RepoPrState::new(
            jiji_forge::PrStateReport {
                prs: vec![watcher(|_| {})],
                truncated: false,
            },
            "o",
        );
        let found =
            |bookmark: &BookmarkState| pr_for_bookmark(bookmark, Some(&prs)).map(|pr| pr.number);
        assert_eq!(found(&local("watcher-fix", true, false)), Some(213));
        // Not trunk, not a remote-only entry, not another name.
        assert_eq!(found(&local("watcher-fix", true, true)), None);
        assert_eq!(found(&local("watcher-fix", false, false)), None);
        assert_eq!(found(&local("other", true, false)), None);
        // Nothing before the PR state arrives.
        assert!(pr_for_bookmark(&local("watcher-fix", true, false), None).is_none());
    }

    #[test]
    fn a_plain_open_pr_has_no_cues_to_show() {
        let badge = pr_badge(&watcher(|_| {}));
        assert_eq!(badge.label, "#213");
        assert_eq!(badge.tone, PrTone::Open);
        assert!(badge.review.is_none() && badge.checks.is_none());
        assert_eq!(
            badge.title,
            "#213 \u{201c}Add the watcher\u{201d} — open · into main"
        );
    }

    #[test]
    fn an_open_pr_carries_its_review_and_checks() {
        let ok = pr_badge(&watcher(|pr| {
            pr.review = ReviewDecision::Approved;
            pr.checks = ChecksRollup::Passing;
        }));
        assert_eq!(
            (ok.review.unwrap().glyph, ok.review.unwrap().tone),
            ("✓", Tone::Ok)
        );
        assert_eq!(
            (ok.checks.unwrap().glyph, ok.checks.unwrap().tone),
            ("✓", Tone::Ok)
        );
        assert!(ok.title.contains("approved · checks passing"));
        let blocked = pr_badge(&watcher(|pr| {
            pr.review = ReviewDecision::ChangesRequested;
            pr.checks = ChecksRollup::Failing;
        }));
        assert_eq!(blocked.review.unwrap().glyph, "±");
        assert_eq!(blocked.checks.unwrap().glyph, "×");
        assert_eq!(blocked.review.unwrap().tone, Tone::Danger);
        let waiting = pr_badge(&watcher(|pr| {
            pr.review = ReviewDecision::ReviewRequired;
            pr.checks = ChecksRollup::Pending;
        }));
        assert_eq!(waiting.review.unwrap().tone, Tone::Warn);
        assert_eq!(waiting.checks.unwrap().tone, Tone::Warn);
    }

    #[test]
    fn a_draft_is_its_own_tone_and_keeps_its_live_cues() {
        let draft = pr_badge(&watcher(|pr| {
            pr.is_draft = true;
            pr.checks = ChecksRollup::Pending;
        }));
        assert_eq!(draft.tone, PrTone::Draft);
        assert!(draft.checks.is_some());
        assert!(draft.title.contains("draft · checks running"));
        // Closed, a draft reads closed.
        let closed = pr_badge(&watcher(|pr| {
            pr.state = PrState::Closed;
            pr.is_draft = true;
        }));
        assert_eq!(closed.tone, PrTone::Closed);
    }
}
