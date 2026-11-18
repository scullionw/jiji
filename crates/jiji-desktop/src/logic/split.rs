//! The split panel's selection model: which files, hunks or single lines
//! of a change are checked, what that sends to the backend, and when a
//! selection is ready to run.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiji_core::snapshot::{
    DiffHunk, DiffLine, DiffLineKind, FileDiff, FileDiffContent, SplitHunk, SplitSelection,
};

/// One hunk's part in a selection: all of it, or some of its changed
/// lines (indices into the hunk's lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkPick {
    All,
    Lines(BTreeSet<u32>),
}

/// One file's part in a selection: the whole file, or some of its hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitPick {
    All,
    Hunks(BTreeMap<usize, HunkPick>),
}

pub type Selection = HashMap<String, SplitPick>;

pub fn file_hunks(file: &FileDiff) -> &[DiffHunk] {
    match &file.content {
        FileDiffContent::Text { hunks, .. } => hunks,
        _ => &[],
    }
}

/// Hunk-level picking needs a clean, fully rendered text diff with more
/// than one hunk to choose between.
pub fn can_select_hunks(file: &FileDiff) -> bool {
    matches!(
        &file.content,
        FileDiffContent::Text { hunks, truncated: false } if !file.has_conflict && hunks.len() >= 2
    )
}

pub fn changed_line_indices(hunk: &DiffHunk) -> Vec<u32> {
    hunk.lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.kind != DiffLineKind::Context)
        .map(|(index, _)| index as u32)
        .collect()
}

pub fn can_select_lines(hunk: &DiffHunk) -> bool {
    changed_line_indices(hunk).len() >= 2
}

/// A hunk's unified-diff coordinates, as the backend verifies them.
pub fn hunk_coords(hunk: &DiffHunk) -> SplitHunk {
    let count = |skip: DiffLineKind| hunk.lines.iter().filter(|l| l.kind != skip).count() as u32;
    SplitHunk {
        old_start: hunk.old_start,
        new_start: hunk.new_start,
        old_lines: count(DiffLineKind::Added),
        new_lines: count(DiffLineKind::Removed),
        lines: None,
    }
}

pub fn hunk_label(hunk: &DiffHunk) -> String {
    let c = hunk_coords(hunk);
    format!(
        "@@ -{},{} +{},{}",
        c.old_start, c.old_lines, c.new_start, c.new_lines
    )
}

pub fn hunk_stats(hunk: &DiffHunk) -> (u32, u32) {
    let count = |kind: DiffLineKind| hunk.lines.iter().filter(|l| l.kind == kind).count() as u32;
    (count(DiffLineKind::Added), count(DiffLineKind::Removed))
}

pub fn line_text(line: &DiffLine) -> String {
    line.segments.iter().map(|s| s.text.as_str()).collect()
}

/// The hunk's first changed line, as a one-line teaser.
pub fn hunk_preview(hunk: &DiffHunk) -> String {
    hunk.lines
        .iter()
        .find(|line| line.kind != DiffLineKind::Context)
        .map(|line| {
            let sign = if line.kind == DiffLineKind::Added {
                '+'
            } else {
                '-'
            };
            format!("{sign} {}", line_text(line).trim_start())
        })
        .unwrap_or_default()
}

pub fn toggle_file(selection: &mut Selection, path: &str) {
    if selection.remove(path) != Some(SplitPick::All) {
        selection.insert(path.to_owned(), SplitPick::All);
    }
}

fn materialize(current: Option<&SplitPick>, hunk_count: usize) -> BTreeMap<usize, HunkPick> {
    match current {
        Some(SplitPick::All) => (0..hunk_count).map(|i| (i, HunkPick::All)).collect(),
        Some(SplitPick::Hunks(map)) => map.clone(),
        None => BTreeMap::new(),
    }
}

/// Every hunk fully picked is the whole file; nothing picked is no entry.
fn normalize(map: BTreeMap<usize, HunkPick>, hunk_count: usize) -> Option<SplitPick> {
    if map.is_empty() {
        None
    } else if map.len() == hunk_count && map.values().all(|pick| *pick == HunkPick::All) {
        Some(SplitPick::All)
    } else {
        Some(SplitPick::Hunks(map))
    }
}

fn store(selection: &mut Selection, path: &str, pick: Option<SplitPick>) {
    match pick {
        Some(pick) => {
            selection.insert(path.to_owned(), pick);
        }
        None => {
            selection.remove(path);
        }
    }
}

