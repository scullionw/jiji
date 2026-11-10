//! Row model for the continuous multi-file diff: a Rust port of the pure
//! shaping logic in `src/lib/components/diff/diff.ts` (line numbering, hunk
//! gaps, similarity-paired split rows), flattened into one row list per
//! layout.
//!
//! Lines never wrap, so every row kind has a fixed height and a run of rows
//! has an exact height without laying anything out. That is what makes the
//! windowing trivial here: `tops` is a prefix sum, the first visible row is
//! a binary search, and nothing off screen is ever shaped or painted.

use std::borrow::Cow;
use std::ops::Range;

use gpui::SharedString;
use jiji_core::snapshot::{
    ChangeDiff, ConflictSection, DiffHunk, DiffLine, DiffLineKind, FileDiff, FileDiffContent,
    FileStatus,
};

use crate::highlight::{lang_for_path, HighlightJob, Mark};

pub type LineId = u32;

/// 12px code at line-height 1.5 (`--text-code` × `--lh-code`).
pub const LINE_H: f32 = 18.;
/// An elision band is one and a half rows.
pub const GAP_H: f32 = 27.;
/// 30px file header plus its bottom hairline.
pub const HEADER_H: f32 = 31.;
pub const NOTE_H: f32 = 34.;
/// `.lines { padding: 2px 0 }`.
pub const PAD_H: f32 = 2.;
const TAB: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Context,
    Removed,
    Added,
}

impl From<DiffLineKind> for Kind {
    fn from(kind: DiffLineKind) -> Self {
        match kind {
            DiffLineKind::Context => Kind::Context,
            DiffLineKind::Removed => Kind::Removed,
            DiffLineKind::Added => Kind::Added,
        }
    }
}

/// One line of diff text, stored once and shared by both layouts.
pub struct Line {
    /// What is painted: tabs expanded to 4-column stops, control characters
    /// blanked. Byte offsets everywhere (marks, selection) index this.
    pub text: SharedString,
    /// The original text, kept only when it differs from `text`, so copying
    /// returns what is actually in the file.
    source: Option<Box<str>>,
    /// Intraline emphasis ranges. Empty unless the line mixes changed and
    /// unchanged segments: a wholly added/removed line stays a flat tint.
    pub changed: Vec<Range<u32>>,
    /// Syntax marks, sorted and non-overlapping; filled in by the
    /// background highlighter after first paint.
    pub marks: Vec<Mark>,
    pub cols: u32,
}

impl Line {
    fn new(line: &DiffLine) -> Self {
        let intraline =
            line.segments.iter().any(|s| s.changed) && line.segments.iter().any(|s| !s.changed);
        let mut text = String::new();
        let mut col = 0usize;
        let mut rewritten = false;
        let mut changed: Vec<Range<u32>> = Vec::new();
        for segment in &line.segments {
            let start = text.len();
            for ch in segment.text.chars() {
                if ch == '\t' {
                    let width = TAB - col % TAB;
                    text.extend(std::iter::repeat_n(' ', width));
                    col += width;
                    rewritten = true;
                } else if ch.is_control() {
                    text.push(' ');
                    col += 1;
                    rewritten = true;
                } else {
                    text.push(ch);
                    col += 1;
                }
            }
            if intraline && segment.changed && text.len() > start {
                let range = start as u32..text.len() as u32;
                match changed.last_mut() {
                    Some(last) if last.end == range.start => last.end = range.end,
                    _ => changed.push(range),
                }
            }
        }
        let source = rewritten.then(|| {
            line.segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
                .into_boxed_str()
        });
        Self {
            text: text.into(),
            source,
            changed,
            marks: Vec::new(),
            cols: col as u32,
        }
    }

    /// The file's own text for a range of the painted text.
    pub fn source_slice(&self, range: Range<usize>) -> Cow<'_, str> {
        match &self.source {
            None => Cow::Borrowed(&self.text[range]),
            Some(source) => {
                let start = display_to_source(source, range.start);
                let end = display_to_source(source, range.end);
                Cow::Borrowed(&source[start..end.max(start)])
            }
        }
    }
}

