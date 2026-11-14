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

#[cfg(test)]
mod tests {
    use super::*;

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