pub fn toggle_hunk(selection: &mut Selection, path: &str, index: usize, hunk_count: usize) {
    let mut map = materialize(selection.get(path), hunk_count);
    if map.remove(&index) != Some(HunkPick::All) {
        map.insert(index, HunkPick::All);
    }
    store(selection, path, normalize(map, hunk_count));
}

pub fn toggle_line(selection: &mut Selection, file: &FileDiff, hunk_index: usize, line_index: u32) {
    let hunks = file_hunks(file);
    let Some(hunk) = hunks.get(hunk_index) else {
        return;
    };
    let mut map = materialize(selection.get(&file.path), hunks.len());
    let changed = changed_line_indices(hunk);
    let mut set: BTreeSet<u32> = match map.get(&hunk_index) {
        Some(HunkPick::All) => changed.iter().copied().collect(),
        Some(HunkPick::Lines(lines)) => lines.clone(),
        None => BTreeSet::new(),
    };
    if !set.remove(&line_index) {
        set.insert(line_index);
    }
    if set.is_empty() {
        map.remove(&hunk_index);
    } else if set.len() == changed.len() {
        map.insert(hunk_index, HunkPick::All);
    } else {
        map.insert(hunk_index, HunkPick::Lines(set));
    }
    store(selection, &file.path, normalize(map, hunks.len()));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SplitSummary {
    /// Files checked whole.
    pub whole: usize,
    /// Files with only some hunks or lines checked.
    pub partial: usize,
    pub hunks: usize,
    pub lines: usize,
    /// Every file is fully checked: nothing would be left behind.
    pub all_covered: bool,
    /// Something is checked and something is left: a split can run.
    pub valid: bool,
}

pub fn split_summary(files: Option<&[FileDiff]>, selection: &Selection) -> SplitSummary {
    let mut summary = SplitSummary::default();
    for file in files.unwrap_or_default() {
        match selection.get(&file.path) {
            None => {}
            Some(SplitPick::All) => summary.whole += 1,
            Some(SplitPick::Hunks(map)) => {
                summary.partial += 1;
                for pick in map.values() {
                    match pick {
                        HunkPick::All => summary.hunks += 1,
                        HunkPick::Lines(lines) => summary.lines += lines.len(),
                    }
                }
            }
        }
    }
    let count = files.map_or(0, <[FileDiff]>::len);
    summary.all_covered = count > 0 && summary.whole == count;
    summary.valid = files.is_some() && summary.whole + summary.partial > 0 && !summary.all_covered;
    summary
}

/// What the backend receives: checked files in diff order, each whole or
/// as the coordinates of its picked hunks.
pub fn split_payload(files: &[FileDiff], selection: &Selection) -> Vec<SplitSelection> {
    files
        .iter()
        .filter_map(|file| match selection.get(&file.path)? {
            SplitPick::All => Some(SplitSelection::whole(file.path.clone())),
            SplitPick::Hunks(map) => {
                let hunks = file_hunks(file);
                Some(SplitSelection {
                    path: file.path.clone(),
                    hunks: Some(
                        map.iter()
                            .filter_map(|(index, pick)| {
                                let mut coords = hunk_coords(hunks.get(*index)?);
                                if let HunkPick::Lines(lines) = pick {
                                    coords.lines = Some(lines.iter().copied().collect());
                                }
                                Some(coords)
                            })
                            .collect(),
                    ),
                })
            }
        })
        .collect()
}

/// Whether the panel may run: a new change needs something on each side;
/// moving into an existing change needs a selection and a destination.
pub fn selection_ready(summary: &SplitSummary, into: Option<Option<&str>>) -> bool {
    match into {
        None => summary.valid,
        Some(destination) => summary.whole + summary.partial > 0 && destination.is_some(),
    }
}

/// A change can split when there is more than one thing to tell apart.
pub fn splittable(files: &[FileDiff]) -> bool {
    files.len() >= 2 || (files.len() == 1 && can_select_hunks(&files[0]))
}

pub fn movable(files: &[FileDiff]) -> bool {
    !files.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::snapshot::{DiffSegment, FileStatus};

    fn line(kind: DiffLineKind, text: &str) -> DiffLine {
        DiffLine {
            kind,
            segments: vec![DiffSegment {
                text: text.into(),
                changed: false,
            }],
            pair: None,
        }
    }

    fn hunk(start: u32) -> DiffHunk {
        use DiffLineKind::*;
        DiffHunk {
            old_start: start,
            new_start: start,
            context: None,
            lines: vec![
                line(Context, "a"),
                line(Removed, "b"),
                line(Added, "  c"),
                line(Added, "d"),
            ],
        }
    }

    fn file(path: &str, hunks: usize) -> FileDiff {
        FileDiff {
            path: path.into(),
            status: FileStatus::Modified,
            renamed_from: None,
            has_conflict: false,
            content: FileDiffContent::Text {
                hunks: (0..hunks).map(|i| hunk(10 * (i as u32 + 1))).collect(),
                truncated: false,
            },
        }
    }

    #[test]
    fn hunks_describe_themselves() {
        let h = hunk(10);
        assert_eq!(hunk_label(&h), "@@ -10,2 +10,3");
        assert_eq!(hunk_stats(&h), (2, 1));
        assert_eq!(hunk_preview(&h), "- b");
        assert_eq!(changed_line_indices(&h), [1, 2, 3]);
    }

    #[test]
    fn picking_every_hunk_or_line_collapses_to_the_whole() {
        let f = file("a.rs", 2);
        let mut selection = Selection::new();
        toggle_hunk(&mut selection, "a.rs", 0, 2);
        assert!(matches!(selection["a.rs"], SplitPick::Hunks(_)));
        toggle_hunk(&mut selection, "a.rs", 1, 2);
        assert_eq!(selection["a.rs"], SplitPick::All);
        // Unpicking one line of a whole file leaves the rest picked.
        toggle_line(&mut selection, &f, 0, 2);
        let SplitPick::Hunks(map) = &selection["a.rs"] else {
            panic!("partial");
        };
        assert_eq!(map[&0], HunkPick::Lines(BTreeSet::from([1, 3])));
        assert_eq!(map[&1], HunkPick::All);
        // Picking it back restores the whole file.
        toggle_line(&mut selection, &f, 0, 2);
        assert_eq!(selection["a.rs"], SplitPick::All);
        toggle_file(&mut selection, "a.rs");
        assert!(selection.is_empty());
    }

    #[test]
    fn a_split_needs_something_on_each_side() {
        let files = [file("a.rs", 2), file("b.rs", 1)];
        let mut selection = Selection::new();
        assert!(!split_summary(Some(&files), &selection).valid);
        toggle_file(&mut selection, "a.rs");
        let summary = split_summary(Some(&files), &selection);
        assert!(summary.valid && !summary.all_covered);
        assert!(selection_ready(&summary, None));
        // Moving into an existing change needs a destination instead.
        assert!(!selection_ready(&summary, Some(None)));
        assert!(selection_ready(&summary, Some(Some("kx"))));
        toggle_file(&mut selection, "b.rs");
        let summary = split_summary(Some(&files), &selection);
        assert!(summary.all_covered && !summary.valid);
        // Everything may still move into another change.
        assert!(selection_ready(&summary, Some(Some("kx"))));
        assert!(!split_summary(None, &selection).valid);
    }

    #[test]
    fn the_payload_carries_coordinates_for_partial_files() {
        let files = [file("a.rs", 2), file("b.rs", 1)];
        let mut selection = Selection::new();
        toggle_file(&mut selection, "b.rs");
        toggle_line(&mut selection, &files[0], 1, 1);
        let payload = split_payload(&files, &selection);
        assert_eq!(payload.len(), 2);
        let hunks = payload[0].hunks.as_ref().unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(
            (hunks[0].old_start, hunks[0].old_lines, hunks[0].new_lines),
            (20, 2, 3)
        );
        assert_eq!(hunks[0].lines, Some(vec![1]));
        assert_eq!(payload[1], SplitSelection::whole("b.rs"));
    }

    #[test]
    fn one_file_splits_only_when_its_hunks_can_be_told_apart() {
        assert!(splittable(&[file("a.rs", 2)]));
        assert!(!splittable(&[file("a.rs", 1)]));
        assert!(splittable(&[file("a.rs", 1), file("b.rs", 1)]));
        assert!(movable(&[file("a.rs", 1)]) && !movable(&[]));
        let mut conflicted = file("a.rs", 2);
        conflicted.has_conflict = true;
        assert!(!can_select_hunks(&conflicted));
    }
}
