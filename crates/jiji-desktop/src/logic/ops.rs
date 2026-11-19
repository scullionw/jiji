//! Shaping for the operation timeline: day grouping, and collapsing runs
//! of working-copy snapshots so the log reads like a journal of real work
//! instead of an undifferentiated stream.

use chrono::{DateTime, Datelike, FixedOffset};
use jiji_core::snapshot::OperationItem;

#[derive(Debug, PartialEq)]
pub enum TimelineRow<'a> {
    Op(&'a OperationItem),
    /// A collapsed run of snapshots, keyed by its first operation.
    Snapshots(&'a [OperationItem]),
}

#[derive(Debug, PartialEq)]
pub struct OpDayGroup<'a> {
    pub label: String,
    pub rows: Vec<TimelineRow<'a>>,
}

/// Runs of at least this many consecutive snapshot ops collapse into one
/// row.
pub const SNAPSHOT_RUN_MIN: usize = 3;

fn local(timestamp: &str, now: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|at| at.with_timezone(&now.timezone()))
}

pub fn now() -> DateTime<FixedOffset> {
    chrono::Local::now().fixed_offset()
}

pub fn day_label(timestamp: &str, now: DateTime<FixedOffset>) -> String {
    let Some(at) = local(timestamp, now) else {
        return timestamp.to_owned();
    };
    let (day, today) = (at.date_naive(), now.date_naive());
    if day == today {
        "Today".to_owned()
    } else if Some(day) == today.pred_opt() {
        "Yesterday".to_owned()
    } else if day.year() == today.year() {
        at.format("%B %-d").to_string()
    } else {
        at.format("%B %-d, %Y").to_string()
    }
}

/// Time of day for rows already grouped under a day heading.
pub fn clock_time(timestamp: &str, now: DateTime<FixedOffset>) -> String {
    local(timestamp, now).map_or_else(String::new, |at| at.format("%H:%M").to_string())
}

/// How many operations a restore to `op_id` would unwind: everything
/// newer than it in the newest-first list. Zero for the current head (a
/// no-op restore); `None` when the op is not in the list at all.
pub fn ops_since(ops: &[OperationItem], op_id: &str) -> Option<usize> {
    ops.iter().position(|op| op.id == op_id)
}

/// jj's root operation (its id is all zeros) is the state before the repo
/// existed: nothing to revert, and restoring to it would empty the repo —
/// it gets no time-travel affordances.
pub fn is_root_op(op: &OperationItem) -> bool {
    !op.id.is_empty() && op.id.bytes().all(|b| b == b'0')
}

fn collapse_snapshot_runs(ops: &[OperationItem]) -> Vec<TimelineRow<'_>> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        let mut j = i;
        while j < ops.len() && ops[j].is_snapshot && !ops[j].is_current {
            j += 1;
        }
        let run = &ops[i..j];
        if run.len() >= SNAPSHOT_RUN_MIN {
            rows.push(TimelineRow::Snapshots(run));
        } else {
            rows.extend(run.iter().map(TimelineRow::Op));
        }
        if j == i {
            rows.push(TimelineRow::Op(&ops[i]));
            j = i + 1;
        }
        i = j;
    }
    rows
}

/// Group newest-first operations by calendar day, collapsing snapshot runs
/// within each day. The current operation never collapses.
pub fn group_operations(ops: &[OperationItem], now: DateTime<FixedOffset>) -> Vec<OpDayGroup<'_>> {
    let mut groups: Vec<OpDayGroup> = Vec::new();
    let mut start = 0;
    let mut label: Option<String> = None;
    for (index, op) in ops.iter().enumerate() {
        let op_label = day_label(&op.timestamp, now);
        if label.as_ref() != Some(&op_label) {
            if let Some(label) = label.take() {
                groups.push(OpDayGroup {
                    label,
                    rows: collapse_snapshot_runs(&ops[start..index]),
                });
            }
            start = index;
            label = Some(op_label);
        }
    }
    if let Some(label) = label {
        groups.push(OpDayGroup {
            label,
            rows: collapse_snapshot_runs(&ops[start..]),
        });
    }
    groups
}

/// What an operation did, by the verb jj describes it with: the family
/// its station on the timeline is colored by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// Made or edited a change: commit, new, describe, edit, split.
    Change,
    /// Moved or folded history: rebase, squash, abandon, duplicate.
    Rewrite,
    /// Talked to a remote or to Git: fetch, push, import, export.
    Remote,
    /// Created, moved or dropped a bookmark or tag.
    Bookmark,
    /// Took operations back: undo, revert, restore.
    Undo,
    /// jj noticed the files changed.
    Snapshot,
    Other,
}

