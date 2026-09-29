//! Intraline diff refinement: pair the lines of a changed block by
//! content similarity, then word-diff each pair in isolation.
//!
//! jj-lib's unified hunks refine a changed region by word-diffing the
//! whole removed run against the whole added run, so a token in a
//! removed line can "match" a token three lines away inside an added
//! comment. What's left emphasized is meaningless fragments — a lone
//! `.`, half an identifier. Here the unit of refinement is the line
//! pair: only genuinely similar lines pair up, only pairs carry
//! intraline emphasis, and the pairing doubles as the split layout's
//! row alignment.

use std::ops::Range;

use crate::snapshot::{DiffLine, DiffLineKind, DiffSegment};

/// Lines pair when their length-weighted token overlap reaches this;
/// below it, emphasis would mark most of both lines — flat reads better.
const SIM_THRESHOLD: f64 = 0.3;
/// Beyond this many lines on a side, pairing falls back from optimal
/// alignment to positional matching (still similarity-gated).
const DP_LINES_CAP: usize = 300;
/// Beyond this token-count product, a pair renders flat rather than
/// paying a quadratic word diff (minified single-line files).
const WORD_DIFF_CAP: usize = 250_000;
/// An unchanged run this short (or whitespace-only) between two changed
/// runs reads as speckle; absorb it into the emphasis.
const ABSORB_MAX_BYTES: usize = 2;
/// A pair more changed than this after absorption renders flat: the
/// lines stay aligned, but emphasis that covers everything says nothing.
const DEMOTE_CHANGED_FRACTION: f64 = 0.8;

/// Rebuild a hunk's lines with paired intraline segments: walk the
/// changed blocks (maximal non-context runs), align their removed and
/// added lines by similarity, word-diff each pair, and leave everything
/// else as one flat segment. `pair` holds the counterpart's index into
/// the same slice.
pub fn refine_lines(lines: &[(DiffLineKind, String)]) -> Vec<DiffLine> {
    let mut out: Vec<DiffLine> = lines
        .iter()
        .map(|(kind, text)| DiffLine {
            kind: *kind,
            segments: if text.is_empty() {
                Vec::new()
            } else {
                vec![DiffSegment {
                    text: text.clone(),
                    changed: *kind != DiffLineKind::Context,
                }]
            },
            pair: None,
        })
        .collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].0 == DiffLineKind::Context {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].0 != DiffLineKind::Context {
            i += 1;
        }
        refine_block(lines, &mut out, start..i);
    }
    out
}

fn refine_block(lines: &[(DiffLineKind, String)], out: &mut [DiffLine], block: Range<usize>) {
    let removed_idx: Vec<usize> = block
        .clone()
        .filter(|&k| lines[k].0 == DiffLineKind::Removed)
        .collect();
    let added_idx: Vec<usize> = block
        .filter(|&k| lines[k].0 == DiffLineKind::Added)
        .collect();
    let removed: Vec<&str> = removed_idx.iter().map(|&k| lines[k].1.as_str()).collect();
    let added: Vec<&str> = added_idx.iter().map(|&k| lines[k].1.as_str()).collect();
    for (r, a) in pair_lines(&removed, &added) {
        let (ri, ai) = (removed_idx[r], added_idx[a]);
        out[ri].pair = Some(ai as u32);
        out[ai].pair = Some(ri as u32);
        if let Some((old_segs, new_segs)) = pair_segments(removed[r], added[a]) {
            out[ri].segments = old_segs;
            out[ai].segments = new_segs;
        }
    }
}

/// Split a line into word runs (alphanumeric + `_`), whitespace runs,
/// and single other characters.
fn tokenize(text: &str) -> Vec<&str> {
    fn class(c: char) -> u8 {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    }
    let mut tokens = Vec::new();
    let mut iter = text.char_indices().peekable();
    while let Some((start, c)) = iter.next() {
        let cl = class(c);
        let mut end = start + c.len_utf8();
        if cl != 2 {
            while let Some(&(j, d)) = iter.peek() {
                if class(d) != cl {
                    break;
                }
                end = j + d.len_utf8();
                iter.next();
            }
        }
        tokens.push(&text[start..end]);
    }
    tokens
}

/// One line's non-whitespace tokens, sorted for multiset intersection,
/// plus their summed byte length.
struct LineTokens<'a> {
    words: Vec<&'a str>,
    weight: usize,
}

