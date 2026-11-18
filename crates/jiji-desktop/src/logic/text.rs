//! Small wording helpers shared by the models and the views.

/// The first `len` characters of an id: the form shown wherever the full
/// one would not fit.
pub fn short_id(id: &str, len: usize) -> &str {
    match id.char_indices().nth(len) {
        Some((end, _)) => &id[..end],
        None => id,
    }
}

/// The "s" a regular noun takes after this count.
pub fn plural_s(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

/// A count with its noun: "1 change", "3 changes".
pub fn counted(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", plural_s(count))
}

/// One square of a five-block diffstat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    Added,
    Removed,
    Unchanged,
}

/// How a change splits between lines added and removed, in five blocks —
/// the proportion at a glance. A change of fewer than five lines lights
/// only as many blocks as it has lines, and a side that changed at all
/// keeps at least one.
pub fn diffstat_blocks(added: u32, removed: u32) -> [Block; 5] {
    let total = added + removed;
    let lit = total.min(5) as usize;
    let mut adds = if total == 0 {
        0
    } else {
        (added as f32 / total as f32 * lit as f32).round() as usize
    };
    if added > 0 && adds == 0 {
        adds = 1;
    }
    if removed > 0 && adds == lit {
        adds = lit.saturating_sub(1);
    }
    let mut blocks = [Block::Unchanged; 5];
    for (ix, block) in blocks.iter_mut().enumerate().take(lit) {
        *block = if ix < adds {
            Block::Added
        } else {
            Block::Removed
        };
    }
    blocks
}

/// jj names commits and operations by full hashes in the sentences it
/// records ("commit f852890351…"). Runs of twenty or more hex digits keep
/// their first twelve: enough to tell them apart, short enough to read.
pub fn abbreviate_hashes(text: &str) -> String {
    const LONG: usize = 20;
    const KEEP: usize = 12;
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= LONG {
            out.push_str(&run[..KEEP]);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_hexdigit() {
            run.push(ch);
        } else {
            flush(&mut run, &mut out);
            out.push(ch);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// `user@host` as a person reads it: the machine's local-network suffix
/// says nothing.
pub fn short_user_host(text: &str) -> &str {
    text.strip_suffix(".localdomain")
        .or_else(|| text.strip_suffix(".local"))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffstats_keep_proportion_and_both_sides() {
        use Block::*;
        assert_eq!(diffstat_blocks(0, 0), [Unchanged; 5]);
        assert_eq!(
            diffstat_blocks(2, 0),
            [Added, Added, Unchanged, Unchanged, Unchanged]
        );
        assert_eq!(
            diffstat_blocks(1605, 1001),
            [Added, Added, Added, Removed, Removed]
        );
        // A side that changed keeps a block, however lopsided the split.
        assert_eq!(
            diffstat_blocks(1000, 1),
            [Added, Added, Added, Added, Removed]
        );
        assert_eq!(
            diffstat_blocks(1, 1000),
            [Added, Removed, Removed, Removed, Removed]
        );
    }

    #[test]
    fn long_hashes_shorten_and_short_words_stay() {
        assert_eq!(
            abbreviate_hashes("commit f8528903512bdfaab5d9b4c0e4a4876c03f353c5"),
            "commit f8528903512b"
        );
        assert_eq!(
            abbreviate_hashes("restore into commit be43cf77d28a3bd8a0683271048449fd1972b335 (x)"),
            "restore into commit be43cf77d28a (x)"
        );
        assert_eq!(abbreviate_hashes("add deadbeef cafe"), "add deadbeef cafe");
        assert_eq!(abbreviate_hashes(""), "");
    }

    #[test]
    fn hosts_lose_their_local_suffix() {
        assert_eq!(short_user_host("me@box.localdomain"), "me@box");
        assert_eq!(short_user_host("me@box.local"), "me@box");
        assert_eq!(short_user_host("me@example.com"), "me@example.com");
    }

    #[test]
    fn ids_shorten_on_character_boundaries() {
        assert_eq!(short_id("kzqxwvut", 4), "kzqx");
        assert_eq!(short_id("kz", 4), "kz");
        assert_eq!(short_id("café", 3), "caf");
        assert_eq!(short_id("é", 0), "");
    }

    #[test]
    fn counts_agree_with_their_nouns() {
        assert_eq!(counted(1, "file"), "1 file");
        assert_eq!(counted(0, "file"), "0 files");
        assert_eq!(counted(12, "descendant"), "12 descendants");
    }
}
