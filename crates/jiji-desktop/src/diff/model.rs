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
use std::sync::{Arc, OnceLock};

use futures::future::{FutureExt as _, Shared};
use gpui::{BackgroundExecutor, SharedString, Task};
use jiji_core::snapshot::{
    ChangeDiff, ConflictSection, DiffHunk, DiffLine, DiffLineKind, FileDiff, FileDiffContent,
    FileStatus,
};

use crate::diff::highlight::{lang_for_path, HighlightJob, Mark};
use crate::diff::picture::{format_for, Picture, Undrawable};

pub type LineId = u32;

/// 12px code at line-height 1.5 (`--text-code` × `--lh-code`).
pub const LINE_H: f32 = 18.;
/// An elision band is one and a half rows.
pub const GAP_H: f32 = 27.;
/// 30px file header plus its bottom hairline.
pub const HEADER_H: f32 = 31.;
pub const NOTE_H: f32 = 34.;
/// An image comparison: the picture boxes, their captions and padding.
pub const IMAGE_H: f32 = 300.;
/// The tallest a picture draws inside its box.
pub const IMAGE_MAX_H: f32 = 220.;
pub const REGION_H: f32 = 26.;
pub const SIDE_H: f32 = 22.;
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
            cols: col as u32,
        }
    }

    /// The file's own text for a range of the painted text.
    /// The whole line as it is in the file (tabs intact).
    pub fn source_text(&self) -> &str {
        match &self.source {
            Some(source) => source,
            None => &self.text,
        }
    }

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

#[derive(Clone)]
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
    /// An image file's two sides as pictures, in one tall row.
    Image {
        file: u32,
    },
    /// The heading of one conflict region, or of one side within it.
    Region {
        text: SharedString,
        /// A side's caption ("vs base", its label); `None` on the region's
        /// own heading.
        side: Option<SharedString>,
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
            Row::Image { .. } => IMAGE_H,
            Row::Region { side: None, .. } => REGION_H,
            Row::Region { .. } => SIDE_H,
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

    /// This layout with the bodies of collapsed files left out: their
    /// headers stay, so the file list still reads top to bottom.
    pub fn collapsing(&self, collapsed: &[bool]) -> Layout {
        let mut out = Layout::new();
        for (file, &start) in self.file_rows.iter().enumerate() {
            let end = self
                .file_rows
                .get(file + 1)
                .copied()
                .unwrap_or(self.rows.len());
            out.file_rows.push(out.rows.len());
            let end = if collapsed.get(file).copied().unwrap_or(false) {
                start + 1
            } else {
                end
            };
            for row in &self.rows[start..end] {
                out.push(row.clone());
            }
        }
        out
    }

    /// The first row showing `line`, on either side.
    pub fn row_of_line(&self, file: usize, line: LineId) -> Option<usize> {
        let start = *self.file_rows.get(file)?;
        let end = self
            .file_rows
            .get(file + 1)
            .copied()
            .unwrap_or(self.rows.len());
        (start..end).find(|&row| match &self.rows[row] {
            Row::Line { line: id, .. } => *id == line,
            Row::Split { left, right } => {
                left.is_some_and(|cell| cell.line == line)
                    || right.is_some_and(|cell| cell.line == line)
            }
            _ => false,
        })
    }
}

pub struct FileModel {
    /// Repo-relative path, `/`-separated.
    pub path: SharedString,
    /// The file's lines in `DiffDoc::lines`.
    pub lines: Range<LineId>,
    /// How many conflict regions the file holds (conflicted files only).
    pub regions: u32,
    pub dir: SharedString,
    pub name: SharedString,
    pub status: FileStatus,
    pub renamed_from: Option<SharedString>,
    pub has_conflict: bool,
    pub added: u32,
    pub removed: u32,
    /// The file's widest line, in columns: sizes its horizontal pan range.
    pub max_cols: u32,
    /// An image file's sides, old then new; a missing side means the file
    /// was added or removed.
    pub images: Option<[Option<ImageSide>; 2]>,
}