/// Maps a byte offset in the painted text back to the original, replaying
/// the tab expansion.
fn display_to_source(source: &str, display: usize) -> usize {
    let mut shown = 0;
    let mut col = 0;
    for (index, ch) in source.char_indices() {
        if shown >= display {
            return index;
        }
        if ch == '\t' {
            let width = TAB - col % TAB;
            shown += width;
            col += width;
        } else {
            shown += if ch.is_control() { 1 } else { ch.len_utf8() };
            col += 1;
        }
    }
    source.len()
}

#[derive(Clone, Copy)]
pub struct Cell {
    pub line: LineId,
    pub no: u32,
    pub kind: Kind,
}

pub enum Row {
    FileHeader {
        file: u32,
    },
    Pad,
    /// The seam between hunks: how many unchanged lines were skipped, and
    /// the declaration enclosing the hunk below.
    Gap {
        skipped: u32,
        context: Option<SharedString>,
    },
    /// A full-width line: every row of the unified layout, and conflict
    /// content in either layout.
    Line {
        line: LineId,
        kind: Kind,
        old_no: Option<u32>,
        new_no: Option<u32>,
    },
    /// One aligned side-by-side row. A missing side is "nothing here".
    Split {
        left: Option<Cell>,
        right: Option<Cell>,
    },
    Note {
        text: SharedString,
        danger: bool,
    },
}

impl Row {
    fn height(&self) -> f32 {
        match self {
            Row::FileHeader { .. } => HEADER_H,
            Row::Pad => PAD_H,
            Row::Gap { .. } => GAP_H,
            Row::Line { .. } | Row::Split { .. } => LINE_H,
            Row::Note { .. } => NOTE_H,
        }
    }
}

pub struct Layout {
    pub rows: Vec<Row>,
    /// Prefix sum of row heights; `tops[i]` is row i's top and the last
    /// entry is the content height.
    pub tops: Vec<f32>,
    /// Row index of each file's header.
    pub file_rows: Vec<usize>,
    /// The file each row belongs to.
    row_files: Vec<u32>,
}

impl Layout {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            tops: vec![0.],
            file_rows: Vec::new(),
            row_files: Vec::new(),
        }
    }

    fn push(&mut self, row: Row) {
        let top = self.height();
        self.tops.push(top + row.height());
        self.rows.push(row);
        self.row_files
            .push(self.file_rows.len().saturating_sub(1) as u32);
    }

    pub fn height(&self) -> f32 {
        *self.tops.last().unwrap()
    }

    /// The row covering content offset `y`, clamped to the ends.
    pub fn row_at(&self, y: f32) -> usize {
        let after = self.tops.partition_point(|&top| top <= y);
        after
            .saturating_sub(1)
            .min(self.rows.len().saturating_sub(1))
    }

    pub fn file_of(&self, row: usize) -> usize {
        self.row_files.get(row).map_or(0, |&file| file as usize)
    }

    pub fn file_top(&self, file: usize) -> f32 {
        self.tops[self.file_rows[file]]
    }

    /// Where the file ends: the next header's top, or the content height.
    pub fn file_bottom(&self, file: usize) -> f32 {
        match self.file_rows.get(file + 1) {
            Some(&row) => self.tops[row],
            None => self.height(),
        }
    }
}

pub struct FileModel {
    pub dir: SharedString,
    pub name: SharedString,
    pub status: FileStatus,
    pub renamed_from: Option<SharedString>,
    pub has_conflict: bool,
    pub added: u32,
    pub removed: u32,
    /// The file's widest line, in columns: sizes its horizontal pan range.
    pub max_cols: u32,
}

pub struct DiffDoc {
    pub files: Vec<FileModel>,
    pub lines: Vec<Line>,
    pub unified: Layout,
    pub split: Layout,
    /// Digits in the widest line number across the diff (at least 2).
    pub digits: u32,
    pub added: u32,
    pub removed: u32,
    /// One per file with a known language; drained by the view at startup.
    pub jobs: Vec<HighlightJob>,
}

