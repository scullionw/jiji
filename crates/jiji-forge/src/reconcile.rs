//! PR text reconciliation: updating an existing PR's title and description
//! from the commit without clobbering hand edits.
//!
//! The shape is jjpr's fingerprint machinery (see the jjpr inspiration
//! note): Jiji writes the commit-derived description inside sentinel
//! markers, followed by fingerprints of the text it wrote. On the next
//! submit, a three-way comparison — the commit-derived text ("ours"), the
//! on-GitHub text ("theirs"), and the fingerprint of what Jiji last wrote
//! ("base") — tells "the commit moved, the PR is stale" apart from "the
//! user hand-edited the PR", which are otherwise indistinguishable. Text
//! outside the sentinels always belongs to the user and is preserved
//! verbatim.
//!
//! Jiji extends jjpr in two ways, both documented on [`plan_pr_text`]:
//! titles are fingerprinted too (jjpr only warns on title drift), and a
//! markerless body that is empty or byte-identical to the commit's is
//! adopted — which is what migrates PRs Jiji created before this existed.

/// Opens the Jiji-managed section of a PR description.
pub const DESCRIPTION_START: &str = "<!-- jiji:description -->";
/// Closes the Jiji-managed section.
pub const DESCRIPTION_END: &str = "<!-- /jiji:description -->";
const BODY_FP_PREFIX: &str = "<!-- jiji:body-fp ";
const TITLE_FP_PREFIX: &str = "<!-- jiji:title-fp ";
const FP_SUFFIX: &str = " -->";

/// Stable 64-bit FNV-1a hash, hex-encoded.
///
/// The fingerprint is written into PR bodies and read back by future Jiji
/// versions, so the algorithm must stay byte-for-byte stable forever —
/// which rules out `std`'s `DefaultHasher` (explicitly not portable).
/// FNV-1a is trivial and dependency-free; we only need change detection,
/// not adversarial collision resistance. (jjpr uses the same function, but
/// the markers are Jiji-owned — the two tools ignore each other's.)
pub fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Extract the managed section's text, when the sentinels are present.
pub fn extract_managed_body(pr_body: &str) -> Option<&str> {
    let start = pr_body.find(DESCRIPTION_START)? + DESCRIPTION_START.len();
    let end = pr_body[start..].find(DESCRIPTION_END)? + start;
    Some(pr_body[start..end].trim())
}

fn extract_fp<'a>(pr_body: &'a str, prefix: &str) -> Option<&'a str> {
    let start = pr_body.find(prefix)? + prefix.len();
    let end = pr_body[start..].find(FP_SUFFIX)? + start;
    Some(pr_body[start..end].trim())
}

/// The body fingerprint Jiji last recorded, if any. Absence means the PR
/// predates fingerprinting or the user deleted the marker — "ownership
/// unknown", never a license to overwrite.
pub fn stored_body_fp(pr_body: &str) -> Option<&str> {
    extract_fp(pr_body, BODY_FP_PREFIX)
}

/// The title fingerprint Jiji last recorded, if any.
pub fn stored_title_fp(pr_body: &str) -> Option<&str> {
    extract_fp(pr_body, TITLE_FP_PREFIX)
}

fn fp_block(body_fp: &str, title_fp: Option<&str>) -> String {
    let mut block = format!("{BODY_FP_PREFIX}{body_fp}{FP_SUFFIX}");
    if let Some(title_fp) = title_fp {
        block.push('\n');
        block.push_str(&format!("{TITLE_FP_PREFIX}{title_fp}{FP_SUFFIX}"));
    }
    block
}

/// The initial PR body: the commit-derived text inside sentinels, followed
/// by fingerprints of the body and title Jiji is writing.
pub fn wrap_managed_body(commit_body: &str, title: &str) -> String {
    format!(
        "{DESCRIPTION_START}\n{commit_body}\n{DESCRIPTION_END}\n{}",
        fp_block(&fingerprint(commit_body), Some(&fingerprint(title)))
    )
}

