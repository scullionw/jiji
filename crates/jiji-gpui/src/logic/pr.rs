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
}