struct Numbered {
    id: LineId,
    kind: Kind,
    old_no: Option<u32>,
    new_no: Option<u32>,
    pair: Option<u32>,
}

impl DiffDoc {
    pub fn build(diff: &ChangeDiff) -> Self {
        let mut doc = Self {
            files: Vec::with_capacity(diff.files.len()),
            lines: Vec::new(),
            unified: Layout::new(),
            split: Layout::new(),
            digits: 2,
            added: 0,
            removed: 0,
            jobs: Vec::new(),
        };
        let mut max_no = 1u32;
        for (index, file) in diff.files.iter().enumerate() {
            let first_line = doc.lines.len();
            doc.push_both(|| Row::FileHeader { file: index as u32 }, true);
            let mut job = lang_for_path(&file.path).map(|lang| HighlightJob {
                lang,
                hunks: Vec::new(),
            });
            let (mut added, mut removed) = (0, 0);
            match &file.content {
                FileDiffContent::Text { hunks, truncated } => {
                    if hunks.is_empty() {
                        doc.note("No content changes", false);
                    } else {
                        doc.push_both(|| Row::Pad, false);
                        for (hunk_index, hunk) in hunks.iter().enumerate() {
                            if let Some(skipped) = gap_before(hunks, hunk_index) {
                                let context = hunk.context.clone().map(SharedString::from);
                                doc.push_both(
                                    || Row::Gap {
                                        skipped,
                                        context: context.clone(),
                                    },
                                    false,
                                );
                            }
                            let numbered = doc.number_hunk(hunk, true, &mut job);
                            for line in &numbered {
                                match line.kind {
                                    Kind::Added => added += 1,
                                    Kind::Removed => removed += 1,
                                    Kind::Context => {}
                                }
                                max_no = max_no
                                    .max(line.old_no.unwrap_or(0))
                                    .max(line.new_no.unwrap_or(0));
                                doc.unified.push(Row::Line {
                                    line: line.id,
                                    kind: line.kind,
                                    old_no: line.old_no,
                                    new_no: line.new_no,
                                });
                            }
                            split_rows(&numbered, &mut doc.split);
                        }
                        doc.push_both(|| Row::Pad, false);
                    }
                    if *truncated {
                        doc.note(
                            "Diff truncated: this file exceeds the per-file budget",
                            false,
                        );
                    }
                }
                FileDiffContent::Conflict {
                    sections,
                    truncated,
                } => {
                    doc.push_conflict(sections, &mut job);
                    if *truncated {
                        doc.note("Conflict truncated", false);
                    }
                }
                FileDiffContent::Image { .. } => {
                    doc.note("Image diff (not rendered in this spike)", false)
                }
                FileDiffContent::Binary { old_size, new_size } => {
                    doc.note(&binary_note(*old_size, *new_size), false)
                }
                FileDiffContent::TooLarge => doc.note("File too large to diff", false),
                FileDiffContent::Omitted => {
                    doc.note("Diff omitted: the change exceeds the diff budget", false)
                }
            }
            doc.added += added;
            doc.removed += removed;
            let max_cols = doc.lines[first_line..].iter().map(|line| line.cols).max();
            doc.files
                .push(file_model(file, added, removed, max_cols.unwrap_or(0)));
            if let Some(job) = job.filter(|job| !job.hunks.is_empty()) {
                doc.jobs.push(job);
            }
        }
        doc.digits = (max_no.max(1).ilog10() + 1).max(2);
        doc
    }

    fn push_both(&mut self, mut row: impl FnMut() -> Row, header: bool) {
        for layout in [&mut self.unified, &mut self.split] {
            if header {
                layout.file_rows.push(layout.rows.len());
            }
            layout.push(row());
        }
    }

    fn note(&mut self, text: &str, danger: bool) {
        let text = SharedString::from(text.to_owned());
        self.push_both(
            || Row::Note {
                text: text.clone(),
                danger,
            },
            false,
        );
    }