/// One side of an image comparison.
pub struct ImageSide {
    /// The picture to decode, or why the side has none to show.
    pub picture: Result<Picture, Undrawable>,
    pub mime: SharedString,
    /// The side's full byte size, already phrased.
    pub size: String,
}

fn image_side(content: &jiji_core::snapshot::ImageContent) -> ImageSide {
    use base64::Engine as _;
    let picture = match (format_for(&content.mime), &content.data) {
        // The backend keeps a side's bytes back past its per-image cap, or
        // once the change's images have used what one diff carries.
        (_, None) if content.size > jiji_core::jj::MAX_IMAGE_INLINE_BYTES as u64 => {
            Err(Undrawable::TooLarge)
        }
        (_, None) => Err(Undrawable::OverBudget),
        (None, Some(_)) => Err(Undrawable::Unsupported),
        (Some(format), Some(data)) => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map(|bytes| Picture::new(format, bytes))
            .map_err(|_| Undrawable::Corrupt),
    };
    ImageSide {
        picture,
        mime: content.mime.clone().into(),
        size: format_bytes(content.size),
    }
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
    /// One per file with a known language, run once (`Prepared`).
    jobs: Vec<HighlightJob>,
    /// Each line's syntax marks, sorted and non-overlapping, once its
    /// file's job has run. Set from the highlighter's threads while the
    /// document is on screen.
    marks: Box<[OnceLock<Vec<Mark>>]>,
}

struct Numbered {
    id: LineId,
    kind: Kind,
    old_no: Option<u32>,
    new_no: Option<u32>,
    pair: Option<u32>,
}

impl DiffDoc {
    /// A document with nothing in it: the surface before its first diff.
    pub fn empty() -> Self {
        Self {
            files: Vec::new(),
            lines: Vec::new(),
            unified: Layout::new(),
            split: Layout::new(),
            digits: 2,
            added: 0,
            removed: 0,
            jobs: Vec::new(),
            marks: Box::default(),
        }
    }