fn line_tokens(text: &str) -> LineTokens<'_> {
    let mut words: Vec<&str> = tokenize(text)
        .into_iter()
        .filter(|t| !t.starts_with(char::is_whitespace))
        .collect();
    words.sort_unstable();
    let weight = words.iter().map(|t| t.len()).sum();
    LineTokens { words, weight }
}

/// Length-weighted token overlap: 2·common/(a+b) over summed byte
/// lengths, so shared identifiers dominate shared punctuation.
/// Whitespace is excluded — deep indentation must not pair unrelated
/// lines.
fn similarity(a: &LineTokens, b: &LineTokens) -> f64 {
    if a.weight == 0 || b.weight == 0 {
        return 0.0;
    }
    let (mut i, mut j, mut common) = (0, 0, 0usize);
    while i < a.words.len() && j < b.words.len() {
        match a.words[i].cmp(b.words[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += a.words[i].len();
                i += 1;
                j += 1;
            }
        }
    }
    2.0 * common as f64 / (a.weight + b.weight) as f64
}

/// Order-preserving pairing of a block's removed and added lines,
/// maximizing total similarity over pairs at or above the threshold.
fn pair_lines(removed: &[&str], added: &[&str]) -> Vec<(usize, usize)> {
    if removed.is_empty() || added.is_empty() {
        return Vec::new();
    }
    let rt: Vec<LineTokens> = removed.iter().map(|t| line_tokens(t)).collect();
    let at: Vec<LineTokens> = added.iter().map(|t| line_tokens(t)).collect();
    if removed.len() > DP_LINES_CAP || added.len() > DP_LINES_CAP {
        return (0..removed.len().min(added.len()))
            .filter(|&k| similarity(&rt[k], &at[k]) >= SIM_THRESHOLD)
            .map(|k| (k, k))
            .collect();
    }
    let (n, m) = (removed.len(), added.len());
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    let mut score = vec![0f64; (n + 1) * (m + 1)];
    // 1 = pair (diagonal), 2 = skip removed (up), 3 = skip added (left).
    let mut from = vec![0u8; (n + 1) * (m + 1)];
    for i in 1..=n {
        for j in 1..=m {
            let (mut best, mut f) = (score[idx(i - 1, j)], 2u8);
            if score[idx(i, j - 1)] > best {
                best = score[idx(i, j - 1)];
                f = 3;
            }
            let s = similarity(&rt[i - 1], &at[j - 1]);
            if s >= SIM_THRESHOLD && score[idx(i - 1, j - 1)] + s >= best {
                best = score[idx(i - 1, j - 1)] + s;
                f = 1;
            }
            score[idx(i, j)] = best;
            from[idx(i, j)] = f;
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        match from[idx(i, j)] {
            1 => {
                pairs.push((i - 1, j - 1));
                i -= 1;
                j -= 1;
            }
            2 => i -= 1,
            _ => j -= 1,
        }
    }
    pairs.reverse();
    pairs
}

/// Word-diff one pair into per-side segments. None when emphasis would
/// not help — the pair is mostly rewrite, or too large to word-diff —
/// in which case the caller keeps the alignment but renders both sides
/// flat.
fn pair_segments(old: &str, new: &str) -> Option<(Vec<DiffSegment>, Vec<DiffSegment>)> {
    let a = tokenize(old);
    let b = tokenize(new);
    if a.is_empty() || b.is_empty() || a.len() * b.len() > WORD_DIFF_CAP {
        return None;
    }
    // Longest common subsequence over tokens, maximizing matched byte
    // weight so long identifiers win over stray punctuation matches.
    let (n, m) = (a.len(), b.len());
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    let mut score = vec![0u32; (n + 1) * (m + 1)];
    let mut from = vec![0u8; (n + 1) * (m + 1)];
    for i in 1..=n {
        for j in 1..=m {
            let (mut best, mut f) = (score[idx(i - 1, j)], 2u8);
            if score[idx(i, j - 1)] > best {
                best = score[idx(i, j - 1)];
                f = 3;
            }
            if a[i - 1] == b[j - 1] {
                let s = score[idx(i - 1, j - 1)] + a[i - 1].len() as u32;
                if s >= best {
                    best = s;
                    f = 1;
                }
            }
            score[idx(i, j)] = best;
            from[idx(i, j)] = f;
        }
    }
    let mut a_changed = vec![true; n];
    let mut b_changed = vec![true; m];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        match from[idx(i, j)] {
            1 => {
                a_changed[i - 1] = false;
                b_changed[j - 1] = false;
                i -= 1;
                j -= 1;
            }
            2 => i -= 1,
            _ => j -= 1,
        }
    }
    absorb(&a, &mut a_changed);
    absorb(&b, &mut b_changed);
    let changed_bytes = |tokens: &[&str], changed: &[bool]| {
        tokens
            .iter()
            .zip(changed)
            .filter(|(_, &c)| c)
            .map(|(t, _)| t.len())
            .sum::<usize>()
    };
    let changed = changed_bytes(&a, &a_changed) + changed_bytes(&b, &b_changed);
    if changed as f64 / (old.len() + new.len()) as f64 > DEMOTE_CHANGED_FRACTION {
        return None;
    }
    Some((build_segments(&a, &a_changed), build_segments(&b, &b_changed)))
}