    /// Stores a hunk's lines and numbers them: context advances both sides,
    /// removed only the old side, added only the new side.
    fn number_hunk(
        &mut self,
        hunk: &DiffHunk,
        numbered: bool,
        job: &mut Option<HighlightJob>,
    ) -> Vec<Numbered> {
        let mut old_no = hunk.old_start;
        let mut new_no = hunk.new_start;
        let mut out = Vec::with_capacity(hunk.lines.len());
        let mut job_lines = Vec::with_capacity(hunk.lines.len());
        for line in &hunk.lines {
            let kind = Kind::from(line.kind);
            let id = self.lines.len() as LineId;
            let stored = Line::new(line);
            job_lines.push((id, kind, stored.text.clone()));
            self.lines.push(stored);
            let (old, new) = match kind {
                Kind::Context => (Some(old_no), Some(new_no)),
                Kind::Removed => (Some(old_no), None),
                Kind::Added => (None, Some(new_no)),
            };
            old_no += old.is_some() as u32;
            new_no += new.is_some() as u32;
            out.push(Numbered {
                id,
                kind,
                old_no: old.filter(|_| numbered),
                new_no: new.filter(|_| numbered),
                pair: line.pair,
            });
        }
        if let Some(job) = job {
            job.hunks.push(job_lines);
        }
        out
    }

    /// A conflicted file's merge structure: resolved runs as context, each
    /// region's sides as their diff from the base. Rendered full-width in
    /// both layouts; line coordinates are meaningless here and stay blank.
    fn push_conflict(&mut self, sections: &[ConflictSection], job: &mut Option<HighlightJob>) {
        let pseudo = |lines: &[DiffLine]| DiffHunk {
            old_start: 0,
            new_start: 0,
            context: None,
            lines: lines.to_vec(),
        };
        for section in sections {
            match section {
                ConflictSection::Resolved { lines } => {
                    let numbered = self.number_hunk(&pseudo(lines), false, job);
                    self.push_lines(&numbered);
                }
                ConflictSection::Elided { lines } => {
                    let skipped = *lines;
                    self.push_both(
                        || Row::Gap {
                            skipped,
                            context: None,
                        },
                        false,
                    );
                }
                ConflictSection::Region { sides } => {
                    for (index, side) in sides.iter().enumerate() {
                        let label = match &side.label {
                            Some(label) => format!("Conflict side #{}: {label}", index + 1),
                            None => format!("Conflict side #{}", index + 1),
                        };
                        self.note(&label, true);
                        let numbered = self.number_hunk(&pseudo(&side.lines), false, job);
                        self.push_lines(&numbered);
                    }
                }
            }
        }
    }

    fn push_lines(&mut self, numbered: &[Numbered]) {
        for line in numbered {
            self.push_both(
                || Row::Line {
                    line: line.id,
                    kind: line.kind,
                    old_no: None,
                    new_no: None,
                },
                false,
            );
        }
    }
}

fn file_model(file: &FileDiff, added: u32, removed: u32, max_cols: u32) -> FileModel {
    let (dir, name) = match file.path.rfind('/') {
        Some(slash) => file.path.split_at(slash + 1),
        None => ("", file.path.as_str()),
    };
    FileModel {
        dir: dir.to_owned().into(),
        name: name.to_owned().into(),
        status: file.status,
        renamed_from: file.renamed_from.clone().map(SharedString::from),
        has_conflict: file.has_conflict,
        added,
        removed,
        max_cols,
    }
}

/// Unchanged lines skipped before `hunks[index]`: above the first hunk, or
/// between it and its predecessor.
fn gap_before(hunks: &[DiffHunk], index: usize) -> Option<u32> {
    let start = hunks[index].new_start;
    if index == 0 {
        return (start > 1).then(|| start - 1);
    }
    let prev = &hunks[index - 1];
    let prev_new = prev
        .lines
        .iter()
        .filter(|line| line.kind != DiffLineKind::Removed)
        .count() as u32;
    start
        .checked_sub(prev.new_start + prev_new)
        .filter(|&n| n > 0)
}