/// Drop the fingerprint marker lines (and the single newlines Jiji writes
/// before them) that immediately follow the closing sentinel, returning
/// the rest of the user's trailing content untouched.
fn strip_leading_fp_markers(after_end: &str) -> &str {
    let mut rest = after_end;
    for prefix in [BODY_FP_PREFIX, TITLE_FP_PREFIX] {
        let candidate = rest.strip_prefix('\n').unwrap_or(rest);
        if candidate.starts_with(prefix) {
            if let Some(rel) = candidate.find(FP_SUFFIX) {
                rest = &candidate[rel + FP_SUFFIX.len()..];
            }
        }
    }
    rest
}

/// Rebuild a PR body around its managed section: the given managed text
/// and fingerprint block replace the old ones, everything the user wrote
/// before and after the sentinels survives verbatim. Bodies without
/// sentinels come back unchanged — callers wrap those with
/// [`wrap_managed_body`] instead.
fn rebuild_body(pr_body: &str, managed: &str, body_fp: &str, title_fp: Option<&str>) -> String {
    let Some(start) = pr_body.find(DESCRIPTION_START) else {
        return pr_body.to_owned();
    };
    let Some(end_rel) = pr_body[start..].find(DESCRIPTION_END) else {
        return pr_body.to_owned();
    };
    let end = start + end_rel + DESCRIPTION_END.len();
    let before = &pr_body[..start];
    let after = strip_leading_fp_markers(&pr_body[end..]);
    format!(
        "{before}{DESCRIPTION_START}\n{managed}\n{DESCRIPTION_END}\n{}{after}",
        fp_block(body_fp, title_fp)
    )
}

/// One text's three-way verdict: the stored fingerprint ("base") against
/// the current on-GitHub text ("theirs") and the commit-derived text
/// ("ours").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextReconcile {
    /// Already matching (fingerprint recorded).
    InSync,
    /// Matching, but no fingerprint recorded yet — record one.
    Backfill,
    /// The commit moved and the PR did not: safe to update.
    Update,
    /// The user edited the PR and the commit did not: respect it silently.
    Leave,
    /// Both moved (or the fingerprint is gone): no safe winner.
    Conflict,
}

fn reconcile_text(stored_fp: Option<&str>, current: &str, expected: &str) -> TextReconcile {
    if current == expected {
        return match stored_fp {
            Some(_) => TextReconcile::InSync,
            None => TextReconcile::Backfill,
        };
    }
    let Some(base) = stored_fp else {
        return TextReconcile::Conflict;
    };
    let pr_edited = fingerprint(current) != base;
    let commit_edited = fingerprint(expected) != base;
    match (commit_edited, pr_edited) {
        (true, false) => TextReconcile::Update,
        (false, true) => TextReconcile::Leave,
        (true, true) => TextReconcile::Conflict,
        // Differing texts with neither side moved from base is impossible;
        // treat it as nothing-to-do rather than guess.
        (false, false) => TextReconcile::InSync,
    }
}

/// Why a PR's text is being left alone, for the plan's warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextWarning {
    /// The managed description and the commit body both changed since Jiji
    /// last wrote the PR (`unfingerprinted`: the sentinels are there but
    /// the fingerprint is gone, so ownership cannot be proven).
    BodyConflict { unfingerprinted: bool },
    /// The title and the commit's first line both changed since Jiji last
    /// wrote the title.
    TitleConflict,
    /// The title differs from the commit and no fingerprint says who wrote
    /// it (a hand-created or pre-tracking PR).
    TitleDrift,
}

/// What a re-submit should do to one PR's text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextPlan {
    /// The title to set, when the commit moved and the PR title did not.
    pub title: Option<String>,
    /// The full replacement body, when the managed section or its
    /// fingerprints should be rewritten.
    pub body: Option<String>,
    /// True when nothing visible changes — the write only records
    /// fingerprints (adopting a pre-tracking PR).
    pub seed: bool,
    pub warnings: Vec<TextWarning>,
}

impl TextPlan {
    pub fn has_write(&self) -> bool {
        self.title.is_some() || self.body.is_some()
    }
}