pub fn op_kind(description: &str, is_snapshot: bool) -> OpKind {
    if is_snapshot {
        return OpKind::Snapshot;
    }
    let lower = description.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    let mentions = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    match first {
        "undo" | "revert" | "restore" if mentions(&["operation"]) || first == "undo" => {
            OpKind::Undo
        }
        "fetch" | "push" | "import" | "export" | "clone" | "init" => OpKind::Remote,
        "rebase" | "squash" | "abandon" | "duplicate" | "absorb" | "parallelize" | "backout"
        | "revert" | "simplify" => OpKind::Rewrite,
        "commit" | "new" | "describe" | "edit" | "split" | "restore" | "resolve" | "diffedit"
        | "create" | "snapshot"
            if !mentions(&["bookmark", "tag "]) =>
        {
            OpKind::Change
        }
        "create" | "set" | "move" | "delete" | "forget" | "rename" | "track" | "untrack"
        | "point" => OpKind::Bookmark,
        _ if mentions(&["bookmark"]) => OpKind::Bookmark,
        _ => OpKind::Other,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn operations_are_known_by_their_verb() {
        use super::{op_kind, OpKind};
        assert_eq!(op_kind("snapshot working copy", true), OpKind::Snapshot);
        assert_eq!(op_kind("commit f8528903512b", false), OpKind::Change);
        assert_eq!(op_kind("new empty commit", false), OpKind::Change);
        assert_eq!(
            op_kind("rebase commit 1234 and descendants", false),
            OpKind::Rewrite
        );
        assert_eq!(
            op_kind("fetch from git remote(s) origin", false),
            OpKind::Remote
        );
        assert_eq!(
            op_kind("push all tracked bookmarks to git remote origin", false),
            OpKind::Remote
        );
        assert_eq!(
            op_kind("create bookmark main pointing to commit 1234", false),
            OpKind::Bookmark
        );
        assert_eq!(op_kind("undo operation 5678", false), OpKind::Undo);
        assert_eq!(op_kind("restore to operation 5678", false), OpKind::Undo);
        assert_eq!(op_kind("restore into commit 1234", false), OpKind::Change);
        assert_eq!(op_kind("", false), OpKind::Other);
    }

    use super::*;

    fn op(id: &str, timestamp: &str, snapshot: bool, current: bool) -> OperationItem {
        OperationItem {
            id: id.into(),
            description: if snapshot {
                "snapshot working copy"
            } else {
                "describe"
            }
            .into(),
            timestamp: timestamp.into(),
            is_current: current,
            user: "me@host".into(),
            is_snapshot: snapshot,
            effects: Vec::new(),
            more_effects: 0,
        }
    }

    fn at(text: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(text).unwrap()
    }

    #[test]
    fn days_read_relative_then_by_date() {
        let now = at("2026-03-10T12:00:00+00:00");
        assert_eq!(day_label("2026-03-10T01:00:00+00:00", now), "Today");
        assert_eq!(day_label("2026-03-09T23:00:00+00:00", now), "Yesterday");
        assert_eq!(day_label("2026-02-03T10:00:00+00:00", now), "February 3");
        assert_eq!(
            day_label("2025-12-31T10:00:00+00:00", now),
            "December 31, 2025"
        );
        // The day is the viewer's, not the recorder's.
        assert_eq!(day_label("2026-03-09T22:30:00-05:00", now), "Today");
        assert_eq!(clock_time("2026-03-10T09:05:00+00:00", now), "09:05");
    }

    #[test]
    fn snapshot_runs_collapse_but_never_swallow_the_current_operation() {
        let day = "2026-03-10T10:00:00+00:00";
        let ops = vec![
            op("a", day, true, true),
            op("b", day, true, false),
            op("c", day, true, false),
            op("d", day, true, false),
            op("e", day, false, false),
            op("f", day, true, false),
            op("g", day, true, false),
            op("h", "2026-03-09T10:00:00+00:00", false, false),
        ];
        let groups = group_operations(&ops, at("2026-03-10T12:00:00+00:00"));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].label, "Today");
        assert_eq!(
            groups[0].rows,
            vec![
                TimelineRow::Op(&ops[0]),
                TimelineRow::Snapshots(&ops[1..4]),
                TimelineRow::Op(&ops[4]),
                // A run of two stays as plain rows.
                TimelineRow::Op(&ops[5]),
                TimelineRow::Op(&ops[6]),
            ]
        );
        assert_eq!(groups[1].label, "Yesterday");
        assert_eq!(groups[1].rows, vec![TimelineRow::Op(&ops[7])]);
    }

    #[test]
    fn restores_count_what_they_unwind() {
        let day = "2026-03-10T10:00:00+00:00";
        let ops = vec![
            op("a", day, false, true),
            op("b", day, false, false),
            op("000", day, false, false),
        ];
        assert_eq!(ops_since(&ops, "a"), Some(0));
        assert_eq!(ops_since(&ops, "000"), Some(2));
        assert_eq!(ops_since(&ops, "zz"), None);
        assert!(is_root_op(&ops[2]) && !is_root_op(&ops[0]));
    }

    #[test]
    fn a_run_of_snapshots_never_collapses_across_midnight() {
        let ops = vec![
            op("a", "2026-06-10T01:00:00+00:00", true, false),
            op("b", "2026-06-10T00:30:00+00:00", true, false),
            op("c", "2026-06-09T23:59:00+00:00", true, false),
            op("d", "2026-06-09T23:00:00+00:00", true, false),
        ];
        let groups = group_operations(&ops, at("2026-06-10T13:00:00+00:00"));
        let labels: Vec<&str> = groups.iter().map(|group| group.label.as_str()).collect();
        assert_eq!(labels, ["Today", "Yesterday"]);
        assert_eq!(
            groups[0].rows,
            [TimelineRow::Op(&ops[0]), TimelineRow::Op(&ops[1])]
        );
        assert_eq!(
            groups[1].rows,
            [TimelineRow::Op(&ops[2]), TimelineRow::Op(&ops[3])]
        );
    }
}