/// Aligns each changed block on the backend's similarity pairs: a paired
/// removed/added line shares one row, so what changed reads straight
/// across, and the unpaired stretches between anchors pack side by side.
/// A block with no pairs degenerates to positional packing.
fn split_rows(lines: &[Numbered], out: &mut Layout) {
    // (cell, index in hunk, pair)
    let mut removed: Vec<(Cell, usize, Option<u32>)> = Vec::new();
    let mut added: Vec<(Cell, usize)> = Vec::new();

    fn flush(
        removed: &mut Vec<(Cell, usize, Option<u32>)>,
        added: &mut Vec<(Cell, usize)>,
        out: &mut Layout,
    ) {
        let (mut i, mut j) = (0, 0);
        let pack = |i: &mut usize, j: &mut usize, end_i: usize, end_j: usize, out: &mut Layout| {
            for k in 0..(end_i - *i).max(end_j - *j) {
                out.push(Row::Split {
                    left: (*i + k < end_i).then(|| removed[*i + k].0),
                    right: (*j + k < end_j).then(|| added[*j + k].0),
                });
            }
            *i = end_i;
            *j = end_j;
        };
        for r in 0..removed.len() {
            let Some(pair) = removed[r].2 else { continue };
            let Some(a) = added.iter().position(|(_, index)| *index == pair as usize) else {
                continue;
            };
            // Pairs are monotonic by construction; skip any link a truncated
            // hunk left dangling rather than misalign.
            if r < i || a < j {
                continue;
            }
            pack(&mut i, &mut j, r, a, out);
            out.push(Row::Split {
                left: Some(removed[i].0),
                right: Some(added[j].0),
            });
            i += 1;
            j += 1;
        }
        pack(&mut i, &mut j, removed.len(), added.len(), out);
        removed.clear();
        added.clear();
    }

    for (index, line) in lines.iter().enumerate() {
        let cell = |no: Option<u32>| Cell {
            line: line.id,
            no: no.unwrap_or(0),
            kind: line.kind,
        };
        match line.kind {
            Kind::Removed => removed.push((cell(line.old_no), index, line.pair)),
            Kind::Added => added.push((cell(line.new_no), index)),
            Kind::Context => {
                flush(&mut removed, &mut added, out);
                out.push(Row::Split {
                    left: Some(cell(line.old_no)),
                    right: Some(cell(line.new_no)),
                });
            }
        }
    }
    flush(&mut removed, &mut added, out);
}

/// Human-readable byte size, decimal units: "847 B", "24.1 KB", "3.2 MB".
fn format_bytes(bytes: u64) -> String {
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    for unit in ["KB", "MB", "GB", "TB"] {
        value /= 1000.;
        if value < 1000. || unit == "TB" {
            return if value >= 100. {
                format!("{value:.0} {unit}")
            } else {
                format!("{} {unit}", (value * 10.).round() / 10.)
            };
        }
    }
    unreachable!()
}