/// Mark short or whitespace-only unchanged runs strictly between two
/// changed runs as changed, so emphasis reads as phrases instead of
/// speckle. Runs touching either end of the line always survive.
fn absorb(tokens: &[&str], changed: &mut [bool]) {
    let mut i = 0;
    let mut seen_changed = false;
    while i < tokens.len() {
        if changed[i] {
            seen_changed = true;
            i += 1;
            continue;
        }
        let start = i;
        while i < tokens.len() && !changed[i] {
            i += 1;
        }
        if !seen_changed || i == tokens.len() {
            continue;
        }
        let run = &tokens[start..i];
        let bytes: usize = run.iter().map(|t| t.len()).sum();
        let all_ws = run.iter().all(|t| t.starts_with(char::is_whitespace));
        if bytes <= ABSORB_MAX_BYTES || all_ws {
            for c in &mut changed[start..i] {
                *c = true;
            }
        }
    }
}

fn build_segments(tokens: &[&str], changed: &[bool]) -> Vec<DiffSegment> {
    let mut out: Vec<DiffSegment> = Vec::new();
    for (token, &c) in tokens.iter().zip(changed) {
        match out.last_mut() {
            Some(last) if last.changed == c => last.text.push_str(token),
            _ => out.push(DiffSegment {
                text: (*token).to_string(),
                changed: c,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn removed(text: &str) -> (DiffLineKind, String) {
        (DiffLineKind::Removed, text.to_string())
    }
    fn added(text: &str) -> (DiffLineKind, String) {
        (DiffLineKind::Added, text.to_string())
    }
    fn context(text: &str) -> (DiffLineKind, String) {
        (DiffLineKind::Context, text.to_string())
    }

    fn text_of(line: &DiffLine) -> String {
        line.segments.iter().map(|s| s.text.as_str()).collect()
    }
    fn changed_texts(line: &DiffLine) -> Vec<&str> {
        line.segments
            .iter()
            .filter(|s| s.changed)
            .map(|s| s.text.as_str())
            .collect()
    }
    fn is_flat(line: &DiffLine) -> bool {
        line.segments.len() == 1 && line.segments[0].changed
    }

    #[test]
    fn pairs_the_similar_line_not_the_positional_one() {
        // Four removed lines, one added; only the third removed line
        // resembles it. Cross-line word matching used to sprinkle
        // emphasis over all four.
        let lines = vec![
            removed("// The Rust trait exposes the anchor only for macOS"),
            removed("// so it is declared by hand."),
            removed("unsafe impl PresentationContextProviding for SignInDelegate {}"),
            removed("struct Zzz;"),
            added("unsafe impl PresentationContextProviding for Delegate {}"),
        ];
        let out = refine_lines(&lines);
        assert_eq!(out[2].pair, Some(4));
        assert_eq!(out[4].pair, Some(2));
        // The unrelated lines stay flat and unpaired.
        for k in [0, 1, 3] {
            assert_eq!(out[k].pair, None, "line {k} should not pair");
            assert!(is_flat(&out[k]), "line {k} should be flat");
        }
        // The pair carries real word emphasis: only the renamed type.
        assert_eq!(changed_texts(&out[2]), vec!["SignInDelegate"]);
        assert_eq!(changed_texts(&out[4]), vec!["Delegate"]);
    }

    #[test]
    fn dissimilar_lines_do_not_pair() {
        let lines = vec![
            removed("controller.set_provider(anchor);"),
            added("// a comment about something else entirely"),
        ];
        let out = refine_lines(&lines);
        assert!(out.iter().all(|l| l.pair.is_none()));
        assert!(is_flat(&out[0]));
        assert!(is_flat(&out[1]));
    }

    #[test]
    fn small_gaps_between_changes_are_absorbed() {
        let lines = vec![
            removed("    let total = sum(1, 2);"),
            added("    let total = sum(3, 4);"),
        ];
        let out = refine_lines(&lines);
        assert_eq!(out[0].pair, Some(1));
        // "1, 2" emphasizes as one phrase, not two speckles around an
        // unchanged ", ".
        assert_eq!(changed_texts(&out[0]), vec!["1, 2"]);
        assert_eq!(changed_texts(&out[1]), vec!["3, 4"]);
        assert_eq!(text_of(&out[0]), "    let total = sum(1, 2);");
        assert_eq!(text_of(&out[1]), "    let total = sum(3, 4);");
    }

    #[test]
    fn adjacent_changed_words_emphasize_as_one_phrase() {
        let lines = vec![removed("aa bb shared_tail"), added("xx yy shared_tail")];
        let out = refine_lines(&lines);
        // The unchanged space between two changed words is absorbed, so
        // the phrase reads as one run — while the genuinely shared word
        // keeps its own unchanged run.
        assert_eq!(changed_texts(&out[0]), vec!["aa bb"]);
        assert_eq!(changed_texts(&out[1]), vec!["xx yy"]);
    }

    #[test]
    fn mostly_rewritten_pair_stays_aligned_but_flat() {
        // Enough shared weight to pair, but after the word diff nearly
        // everything is emphasis — flat reads better; alignment stays.
        let lines = vec![removed("aa bb cc dd ee ff gg"), added("gg aa")];
        let out = refine_lines(&lines);
        assert_eq!(out[0].pair, Some(1));
        assert_eq!(out[1].pair, Some(0));
        assert!(is_flat(&out[0]));
        assert!(is_flat(&out[1]));
    }

    #[test]
    fn pairing_is_scoped_to_the_block() {
        // Identical lines in different blocks (separated by context)
        // never pair across the gap.
        let lines = vec![
            removed("let x = compute_value(a, b);"),
            context("fn main() {"),
            added("let x = compute_value(a, c);"),
        ];
        let out = refine_lines(&lines);
        assert!(out.iter().all(|l| l.pair.is_none()));
    }

    #[test]
    fn context_and_empty_lines_keep_current_shape() {
        let lines = vec![context("unchanged"), removed(""), added("new text")];
        let out = refine_lines(&lines);
        assert_eq!(out[0].segments.len(), 1);
        assert!(!out[0].segments[0].changed);
        // The empty removed line has no segments at all.
        assert!(out[1].segments.is_empty());
        assert!(is_flat(&out[2]));
    }

    #[test]
    fn pairs_are_monotonic() {
        let lines = vec![
            removed("fn alpha(a: u32) -> u32 { a }"),
            removed("fn beta(b: u32) -> u32 { b }"),
            added("fn beta(b: u64) -> u64 { b }"),
            added("fn alpha(a: u32) -> u32 { a + 1 }"),
        ];
        let out = refine_lines(&lines);
        // Order-preserving alignment can keep only one of the crossed
        // pairs; it must never emit crossing links.
        let mut links: Vec<(u32, u32)> = out
            .iter()
            .enumerate()
            .filter_map(|(k, l)| l.pair.map(|p| (k as u32, p)))
            .filter(|(k, p)| k < p)
            .collect();
        links.sort();
        for w in links.windows(2) {
            assert!(w[0].1 < w[1].1, "crossing pair links: {links:?}");
        }
    }

    #[test]
    fn indent_change_emphasizes_the_indent() {
        let lines = vec![removed("    if x {"), added("        if x {")];
        let out = refine_lines(&lines);
        assert_eq!(out[0].pair, Some(1));
        let changed = changed_texts(&out[1]);
        assert!(
            changed.iter().all(|t| t.chars().all(char::is_whitespace)),
            "only indentation should be emphasized: {changed:?}"
        );
    }

    #[test]
    fn utf8_boundaries_survive_tokenizing() {
        let lines = vec![removed("emoji → “fancy” ✓"), added("emoji → «fancy» ✓")];
        let out = refine_lines(&lines);
        assert_eq!(text_of(&out[0]), "emoji → “fancy” ✓");
        assert_eq!(text_of(&out[1]), "emoji → «fancy» ✓");
    }
}
