//! Branch-name derivation for unbookmarked stack heads: publishing a
//! stack whose head carries no bookmark derives the branch name from the
//! work itself, so inventing a bookmark by hand stops being a required
//! step of the publish flow.
//!
//! The rules are Jiji-owned. jjpr — the forge-engine inspiration — was
//! checked for derivation rules to borrow (the local source at v0.23.0,
//! its docs, and its history) and has none: jjpr never invents branch
//! names. It submits only up to the topmost existing bookmark and folds
//! unbookmarked commits into the nearest bookmarked ancestor's PR, so in
//! jjpr's model a bookmark is always hand-made first. What Jiji borrows
//! instead is jjpr's posture on pinning: like the reconcile fingerprints,
//! the rules below are pinned byte-stable by test vectors — a description
//! that derives a name today derives the same name forever, which keeps
//! replans stale-proof and gives the deferred regenerate-on-describe
//! story a stable base.
//!
//! The shape: slug the first line of the segment's bottom change
//! description — the same change that titles the segment's PR — into a
//! lowercase ASCII kebab name, fall back to jj's own `push-<change id>`
//! convention (`jj git push --change`) when nothing survives slugging,
//! and step around names the repo already uses with `-2`, `-3`, …
//! suffixes.

/// Longest slug before collision suffixes. Enough for a readable first
/// line, short enough that GitHub's branch UI does not truncate it.
const MAX_SLUG_LEN: usize = 40;

/// Slug a change description's first line into a branch-safe name:
/// lowercase ASCII letters and digits survive, every other character
/// (whitespace, punctuation, non-ASCII) separates words, words join with
/// single dashes, and the result is capped at [`MAX_SLUG_LEN`] by
/// dropping whole words (a first word longer than the cap is cut at the
/// cap). `None` when nothing survives — an empty description, or one
/// with no ASCII letters or digits at all.
///
/// The output alphabet `[a-z0-9-]` (never leading/trailing `-`) is
/// git-ref-safe by construction: none of `git check-ref-format`'s
/// forbidden patterns can appear.
pub fn slug_description(description: &str) -> Option<String> {
    let first_line = description.lines().next().unwrap_or("");
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in first_line.chars() {
        if ch.is_ascii_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut slug = String::new();
    for word in words {
        if slug.is_empty() {
            slug = word;
            slug.truncate(MAX_SLUG_LEN);
        } else if slug.len() + 1 + word.len() <= MAX_SLUG_LEN {
            slug.push('-');
            slug.push_str(&word);
        } else {
            break;
        }
    }
    (!slug.is_empty()).then_some(slug)
}

/// True when `name` cannot be created next to an existing ref `existing`:
/// the same name, or git's ref-directory rule — an existing `feat/x`
/// blocks creating `feat`, because refs are files and `feat` would have
/// to be both a file and a directory. (The other direction — an existing
/// `feat` blocking `feat/x` — cannot bite here: derived names carry no
/// slash.)
fn conflicts(name: &str, existing: &str) -> bool {
    existing == name
        || existing
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Derive the branch name for a segment with no bookmark: the bottom
/// change's description slugged, `push-<change id>` when nothing slugs,
/// then `-2`, `-3`, … past names the repo already uses. `taken` should
/// carry every name the repo knows — local bookmarks, tracked remote
/// bookmarks, open-PR head branches — so the created bookmark neither
/// shadows an existing ref nor collides on push.
pub fn derive_branch_name<'a>(
    description: &str,
    change_id: &str,
    taken: impl Iterator<Item = &'a str> + Clone,
) -> String {
    let base =
        slug_description(description).unwrap_or_else(|| format!("push-{change_id}"));
    if !taken.clone().any(|existing| conflicts(&base, existing)) {
        return base;
    }
    let mut attempt = 2u32;
    loop {
        let candidate = format!("{base}-{attempt}");
        if !taken.clone().any(|existing| conflicts(&candidate, existing)) {
            return candidate;
        }
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slug rules, pinned byte-stable: these vectors are a contract,
    /// like the reconcile fingerprint bytes. If a change to
    /// `slug_description` moves any of them, that change breaks name
    /// stability for existing users — it needs the deferred
    /// regenerate-on-rename story first, not a quiet edit here.
    #[test]
    fn slug_vectors_are_pinned() {
        let vectors: &[(&str, Option<&str>)] = &[
            ("Add parser for config files", Some("add-parser-for-config-files")),
            // Only the first line ever contributes.
            ("Add parser\n\nLong body with details", Some("add-parser")),
            // Punctuation separates; apostrophes split words (don't → don-t).
            ("Fix: don't crash on empty input!!", Some("fix-don-t-crash-on-empty-input")),
            ("feat(scope): add the thing", Some("feat-scope-add-the-thing")),
            ("  spaces   collapse  ", Some("spaces-collapse")),
            ("MixedCase CamelCase lowers", Some("mixedcase-camelcase-lowers")),
            ("snake_case_stays_wordy", Some("snake-case-stays-wordy")),
            ("1234 digits are fine", Some("1234-digits-are-fine")),
            // Non-ASCII separates like punctuation — no transliteration.
            ("Café menü fix", Some("caf-men-fix")),
            // The 40-char cap drops whole words…
            (
                "this is a very long description line that goes on well past the budget",
                Some("this-is-a-very-long-description-line"),
            ),
            // …except a first word longer than the cap, which is cut at it.
            (
                "supercalifragilisticexpialidociousandthensomemore",
                Some("supercalifragilisticexpialidociousandthe"),
            ),
            ("!!!", None),
            ("", None),
            ("日本語だけ", None),
        ];
        for (input, expected) in vectors {
            assert_eq!(
                slug_description(input).as_deref(),
                *expected,
                "slug vector moved for {input:?}"
            );
        }
    }

    #[test]
    fn cap_boundary_is_exact() {
        // Exactly 40 characters, with the next word dropped.
        let slug = slug_description("aaaaaaaaaa bbbbbbbbbb cccccccccc ddddddd x").unwrap();
        assert_eq!(slug, "aaaaaaaaaa-bbbbbbbbbb-cccccccccc-ddddddd");
        assert_eq!(slug.len(), MAX_SLUG_LEN);
    }

    fn taken(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn free_names_derive_bare() {
        let existing = taken(&["main", "other-work"]);
        assert_eq!(
            derive_branch_name("Add parser", "rzvqnkom", existing.iter().map(String::as_str)),
            "add-parser"
        );
    }

    #[test]
    fn collisions_suffix_upward() {
        let existing = taken(&["add-parser", "add-parser-2"]);
        assert_eq!(
            derive_branch_name("Add parser", "rzvqnkom", existing.iter().map(String::as_str)),
            "add-parser-3"
        );
    }

    #[test]
    fn ref_directory_rule_counts_as_taken() {
        // An existing `add-parser/tweak` means `add-parser` cannot be a ref.
        let existing = taken(&["add-parser/tweak"]);
        assert_eq!(
            derive_branch_name("Add parser", "rzvqnkom", existing.iter().map(String::as_str)),
            "add-parser-2"
        );
    }

    #[test]
    fn empty_slug_falls_back_to_jj_convention() {
        let existing = taken(&[]);
        assert_eq!(
            derive_branch_name("!!!", "rzvqnkom", existing.iter().map(String::as_str)),
            "push-rzvqnkom"
        );
        let crowded = taken(&["push-rzvqnkom"]);
        assert_eq!(
            derive_branch_name("", "rzvqnkom", crowded.iter().map(String::as_str)),
            "push-rzvqnkom-2"
        );
    }
}