/// The one line a binary file's diff gets: what the sizes say about the
/// change. A missing side means the file was added or removed.
fn binary_note(old: Option<u64>, new: Option<u64>) -> String {
    match (old, new) {
        (Some(old), Some(new)) if old == new => format!("Binary file — {}", format_bytes(new)),
        (Some(old), Some(new)) => {
            let sign = if new > old { "+" } else { "−" };
            format!(
                "Binary file — {} → {} ({sign}{})",
                format_bytes(old),
                format_bytes(new),
                format_bytes(old.abs_diff(new))
            )
        }
        (None, Some(new)) => format!("Binary file — {}", format_bytes(new)),
        (Some(old), None) => format!("Binary file — was {}", format_bytes(old)),
        (None, None) => "Binary file — no text diff".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::snapshot::DiffSegment;

    fn line(kind: DiffLineKind, text: &str, pair: Option<u32>) -> DiffLine {
        DiffLine {
            kind,
            segments: vec![DiffSegment {
                text: text.to_owned(),
                changed: kind != DiffLineKind::Context,
            }],
            pair,
        }
    }

    fn text_file(path: &str, hunks: Vec<DiffHunk>) -> FileDiff {
        FileDiff {
            path: path.to_owned(),
            status: FileStatus::Modified,
            renamed_from: None,
            has_conflict: false,
            content: FileDiffContent::Text {
                hunks,
                truncated: false,
            },
        }
    }

    fn doc(hunks: Vec<DiffHunk>) -> DiffDoc {
        DiffDoc::build(&ChangeDiff {
            id: "x".into(),
            from: None,
            files: vec![text_file("src/a.txt", hunks)],
            truncated: false,
        })
    }

    #[test]
    fn numbers_lines_and_reports_the_gap_above_a_hunk() {
        use DiffLineKind::*;
        let doc = doc(vec![DiffHunk {
            old_start: 10,
            new_start: 12,
            context: Some("fn main()".into()),
            lines: vec![
                line(Context, "a", None),
                line(Removed, "b", None),
                line(Added, "c", None),
                line(Context, "d", None),
            ],
        }]);
        let rows = &doc.unified.rows;
        assert!(matches!(rows[0], Row::FileHeader { file: 0 }));
        assert!(matches!(rows[2], Row::Gap { skipped: 11, .. }));
        let numbers: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Line { old_no, new_no, .. } => Some((*old_no, *new_no)),
                _ => None,
            })
            .collect();
        assert_eq!(
            numbers,
            vec![
                (Some(10), Some(12)),
                (Some(11), None),
                (None, Some(13)),
                (Some(12), Some(14)),
            ]
        );
        assert_eq!((doc.added, doc.removed), (1, 1));
        assert_eq!(
            doc.unified.height(),
            HEADER_H + 2. * PAD_H + GAP_H + 4. * LINE_H
        );
    }

    #[test]
    fn split_rows_align_on_similarity_pairs() {
        use DiffLineKind::*;
        // Two removed, three added; removed[1] (index 1) pairs with the
        // last added line (index 4). The unpaired stretch before the anchor
        // packs side by side, then the pair shares a row.
        let doc = doc(vec![DiffHunk {
            old_start: 1,
            new_start: 1,
            context: None,
            lines: vec![
                line(Removed, "r0", None),
                line(Removed, "r1", Some(4)),
                line(Added, "a0", None),
                line(Added, "a1", None),
                line(Added, "a2", Some(1)),
            ],
        }]);
        let cells: Vec<_> = doc
            .split
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Split { left, right } => Some((
                    left.map(|c| doc.lines[c.line as usize].text.to_string()),
                    right.map(|c| doc.lines[c.line as usize].text.to_string()),
                )),
                _ => None,
            })
            .collect();
        let s = |v: &str| Some(v.to_owned());
        assert_eq!(
            cells,
            vec![(s("r0"), s("a0")), (None, s("a1")), (s("r1"), s("a2"))]
        );
    }

    #[test]
    fn tabs_expand_for_display_and_copy_returns_the_source() {
        let stored = Line::new(&DiffLine {
            kind: DiffLineKind::Context,
            segments: vec![DiffSegment {
                text: "\tif x {\t// y".to_owned(),
                changed: false,
            }],
            pair: None,
        });
        assert_eq!(stored.text.to_string(), "    if x {  // y");
        assert_eq!(stored.cols, 16);
        assert_eq!(stored.source_slice(0..stored.text.len()), "\tif x {\t// y");
        assert_eq!(stored.source_slice(4..8), "if x");
    }

    #[test]
    fn row_lookup_clamps_and_maps_rows_to_files() {
        let two = DiffDoc::build(&ChangeDiff {
            id: "x".into(),
            from: None,
            files: vec![text_file("a", vec![]), text_file("b", vec![])],
            truncated: false,
        });
        let layout = &two.unified;
        assert_eq!(layout.row_at(-5.), 0);
        assert_eq!(layout.row_at(HEADER_H + 1.), 1);
        assert_eq!(layout.row_at(1e9), layout.rows.len() - 1);
        assert_eq!(layout.file_of(1), 0);
        assert_eq!(layout.file_of(layout.file_rows[1]), 1);
    }
}