    /// A line's syntax marks: none until its file has been highlighted.
    pub fn marks(&self, line: LineId) -> &[Mark] {
        self.marks
            .get(line as usize)
            .and_then(OnceLock::get)
            .map_or(&[], Vec::as_slice)
    }

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
            marks: Box::default(),
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
                            "Diff truncated — this file changes more lines than Jiji renders",
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
                        doc.note(
                            "Conflict truncated — this file holds more conflict content than Jiji renders",
                            false,
                        );
                    }
                }
                FileDiffContent::Image { .. } => {
                    doc.push_both(|| Row::Image { file: index as u32 }, false)
                }
                FileDiffContent::Binary { old_size, new_size } => {
                    doc.note(&binary_note(*old_size, *new_size), false)
                }
                FileDiffContent::TooLarge => doc.note("Too large to diff", false),
                FileDiffContent::Omitted => doc.note(
                    "Not shown — this change is larger than Jiji renders at once",
                    false,
                ),
            }
            doc.added += added;
            doc.removed += removed;
            let max_cols = doc.lines[first_line..].iter().map(|line| line.cols).max();
            let mut model = file_model(file, added, removed, max_cols.unwrap_or(0));
            if let FileDiffContent::Image { old, new } = &file.content {
                model.images = Some([old.as_ref().map(image_side), new.as_ref().map(image_side)]);
            }
            model.lines = first_line as LineId..doc.lines.len() as LineId;
            if let FileDiffContent::Conflict { sections, .. } = &file.content {
                model.regions = sections
                    .iter()
                    .filter(|section| matches!(section, ConflictSection::Region { .. }))
                    .count() as u32;
            }
            doc.files.push(model);
            if let Some(job) = job.filter(|job| !job.hunks.is_empty()) {
                doc.jobs.push(job);
            }
        }
        doc.digits = (max_no.max(1).ilog10() + 1).max(2);
        doc.marks = (0..doc.lines.len()).map(|_| OnceLock::new()).collect();
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
        let total = sections
            .iter()
            .filter(|section| matches!(section, ConflictSection::Region { .. }))
            .count();
        let mut ordinal = 0;
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
                    ordinal += 1;
                    let heading = SharedString::from(format!("Conflict {ordinal} of {total}"));
                    self.push_both(
                        || Row::Region {
                            text: heading.clone(),
                            side: None,
                        },
                        false,
                    );
                    for (index, side) in sides.iter().enumerate() {
                        // Two-sided regions order their sides exactly as
                        // the take verbs' "ours" and "theirs".
                        let name = match (sides.len(), index) {
                            (2, 0) => "Side #1 · ours".to_owned(),
                            (2, 1) => "Side #2 · theirs".to_owned(),
                            _ => format!("Side #{}", index + 1),
                        };
                        let caption = match &side.label {
                            Some(label) => format!("vs base · {label}"),
                            None => "vs base".to_owned(),
                        };
                        let (name, caption) =
                            (SharedString::from(name), SharedString::from(caption));
                        self.push_both(
                            || Row::Region {
                                text: name.clone(),
                                side: Some(caption.clone()),
                            },
                            false,
                        );
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

/// A diff as read and laid out, ready for the surface. The cache keeps it
/// and whatever shows it shares it, so a diff shown again is neither read
/// nor built again, and keeps the colours its first showing gave it.
#[derive(Clone)]
pub struct Prepared(Arc<PreparedInner>);

struct PreparedInner {
    diff: Arc<ChangeDiff>,
    doc: Arc<DiffDoc>,
    /// One task per file with a grammar, started the first time the diff
    /// is shown; each is done once its file's marks are in the document.
    highlighting: OnceLock<Vec<Shared<Task<()>>>>,
}

impl Prepared {
    /// Lay `diff` out. It costs what the diff is long: call it off the UI
    /// thread.
    pub fn new(diff: Arc<ChangeDiff>) -> Self {
        let doc = Arc::new(DiffDoc::build(&diff));
        Self(Arc::new(PreparedInner {
            diff,
            doc,
            highlighting: OnceLock::new(),
        }))
    }

    pub fn diff(&self) -> &Arc<ChangeDiff> {
        &self.0.diff
    }

    pub fn doc(&self) -> &Arc<DiffDoc> {
        &self.0.doc
    }

    /// Highlight the diff, once. The first call starts a task per file, in
    /// file order so the top of the diff colours first; every call answers
    /// the same tasks, finished or not.
    pub fn highlighting(&self, executor: &BackgroundExecutor) -> &[Shared<Task<()>>] {
        self.0.highlighting.get_or_init(|| {
            (0..self.0.doc.jobs.len())
                .map(|job| {
                    let doc = self.0.doc.clone();
                    executor
                        .spawn(async move {
                            for (line, marks) in doc.jobs[job].run() {
                                let _ = doc.marks[line as usize].set(marks);
                            }
                        })
                        .shared()
                })
                .collect()
        })
    }
}

fn file_model(file: &FileDiff, added: u32, removed: u32, max_cols: u32) -> FileModel {
    let (dir, name) = match file.path.rfind('/') {
        Some(slash) => file.path.split_at(slash + 1),
        None => ("", file.path.as_str()),
    };
    FileModel {
        path: file.path.clone().into(),
        lines: 0..0,
        regions: 0,
        dir: dir.to_owned().into(),
        name: name.to_owned().into(),
        status: file.status,
        renamed_from: file.renamed_from.clone().map(SharedString::from),
        has_conflict: file.has_conflict,
        added,
        removed,
        max_cols,
        images: None,
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
pub(crate) fn format_bytes(bytes: u64) -> String {
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
    fn an_image_side_says_why_it_has_no_picture() {
        let why = |mime: &str, size: u64, data: Option<&str>| {
            image_side(&jiji_core::snapshot::ImageContent {
                mime: mime.into(),
                size,
                data: data.map(str::to_owned),
            })
            .picture
            .err()
        };
        let cap = jiji_core::jj::MAX_IMAGE_INLINE_BYTES as u64;
        assert_eq!(why("image/png", cap + 1, None), Some(Undrawable::TooLarge));
        // Within the cap and still without bytes: the change's budget ran out.
        assert_eq!(why("image/png", cap, None), Some(Undrawable::OverBudget));
        assert_eq!(
            why("image/jxl", 3, Some("AAAA")),
            Some(Undrawable::Unsupported)
        );
        assert_eq!(
            why("image/png", 3, Some("not base64!")),
            Some(Undrawable::Corrupt)
        );
        // Bytes to decode: whether they draw is for the decoder to say.
        assert_eq!(why("image/heic", 3, Some("AAAA")), None);
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

    // ── Ported from the Tauri app's diff.test.ts ──

    fn hunk(old_start: u32, new_start: u32, lines: Vec<DiffLine>) -> DiffHunk {
        DiffHunk {
            old_start,
            new_start,
            context: None,
            lines,
        }
    }

    fn mixed(kind: DiffLineKind, same: &str, changed: &str, pair: Option<u32>) -> DiffLine {
        DiffLine {
            kind,
            segments: vec![
                DiffSegment {
                    text: same.to_owned(),
                    changed: false,
                },
                DiffSegment {
                    text: changed.to_owned(),
                    changed: true,
                },
            ],
            pair,
        }
    }

    fn file_doc(files: Vec<FileDiff>) -> DiffDoc {
        DiffDoc::build(&ChangeDiff {
            id: "x".into(),
            from: None,
            files,
            truncated: false,
        })
    }

    /// The split layout's line rows as (left, right) texts.
    fn split_texts(doc: &DiffDoc) -> Vec<(Option<String>, Option<String>)> {
        let text = |cell: Option<Cell>| cell.map(|c| doc.lines[c.line as usize].text.to_string());
        doc.split
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Split { left, right } => Some((text(*left), text(*right))),
                _ => None,
            })
            .collect()
    }

    /// The split layout's line rows as (left, right) numbers.
    fn split_numbers(doc: &DiffDoc) -> Vec<(Option<u32>, Option<u32>)> {
        doc.split
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Split { left, right } => Some((left.map(|c| c.no), right.map(|c| c.no))),
                _ => None,
            })
            .collect()
    }

    fn some(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    #[test]
    fn only_a_line_mixing_kept_and_changed_text_gets_intraline_emphasis() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            1,
            1,
            vec![
                mixed(Removed, "let x = ", "1", Some(1)),
                line(Added, "let x = 2", None),
                line(Context, "}", None),
            ],
        )]);
        let emphasized: Vec<bool> = doc.lines.iter().map(|l| !l.changed.is_empty()).collect();
        assert_eq!(emphasized, [true, false, false]);
        assert_eq!(doc.lines[0].changed, [8..9]);
    }

    #[test]
    fn counts_add_up_per_file_and_skip_what_is_not_text() {
        use DiffLineKind::*;
        let text = text_file(
            "src/a.ts",
            vec![
                hunk(
                    1,
                    1,
                    vec![
                        line(Removed, "x", None),
                        line(Added, "x", None),
                        line(Added, "x", None),
                    ],
                ),
                hunk(
                    9,
                    10,
                    vec![line(Context, "x", None), line(Added, "x", None)],
                ),
            ],
        );
        let doc = file_doc(vec![text]);
        assert_eq!((doc.files[0].added, doc.files[0].removed), (3, 1));

        let binary = FileDiff {
            path: "logo.png".into(),
            status: FileStatus::Added,
            renamed_from: None,
            has_conflict: false,
            content: FileDiffContent::Binary {
                old_size: None,
                new_size: Some(2048),
            },
        };
        let one = text_file("src/a.ts", vec![hunk(1, 1, vec![line(Added, "x", None)])]);
        let doc = file_doc(vec![binary, one.clone(), one]);
        assert_eq!((doc.files[0].added, doc.files[0].removed), (0, 0));
        assert_eq!((doc.added, doc.removed), (2, 0));
    }

    #[test]
    fn context_fills_both_sides_with_its_own_numbers() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            10,
            12,
            vec![line(Context, "a", None), line(Context, "b", None)],
        )]);
        assert_eq!(
            split_numbers(&doc),
            [(Some(10), Some(12)), (Some(11), Some(13))]
        );
        assert!(doc.split.rows.iter().all(|row| match row {
            Row::Split { left, right } =>
                left.is_some_and(|c| c.kind == Kind::Context)
                    && right.is_some_and(|c| c.kind == Kind::Context),
            _ => true,
        }));
    }

    #[test]
    fn an_unpaired_removed_run_sits_beside_the_added_run_after_it() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            5,
            5,
            vec![
                line(Context, "c", None),
                line(Removed, "old1", None),
                line(Removed, "old2", None),
                line(Added, "new1", None),
                line(Context, "c", None),
            ],
        )]);
        assert_eq!(
            split_numbers(&doc),
            [
                (Some(5), Some(5)),
                (Some(6), Some(6)),
                (Some(7), None),
                (Some(8), Some(7))
            ]
        );
        assert_eq!(split_texts(&doc)[1], (some("old1"), some("new1")));
        assert_eq!(split_texts(&doc)[2], (some("old2"), None));
    }

    #[test]
    fn a_pure_insertion_leaves_the_old_side_empty() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            3,
            3,
            vec![
                line(Context, "c", None),
                line(Added, "a", None),
                line(Added, "b", None),
            ],
        )]);
        assert_eq!(
            split_numbers(&doc),
            [(Some(3), Some(3)), (None, Some(4)), (None, Some(5))]
        );
    }

    #[test]
    fn runs_never_pair_across_a_context_line() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            1,
            1,
            vec![
                line(Removed, "r", None),
                line(Context, "c", None),
                line(Added, "a", None),
            ],
        )]);
        let kinds: Vec<_> = doc
            .split
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Split { left, right } => Some((left.map(|c| c.kind), right.map(|c| c.kind))),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds,
            [
                (Some(Kind::Removed), None),
                (Some(Kind::Context), Some(Kind::Context)),
                (None, Some(Kind::Added)),
            ]
        );
    }

    #[test]
    fn a_paired_row_keeps_each_side_s_own_emphasis() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            1,
            1,
            vec![
                mixed(Removed, "let x = ", "1", Some(1)),
                line(Added, "let x = 2", Some(0)),
            ],
        )]);
        let rows: Vec<_> = doc
            .split
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Split { left, right } => Some((left.unwrap(), right.unwrap())),
                _ => None,
            })
            .collect();
        assert_eq!(rows.len(), 1);
        let (left, right) = rows[0];
        assert!(!doc.lines[left.line as usize].changed.is_empty());
        assert!(doc.lines[right.line as usize].changed.is_empty());
    }

    #[test]
    fn a_pair_shares_a_row_with_the_unpaired_lines_packed_above_and_below() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            1,
            1,
            vec![
                line(Removed, "// gone comment", None),
                line(Removed, "// more comment", None),
                line(Removed, "unsafe impl Old {}", Some(4)),
                line(Removed, "struct Zzz;", None),
                line(Added, "unsafe impl New {}", Some(2)),
            ],
        )]);
        assert_eq!(
            split_texts(&doc),
            [
                (some("// gone comment"), None),
                (some("// more comment"), None),
                (some("unsafe impl Old {}"), some("unsafe impl New {}")),
                (some("struct Zzz;"), None),
            ]
        );
    }

    #[test]
    fn unpaired_added_lines_pack_before_their_block_s_pair() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            1,
            1,
            vec![
                line(Removed, "old call", Some(3)),
                line(Added, "// why", None),
                line(Added, "// because", None),
                line(Added, "new call", Some(0)),
            ],
        )]);
        assert_eq!(
            split_texts(&doc),
            [
                (None, some("// why")),
                (None, some("// because")),
                (some("old call"), some("new call")),
            ]
        );
    }

    #[test]
    fn several_pairs_align_and_the_stretches_between_them_pack() {
        use DiffLineKind::*;
        let doc = doc(vec![hunk(
            1,
            1,
            vec![
                line(Removed, "a", Some(3)),
                line(Removed, "b", None),
                line(Removed, "c", Some(5)),
                line(Added, "a2", Some(0)),
                line(Added, "mid", None),
                line(Added, "c2", Some(2)),
                line(Added, "tail", None),
            ],
        )]);
        assert_eq!(
            split_texts(&doc),
            [
                (some("a"), some("a2")),
                (some("b"), some("mid")),
                (some("c"), some("c2")),
                (None, some("tail")),
            ]
        );
    }

    /// Two hunks: the first covers new lines 5 to 6, the second starts at
    /// 20, so thirteen unchanged lines lie between them and four above.
    fn two_hunks() -> Vec<DiffHunk> {
        use DiffLineKind::*;
        vec![
            hunk(
                5,
                5,
                vec![
                    line(Context, "c", None),
                    line(Removed, "r", None),
                    line(Added, "a", None),
                ],
            ),
            hunk(20, 20, vec![line(Context, "c", None)]),
        ]
    }

    #[test]
    fn the_gap_rows_count_what_is_skipped_above_and_between_hunks() {
        use DiffLineKind::*;
        let hunks = two_hunks();
        assert_eq!(gap_before(&hunks, 0), Some(4));
        assert_eq!(gap_before(&hunks, 1), Some(13));
        assert_eq!(
            gap_before(&[hunk(1, 1, vec![line(Added, "a", None)])], 0),
            None
        );
        let contiguous = [hunks[0].clone(), hunk(7, 7, vec![line(Context, "c", None)])];
        assert_eq!(gap_before(&contiguous, 1), None);
    }

    /// A layout's rows past the file's header and padding, as the old
    /// tests named them.
    fn shape(layout: &Layout) -> Vec<String> {
        layout
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Gap { skipped, .. } => Some(format!("gap:{skipped}")),
                Row::Line { kind, .. } => Some(format!("{kind:?}").to_lowercase()),
                Row::Split { left, right } => Some(format!(
                    "{}/{}",
                    left.map_or("-".into(), |c| c.no.to_string()),
                    right.map_or("-".into(), |c| c.no.to_string()),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn both_layouts_interleave_gap_rows_with_their_lines() {
        let doc = doc(two_hunks());
        assert_eq!(
            shape(&doc.unified),
            ["gap:4", "context", "removed", "added", "gap:13", "context"]
        );
        assert_eq!(
            shape(&doc.split),
            ["gap:4", "5/5", "6/6", "gap:13", "20/20"]
        );
        // Nothing skipped from line one: no gap row at all.
        let from_the_top = self::doc(vec![hunk(1, 1, vec![line(DiffLineKind::Added, "a", None)])]);
        assert_eq!(shape(&from_the_top.unified), ["added"]);
    }

    #[test]
    fn the_gutter_fits_the_widest_line_number_on_either_side() {
        use DiffLineKind::*;
        assert_eq!(
            doc(vec![hunk(1, 1, vec![line(Added, "a", None)])]).digits,
            2
        );
        let tall = hunk(
            996,
            8,
            vec![
                line(Context, "c", None),
                line(Removed, "r", None),
                line(Context, "c", None),
                line(Context, "c", None),
            ],
        );
        assert_eq!(doc(vec![tall]).digits, 3);
    }

    #[test]
    fn a_file_is_as_wide_as_its_widest_line_with_tabs_at_four() {
        use DiffLineKind::*;
        let widest = doc(vec![
            hunk(
                1,
                1,
                vec![
                    line(Context, "short", None),
                    line(Added, &"a".repeat(40), None),
                ],
            ),
            hunk(9, 9, vec![line(Removed, "\tindented", None)]),
        ]);
        assert_eq!(widest.files[0].max_cols, 40);
        let tabs = doc(vec![hunk(1, 1, vec![line(Added, "\t\tab", None)])]);
        assert_eq!(tabs.files[0].max_cols, 10);
        // A line's segments add up.
        let segmented = doc(vec![hunk(1, 1, vec![mixed(Added, "one", "twotwo", None)])]);
        assert_eq!(segmented.files[0].max_cols, 9);
    }

    #[test]
    fn sizes_read_in_the_unit_that_suits_them() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(847), "847 B");
        assert_eq!(format_bytes(1000), "1 KB");
        assert_eq!(format_bytes(24_140), "24.1 KB");
        assert_eq!(format_bytes(153_600), "154 KB");
        assert_eq!(format_bytes(3_212_000), "3.2 MB");
        assert_eq!(format_bytes(7_400_000_000), "7.4 GB");
    }

    #[test]
    fn a_binary_file_tells_what_its_sizes_say() {
        assert_eq!(binary_note(None, Some(2048)), "Binary file — 2 KB");
        assert_eq!(binary_note(Some(2048), None), "Binary file — was 2 KB");
        assert_eq!(
            binary_note(Some(47_000), Some(47_900)),
            "Binary file — 47 KB → 47.9 KB (+900 B)"
        );
        assert_eq!(
            binary_note(Some(5000), Some(1200)),
            "Binary file — 5 KB → 1.2 KB (−3.8 KB)"
        );
        assert_eq!(binary_note(Some(512), Some(512)), "Binary file — 512 B");
        assert_eq!(binary_note(None, None), "Binary file — no text diff");
    }

    fn conflicted() -> FileDiff {
        use DiffLineKind::*;
        let side = |label: Option<&str>, theirs: &str| jiji_core::snapshot::ConflictSideDiff {
            label: label.map(str::to_owned),
            lines: vec![line(Removed, "base", None), line(Added, theirs, None)],
        };
        FileDiff {
            path: "src/a.ts".into(),
            status: FileStatus::Modified,
            renamed_from: None,
            has_conflict: true,
            content: FileDiffContent::Conflict {
                sections: vec![
                    ConflictSection::Elided { lines: 4 },
                    ConflictSection::Resolved {
                        lines: vec![line(Context, "shared", None)],
                    },
                    ConflictSection::Region {
                        sides: vec![
                            side(None, "ours"),
                            side(Some("feat: theirs"), "a much longer theirs line"),
                        ],
                    },
                    ConflictSection::Resolved {
                        lines: vec![line(Context, "tail", None)],
                    },
                ],
                truncated: false,
            },
        }
    }

    #[test]
    fn a_conflict_reads_in_order_resolved_runs_around_each_side() {
        let doc = file_doc(vec![conflicted()]);
        let read: Vec<String> = doc
            .unified
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Gap { skipped, .. } => Some(format!("gap:{skipped}")),
                Row::Line { line, .. } => Some(doc.lines[*line as usize].text.to_string()),
                Row::Region { text, side, .. } => Some(match side {
                    Some(side) => format!("{text} ({side})"),
                    None => text.to_string(),
                }),
                _ => None,
            })
            .collect();
        assert_eq!(
            read,
            [
                "gap:4",
                "shared",
                "Conflict 1 of 1",
                "Side #1 · ours (vs base)",
                "base",
                "ours",
                "Side #2 · theirs (vs base · feat: theirs)",
                "base",
                "a much longer theirs line",
                "tail",
            ]
        );
        assert_eq!(doc.files[0].regions, 1);
        // The code column is as wide as the widest line of any side.
        assert_eq!(
            doc.files[0].max_cols,
            "a much longer theirs line".len() as u32
        );
    }

    #[gpui::test]
    fn highlighting_a_file_colours_only_its_own_lines(cx: &mut gpui::TestAppContext) {
        use DiffLineKind::*;
        let prepared = Prepared::new(Arc::new(ChangeDiff {
            id: "x".into(),
            from: None,
            files: vec![
                text_file(
                    "src/a.ts",
                    vec![hunk(1, 1, vec![line(Added, "let x = 1;", None)])],
                ),
                text_file(
                    "notes.unknown",
                    vec![hunk(1, 1, vec![line(Added, "let y = 2;", None)])],
                ),
            ],
            truncated: false,
        }));
        let _ = prepared.highlighting(&cx.executor());
        cx.run_until_parked();
        let doc = prepared.doc();
        let (known, foreign) = (doc.files[0].lines.start, doc.files[1].lines.start);
        assert!(!doc.marks(known).is_empty());
        assert!(doc.marks(foreign).is_empty());
    }
}
