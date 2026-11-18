//! Shaping for the history search overlay: how one backend result reads
//! as rows and status text.

use jiji_core::snapshot::{HistorySearch, SearchFileHit};

/// A query long enough to search. One character matches half of history
/// and costs a full walk; two is where results start meaning something.
pub const MIN_QUERY_LENGTH: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTone {
    Add,
    Del,
    Name,
}

/// One file hit's compact label: the occurrence delta the change caused
/// ("+2" introduced two, "−1" removed one), or "name" when only the file's
/// path matched the query.
pub fn file_hit_label(hit: &SearchFileHit) -> (String, HitTone) {
    if hit.added > hit.removed {
        (format!("+{}", hit.added - hit.removed), HitTone::Add)
    } else if hit.removed > hit.added {
        (format!("\u{2212}{}", hit.removed - hit.added), HitTone::Del)
    } else {
        ("name".to_owned(), HitTone::Name)
    }
}

/// The footer line under a finished search: how much matched, how far the
/// walk got, and whether it stopped early.
pub fn search_status(result: &HistorySearch) -> String {
    let count = result.hits.len();
    let matches = if count == 1 {
        "1 matching change".to_owned()
    } else {
        format!("{count} matching changes")
    };
    let scanned = format!(
        "searched {} change{}",
        result.scanned,
        if result.scanned == 1 { "" } else { "s" }
    );
    let capped = if result.truncated {
        " · stopped early, older history unsearched"
    } else {
        ""
    };
    format!("{matches} · {scanned}{capped}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(added: u32, removed: u32) -> SearchFileHit {
        SearchFileHit {
            path: "a.rs".into(),
            added,
            removed,
            path_match: false,
        }
    }

    #[test]
    fn file_labels_say_what_the_change_did_to_the_string() {
        assert_eq!(file_hit_label(&file(3, 1)), ("+2".into(), HitTone::Add));
        assert_eq!(
            file_hit_label(&file(0, 1)),
            ("\u{2212}1".into(), HitTone::Del)
        );
        assert_eq!(file_hit_label(&file(2, 2)), ("name".into(), HitTone::Name));
    }

    #[test]
    fn the_status_line_counts_and_admits_an_early_stop() {
        let mut result = HistorySearch {
            query: "x".into(),
            hits: Vec::new(),
            scanned: 1,
            truncated: false,
        };
        assert_eq!(
            search_status(&result),
            "0 matching changes · searched 1 change"
        );
        result.scanned = 40;
        result.truncated = true;
        assert_eq!(
            search_status(&result),
            "0 matching changes · searched 40 changes · stopped early, older history unsearched"
        );
    }
}