/// Decide what to do with an existing PR's title and body given the
/// commit-derived expectations. Pure — the caller supplies the on-GitHub
/// text and applies the answer.
///
/// Curated extensions over jjpr, both safe by construction:
/// - Titles reconcile through their own fingerprint instead of only
///   warning on drift — the UI's plan step shows the update before it
///   runs. Without a stored title fingerprint, drift still only warns.
/// - A markerless body is adopted when nothing could be lost: an empty
///   body takes the commit's text outright, and a body byte-identical to
///   the commit's text gains markers (`seed`). Any other markerless body
///   is the user's and is never touched (jjpr's posture).
pub fn plan_pr_text(
    current_title: &str,
    current_body: &str,
    expected_title: &str,
    expected_body: &str,
) -> TextPlan {
    let mut plan = TextPlan::default();

    // The title verdict first: its fingerprint lives in the body's marker
    // block, so the body write below records it.
    let title_fp = stored_title_fp(current_body);
    let title_verdict = reconcile_text(title_fp, current_title, expected_title);
    // What to record as the title fingerprint whenever the body rewrites:
    // claim the title only when Jiji set it (or it matches the commit).
    let record_title_fp = match title_verdict {
        TextReconcile::InSync | TextReconcile::Backfill => Some(fingerprint(expected_title)),
        TextReconcile::Update => Some(fingerprint(expected_title)),
        TextReconcile::Leave => title_fp.map(str::to_owned),
        TextReconcile::Conflict => title_fp.map(str::to_owned),
    };
    let title_updates = title_verdict == TextReconcile::Update && title_fp.is_some();
    if title_updates {
        plan.title = Some(expected_title.to_owned());
    }
    match title_verdict {
        TextReconcile::Conflict if title_fp.is_some() => {
            plan.warnings.push(TextWarning::TitleConflict);
        }
        // No fingerprint and the texts differ: ownership unknown, warn.
        TextReconcile::Conflict => plan.warnings.push(TextWarning::TitleDrift),
        _ => {}
    }

    match extract_managed_body(current_body) {
        Some(managed) => {
            let body_fp = stored_body_fp(current_body);
            match reconcile_text(body_fp, managed, expected_body) {
                TextReconcile::Update => {
                    plan.body = Some(rebuild_body(
                        current_body,
                        expected_body,
                        &fingerprint(expected_body),
                        record_title_fp.as_deref(),
                    ));
                }
                TextReconcile::Backfill => {
                    plan.body = Some(rebuild_body(
                        current_body,
                        expected_body,
                        &fingerprint(expected_body),
                        record_title_fp.as_deref(),
                    ));
                    plan.seed = !title_updates;
                }
                TextReconcile::InSync | TextReconcile::Leave => {
                    // The managed text stays exactly as it is on GitHub; a
                    // rewrite happens only to carry a title change's
                    // fingerprint.
                    if title_updates {
                        let keep_fp = body_fp
                            .map(str::to_owned)
                            .unwrap_or_else(|| fingerprint(managed));
                        plan.body = Some(rebuild_body(
                            current_body,
                            managed,
                            &keep_fp,
                            record_title_fp.as_deref(),
                        ));
                    }
                }
                TextReconcile::Conflict => {
                    plan.warnings.push(TextWarning::BodyConflict {
                        unfingerprinted: body_fp.is_none(),
                    });
                    if title_updates {
                        let keep_fp = body_fp
                            .map(str::to_owned)
                            .unwrap_or_else(|| fingerprint(managed));
                        plan.body = Some(rebuild_body(
                            current_body,
                            managed,
                            &keep_fp,
                            record_title_fp.as_deref(),
                        ));
                    }
                }
            }
        }
        None => {
            let current_trimmed = current_body.trim();
            if current_trimmed.is_empty() {
                // Nothing to clobber: an empty body takes the commit's
                // text outright (and starts tracking).
                if !expected_body.is_empty() {
                    plan.body = Some(wrap_managed_body(expected_body, expected_title));
                    // The wrap claims the title; honor the drift verdict.
                    if title_verdict == TextReconcile::Conflict {
                        plan.body = Some(format!(
                            "{DESCRIPTION_START}\n{expected_body}\n{DESCRIPTION_END}\n{}",
                            fp_block(&fingerprint(expected_body), None)
                        ));
                    }
                }
            } else if current_trimmed == expected_body {
                // Byte-identical to the commit: adopting records that Jiji
                // owns it without changing a visible character.
                plan.body = Some(wrap_managed_body(expected_body, expected_title));
                if title_verdict == TextReconcile::Conflict {
                    plan.body = Some(format!(
                        "{DESCRIPTION_START}\n{expected_body}\n{DESCRIPTION_END}\n{}",
                        fp_block(&fingerprint(expected_body), None)
                    ));
                }
                plan.seed = true;
            }
            // Any other markerless body is the user's; leave silently.
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_pinned_forever() {
        // FNV-1a 64's standard test vector: the algorithm must never
        // change, or every fingerprinted PR reads as conflicted.
        assert_eq!(fingerprint("hello world"), "779a65e7023cd2e7");
        assert_eq!(fingerprint(""), "cbf29ce484222325");
    }

    #[test]
    fn wrap_extract_and_fingerprints_round_trip() {
        let wrapped = wrap_managed_body("The body.\n\nTwo paragraphs.", "feat: thing");
        assert_eq!(
            extract_managed_body(&wrapped),
            Some("The body.\n\nTwo paragraphs.")
        );
        assert_eq!(
            stored_body_fp(&wrapped),
            Some(fingerprint("The body.\n\nTwo paragraphs.").as_str())
        );
        assert_eq!(
            stored_title_fp(&wrapped),
            Some(fingerprint("feat: thing").as_str())
        );
    }

    #[test]
    fn rebuild_preserves_user_text_around_the_section() {
        let body = format!(
            "User intro.\n\n{}\n\nUser outro.",
            wrap_managed_body("old text", "old title")
        );
        let rebuilt = rebuild_body(&body, "new text", &fingerprint("new text"), None);
        assert!(rebuilt.starts_with("User intro.\n\n"));
        assert!(rebuilt.ends_with("\n\nUser outro."));
        assert_eq!(extract_managed_body(&rebuilt), Some("new text"));
        assert_eq!(stored_body_fp(&rebuilt), Some(fingerprint("new text").as_str()));
        // The old title fingerprint line is gone, not duplicated.
        assert_eq!(stored_title_fp(&rebuilt), None);
        assert_eq!(rebuilt.matches(BODY_FP_PREFIX).count(), 1);
    }

    #[test]
    fn commit_moved_updates_and_pr_edited_leaves() {
        let on_github = wrap_managed_body("first draft", "feat: thing");

        // The commit body moved; the PR is untouched → update.
        let plan = plan_pr_text("feat: thing", &on_github, "feat: thing", "second draft");
        assert_eq!(plan.title, None);
        assert!(!plan.seed);
        assert!(plan.warnings.is_empty());
        let new_body = plan.body.expect("body updates");
        assert_eq!(extract_managed_body(&new_body), Some("second draft"));

        // The user edited the PR; the commit is unchanged → leave silently.
        let hand_edited = rebuild_body(
            &on_github,
            "their careful rewrite",
            &fingerprint("first draft"),
            Some(&fingerprint("feat: thing")),
        );
        let plan = plan_pr_text("feat: thing", &hand_edited, "feat: thing", "first draft");
        assert_eq!(plan, TextPlan::default());
    }

    #[test]
    fn both_moved_is_a_conflict_not_a_guess() {
        let on_github = rebuild_body(
            &wrap_managed_body("base text", "t"),
            "their rewrite",
            &fingerprint("base text"),
            Some(&fingerprint("t")),
        );
        let plan = plan_pr_text("t", &on_github, "t", "my rewrite");
        assert_eq!(plan.body, None);
        assert_eq!(
            plan.warnings,
            vec![TextWarning::BodyConflict { unfingerprinted: false }]
        );

        // Markers without a fingerprint and drifted: ownership unknown.
        let stripped = format!("{DESCRIPTION_START}\nsomething\n{DESCRIPTION_END}");
        let plan = plan_pr_text("t", &stripped, "t", "my rewrite");
        assert_eq!(plan.body, None);
        assert_eq!(
            plan.warnings,
            vec![TextWarning::BodyConflict { unfingerprinted: true }]
        );
    }

    #[test]
    fn markers_without_fingerprint_backfill_when_matching() {
        let stripped = format!("{DESCRIPTION_START}\nthe text\n{DESCRIPTION_END}");
        let plan = plan_pr_text("t", &stripped, "t", "the text");
        assert!(plan.seed);
        let body = plan.body.expect("seeding writes");
        assert_eq!(extract_managed_body(&body), Some("the text"));
        assert_eq!(stored_body_fp(&body), Some(fingerprint("the text").as_str()));
    }

    #[test]
    fn markerless_bodies_adopt_only_when_nothing_could_be_lost() {
        // Empty body: take the commit's text outright (not a seed — the
        // description visibly fills in).
        let plan = plan_pr_text("feat: t", "", "feat: t", "The commit body.");
        assert!(!plan.seed);
        let body = plan.body.expect("empty bodies fill in");
        assert_eq!(extract_managed_body(&body), Some("The commit body."));

        // Byte-identical body: adopt quietly.
        let plan = plan_pr_text("feat: t", "The commit body.", "feat: t", "The commit body.");
        assert!(plan.seed);
        assert!(plan.body.is_some());

        // Anything else markerless is the user's: silence, not a warning.
        let plan = plan_pr_text("feat: t", "Hand-written.", "feat: t", "The commit body.");
        assert_eq!(plan, TextPlan::default());

        // Empty on both sides: nothing to manage yet.
        let plan = plan_pr_text("feat: t", "", "feat: t", "");
        assert_eq!(plan, TextPlan::default());
    }

    #[test]
    fn titles_reconcile_through_their_own_fingerprint() {
        let on_github = wrap_managed_body("body", "old title");

        // Commit title moved, PR title untouched → update, body rewrites
        // to carry the new title fingerprint.
        let plan = plan_pr_text("old title", &on_github, "new title", "body");
        assert_eq!(plan.title.as_deref(), Some("new title"));
        assert!(!plan.seed);
        let body = plan.body.expect("fingerprint block rewrites");
        assert_eq!(extract_managed_body(&body), Some("body"));
        assert_eq!(stored_title_fp(&body), Some(fingerprint("new title").as_str()));

        // User retitled, commit unchanged → respected silently.
        let plan = plan_pr_text("their title", &on_github, "old title", "body");
        assert_eq!(plan, TextPlan::default());

        // Both moved → conflict warning, no update.
        let plan = plan_pr_text("their title", &on_github, "new title", "body");
        assert_eq!(plan.title, None);
        assert_eq!(plan.warnings, vec![TextWarning::TitleConflict]);

        // No stored fingerprint and titles differ → drift warning only.
        let plan = plan_pr_text("hand title", "", "commit title", "");
        assert_eq!(plan.title, None);
        assert_eq!(plan.warnings, vec![TextWarning::TitleDrift]);
    }

    #[test]
    fn title_update_preserves_a_hand_edited_body() {
        // The user rewrote the managed body (Leave) while the commit's
        // title moved: the title updates, their body text stays, and the
        // stored body fingerprint survives so the body verdict never
        // flips to "Jiji wrote this".
        let base = wrap_managed_body("base text", "old title");
        let hand_edited = rebuild_body(
            &base,
            "their rewrite",
            &fingerprint("base text"),
            Some(&fingerprint("old title")),
        );
        let plan = plan_pr_text("old title", &hand_edited, "new title", "base text");
        assert_eq!(plan.title.as_deref(), Some("new title"));
        let body = plan.body.expect("rewrites for the title fingerprint");
        assert_eq!(extract_managed_body(&body), Some("their rewrite"));
        assert_eq!(stored_body_fp(&body), Some(fingerprint("base text").as_str()));
        assert_eq!(stored_title_fp(&body), Some(fingerprint("new title").as_str()));
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn adopting_with_a_drifted_title_does_not_claim_it() {
        // Empty body fills in, but the hand-written title stays unclaimed:
        // no title fingerprint is recorded, so later runs keep warning
        // instead of overwriting it.
        let plan = plan_pr_text("hand title", "", "commit title", "The body.");
        let body = plan.body.expect("empty body fills in");
        assert_eq!(stored_title_fp(&body), None);
        assert_eq!(plan.warnings, vec![TextWarning::TitleDrift]);
    }
}
