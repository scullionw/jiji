//! The stack-info comment: one Jiji-maintained comment per PR that
//! explains the whole stack to GitHub readers.
//!
//! The shape is jjpr's stack navigation comment (see the jjpr inspiration
//! note): a sentinel identifies the comment as Jiji's so re-submits edit
//! it in place, a machine-readable data line carries the stack so future
//! runs can keep listing PRs that have since left the local stack
//! ("fossils" — merged, closed, or reshaped away), and the visible part is
//! a plain numbered list, bottom-up in merge order, with the comment's own
//! PR bolded.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Identifies a Jiji stack comment; never change it, or every stack gets a
/// duplicate comment on its next submit.
pub const STACK_SENTINEL: &str = "<!-- jiji:stack-info -->";
const DATA_PREFIX: &str = "<!-- jiji:stack-data ";
const DATA_SUFFIX: &str = " -->";
const FOOTER: &str = "*This comment is kept up to date by Jiji.*";

/// The machine-readable stack embedded in the comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackData {
    pub version: u32,
    pub stack: Vec<StackDataItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackDataItem {
    pub bookmark: String,
    pub number: u64,
    pub url: String,
}

/// One live stack position for rendering: PRs being created in the same
/// submit have no number or URL yet and render as plain names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackEntry {
    pub bookmark: String,
    pub number: Option<u64>,
    pub url: Option<String>,
}

/// Is this comment body Jiji's stack comment?
pub fn is_stack_comment(body: &str) -> bool {
    body.contains(STACK_SENTINEL)
}

/// Parse the embedded stack data from an existing comment, if present.
pub fn parse_stack_data(body: &str) -> Option<StackData> {
    for line in body.lines() {
        let line = line.trim();
        if let Some(encoded) = line
            .strip_prefix(DATA_PREFIX)
            .and_then(|rest| rest.strip_suffix(DATA_SUFFIX))
        {
            let bytes = BASE64.decode(encoded.trim()).ok()?;
            return serde_json::from_slice(&bytes).ok();
        }
    }
    None
}

/// Entries the previous comment knew that are no longer in the live stack:
/// their PRs merged, closed, or were reshaped away locally, but GitHub
/// readers still benefit from the historical links. Previous order is
/// preserved.
pub fn inherit_fossils(previous: Option<&StackData>, live: &[StackEntry]) -> Vec<StackDataItem> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    previous
        .stack
        .iter()
        .filter(|item| !live.iter().any(|entry| entry.bookmark == item.bookmark))
        .cloned()
        .collect()
}

/// Render the comment for one PR. `live` is bottom-up (merge order);
/// `current` names the bookmark whose PR this comment sits on (`None`
/// renders the neutral preview the plan panel shows). `fossils` render
/// struck-through under the live list.
pub fn render_stack_comment(
    live: &[StackEntry],
    fossils: &[StackDataItem],
    current: Option<&str>,
) -> String {
    let mut data_items: Vec<StackDataItem> = live
        .iter()
        .filter_map(|entry| {
            Some(StackDataItem {
                bookmark: entry.bookmark.clone(),
                number: entry.number?,
                url: entry.url.clone()?,
            })
        })
        .collect();
    data_items.extend(fossils.iter().cloned());
    let data = StackData {
        version: 1,
        stack: data_items,
    };
    let json = serde_json::to_vec(&data).expect("stack data serializes");
    let encoded = BASE64.encode(json);

    let mut body = String::new();
    body.push_str(STACK_SENTINEL);
    body.push('\n');
    body.push_str(&format!("{DATA_PREFIX}{encoded}{DATA_SUFFIX}"));
    body.push('\n');
    body.push_str("This pull request is part of a stack, in merge order:\n\n");
    for entry in live {
        if current.is_some_and(|name| name == entry.bookmark) {
            body.push_str(&format!("1. **`{}` ← this PR**\n", entry.bookmark));
        } else if let Some(url) = &entry.url {
            body.push_str(&format!("1. [`{}`]({url})\n", entry.bookmark));
        } else {
            body.push_str(&format!("1. `{}`\n", entry.bookmark));
        }
    }
    if !fossils.is_empty() {
        body.push_str("\nEarlier parts of the stack, no longer open:\n\n");
        for fossil in fossils {
            body.push_str(&format!("1. ~~[`{}`]({})~~\n", fossil.bookmark, fossil.url));
        }
    }
    body.push_str(&format!("\n---\n{FOOTER}\n"));
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(bookmark: &str, number: Option<u64>) -> StackEntry {
        StackEntry {
            bookmark: bookmark.into(),
            number,
            url: number.map(|n| format!("https://github.com/o/r/pull/{n}")),
        }
    }

    #[test]
    fn renders_the_live_list_with_the_current_pr_bolded() {
        let live = [entry("auth", Some(6)), entry("profile", None)];
        let body = render_stack_comment(&live, &[], Some("auth"));
        assert!(is_stack_comment(&body));
        assert!(body.contains("1. **`auth` ← this PR**"));
        // A PR being created in the same submit renders as a plain name.
        assert!(body.contains("1. `profile`"));
        assert!(!body.contains("profile` ← this PR"));

        let sibling = render_stack_comment(&live, &[], Some("profile"));
        assert!(sibling.contains("1. [`auth`](https://github.com/o/r/pull/6)"));
        assert!(sibling.contains("1. **`profile` ← this PR**"));

        // The neutral preview bolds nothing.
        let preview = render_stack_comment(&live, &[], None);
        assert!(!preview.contains("this PR"));
    }

    #[test]
    fn data_round_trips_and_only_carries_real_prs() {
        let live = [entry("auth", Some(6)), entry("profile", None)];
        let body = render_stack_comment(&live, &[], Some("auth"));
        let data = parse_stack_data(&body).expect("data parses back");
        assert_eq!(data.version, 1);
        assert_eq!(data.stack.len(), 1, "the unnumbered entry is not persisted");
        assert_eq!(data.stack[0].bookmark, "auth");
        assert_eq!(data.stack[0].number, 6);

        assert_eq!(parse_stack_data("no marker here"), None);
        assert_eq!(
            parse_stack_data(&format!("{DATA_PREFIX}not base64!{DATA_SUFFIX}")),
            None
        );
    }

    #[test]
    fn fossils_inherit_from_previous_data_and_render_struck() {
        let previous = StackData {
            version: 1,
            stack: vec![
                StackDataItem {
                    bookmark: "landed".into(),
                    number: 4,
                    url: "https://github.com/o/r/pull/4".into(),
                },
                StackDataItem {
                    bookmark: "auth".into(),
                    number: 6,
                    url: "https://github.com/o/r/pull/6".into(),
                },
            ],
        };
        let live = [entry("auth", Some(6)), entry("profile", Some(7))];
        let fossils = inherit_fossils(Some(&previous), &live);
        assert_eq!(fossils.len(), 1);
        assert_eq!(fossils[0].bookmark, "landed");

        let body = render_stack_comment(&live, &fossils, Some("profile"));
        assert!(body.contains("~~[`landed`](https://github.com/o/r/pull/4)~~"));
        // Fossils persist in the data payload so the next run still knows.
        let data = parse_stack_data(&body).unwrap();
        assert!(data.stack.iter().any(|item| item.bookmark == "landed"));

        assert!(inherit_fossils(None, &live).is_empty());
    }
}
