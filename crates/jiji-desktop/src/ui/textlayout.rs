//! Text laid out for the pointer: the visual rows of each paragraph, where
//! a byte offset's caret sits, which offset a point lands on, and the boxes
//! a range covers. The text field and the selectable label share it.

use std::ops::Range;

use gpui::{point, px, Bounds, Pixels, Point, SharedString, TextRun, Window, WrappedLine};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) struct Layout {
    pub bounds: Bounds<Pixels>,
    pub line_height: Pixels,
    pub lines: Vec<LaidLine>,
    pub scroll: Point<Pixels>,
    pub content_height: Pixels,
}

pub(crate) struct LaidLine {
    /// Byte offset of this paragraph in the display text.
    pub start: usize,
    /// Offset of its top from the content origin.
    pub top: Pixels,
    pub line: WrappedLine,
    /// Byte offsets (paragraph-relative) where each visual row begins.
    pub rows: Vec<usize>,
}

impl LaidLine {
    pub fn row_of(&self, index: usize) -> usize {
        match self.rows.binary_search(&index) {
            Ok(row) => row,
            Err(row) => row.saturating_sub(1),
        }
    }

    pub fn row_end(&self, row: usize) -> usize {
        self.rows.get(row + 1).copied().unwrap_or(self.line.len())
    }

    /// X of a paragraph-relative byte offset within its visual row.
    pub fn x_in_row(&self, index: usize, row: usize) -> Pixels {
        let layout = &self.line.unwrapped_layout;
        layout.x_for_index(index) - layout.x_for_index(self.rows[row])
    }

    pub fn height(&self, line_height: Pixels) -> Pixels {
        line_height * self.rows.len() as f32
    }
}

impl Layout {
    fn line_for(&self, index: usize) -> usize {
        match self.lines.binary_search_by(|line| line.start.cmp(&index)) {
            Ok(ix) => ix,
            Err(ix) => ix.saturating_sub(1),
        }
    }

    /// Content-space position of a display byte offset's caret.
    pub fn caret(&self, index: usize) -> Point<Pixels> {
        let Some(line) = self.lines.get(self.line_for(index)) else {
            return Point::default();
        };
        let local = (index - line.start).min(line.line.len());
        let row = line.row_of(local);
        point(
            line.x_in_row(local, row),
            line.top + self.line_height * row as f32,
        )
    }

    /// Display byte offset nearest a content-space position.
    pub fn index_at(&self, position: Point<Pixels>) -> usize {
        let Some(last) = self.lines.last() else {
            return 0;
        };
        if position.y < px(0.) {
            return 0;
        }
        if position.y >= last.top + last.height(self.line_height) {
            return last.start + last.line.len();
        }
        let ix = self
            .lines
            .partition_point(|line| line.top <= position.y)
            .saturating_sub(1);
        let line = &self.lines[ix];
        let row = (((position.y - line.top) / self.line_height) as usize).min(line.rows.len() - 1);
        let layout = &line.line.unwrapped_layout;
        let x = position.x.max(px(0.)) + layout.x_for_index(line.rows[row]);
        let mut local = layout.closest_index_for_x(x);
        // A click past a wrapped row's end belongs to that row, not the
        // start of the next.
        let end = line.row_end(row);
        if local >= end && row + 1 < line.rows.len() {
            local = prev_boundary(&line.line.text, end);
        }
        line.start + local.clamp(line.rows[row], line.line.len())
    }

    /// The boxes `range` covers, one per visual row, placed from `origin`.
    /// A selected line break shows as a sliver past its row's last glyph.
    pub fn boxes(&self, range: Range<usize>, origin: Point<Pixels>) -> Vec<Bounds<Pixels>> {
        let mut boxes = Vec::new();
        if range.is_empty() {
            return boxes;
        }
        let line_height = self.line_height;
        for line in &self.lines {
            let end = line.start + line.line.len();
            if range.end < line.start || range.start > end {
                continue;
            }
            let from = range.start.max(line.start) - line.start;
            let to = range.end.min(end) - line.start;
            for row in line.row_of(from)..=line.row_of(to) {
                let row_start = line.rows[row];
                let row_end = line.row_end(row);
                let a = from.max(row_start);
                let b = to.min(row_end);
                if a > b {
                    continue;
                }
                let mut right = line.x_in_row(b, row);
                if (b == row_end && to > row_end) || (b == line.line.len() && range.end > end) {
                    right += px(4.);
                }
                let left = line.x_in_row(a, row);
                if right <= left {
                    continue;
                }
                boxes.push(Bounds::from_corners(
                    origin + point(left, line.top + line_height * row as f32),
                    origin + point(right, line.top + line_height * (row + 1) as f32),
                ));
            }
        }
        boxes
    }
}

/// Shape the display text and locate each paragraph's visual rows.
pub(crate) fn lay_out(
    text: SharedString,
    runs: &[TextRun],
    font_size: Pixels,
    wrap_width: Option<Pixels>,
    line_height: Pixels,
    window: &mut Window,
) -> (Vec<LaidLine>, Pixels) {
    let shaped = window
        .text_system()
        .shape_text(text, font_size, runs, wrap_width, None)
        .unwrap_or_default();
    let mut lines = Vec::with_capacity(shaped.len());
    let mut start = 0;
    let mut top = px(0.);
    for line in shaped {
        let mut rows = vec![0];
        for boundary in line.wrap_boundaries() {
            let glyph = &line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
            rows.push(glyph.index);
        }
        let len = line.len();
        let laid = LaidLine {
            start,
            top,
            line,
            rows,
        };
        top += laid.height(line_height);
        start += len + 1;
        lines.push(laid);
    }
    (lines, top)
}

pub(crate) fn prev_boundary(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(ix, _)| ix)
}

pub(crate) fn next_boundary(text: &str, offset: usize) -> usize {
    text[offset.min(text.len())..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(text.len(), |(ix, _)| offset + ix)
}

fn is_word(piece: &str) -> bool {
    piece.chars().any(|c| c.is_alphanumeric() || c == '_')
}

pub(crate) fn prev_word(text: &str, offset: usize) -> usize {
    text[..offset]
        .split_word_bound_indices()
        .rev()
        .find(|(_, piece)| is_word(piece))
        .map_or(0, |(ix, _)| ix)
}

pub(crate) fn next_word(text: &str, offset: usize) -> usize {
    text[offset..]
        .split_word_bound_indices()
        .find(|(_, piece)| is_word(piece))
        .map_or(text.len(), |(ix, piece)| offset + ix + piece.len())
}

/// The word (or the run of space or punctuation) a double-click at
/// `offset` takes.
pub(crate) fn word_range(text: &str, offset: usize) -> Range<usize> {
    for (start, piece) in text.split_word_bound_indices() {
        let end = start + piece.len();
        if offset < end || end == text.len() {
            return start..end;
        }
    }
    0..0
}

/// The line a triple-click at `offset` takes, without its break.
pub(crate) fn line_range(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind('\n').map_or(0, |ix| ix + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |ix| offset + ix);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_boundaries_step_over_clusters() {
        let text = "ae\u{301}🇨🇦z";
        assert_eq!(next_boundary(text, 0), 1);
        assert_eq!(next_boundary(text, 1), 4);
        assert_eq!(next_boundary(text, 4), 12);
        assert_eq!(prev_boundary(text, 12), 4);
        assert_eq!(prev_boundary(text, 0), 0);
        assert_eq!(next_boundary(text, text.len()), text.len());
    }

    #[test]
    fn word_motion_skips_punctuation_and_space() {
        let text = "fix(graph): lane colors";
        assert_eq!(next_word(text, 0), 3);
        assert_eq!(next_word(text, 3), 9);
        assert_eq!(next_word(text, 9), 16);
        assert_eq!(prev_word(text, text.len()), 17);
        assert_eq!(prev_word(text, 12), 4);
        assert_eq!(prev_word(text, 2), 0);
    }

    #[test]
    fn double_click_selects_the_word_under_the_pointer() {
        let text = "push main now";
        assert_eq!(word_range(text, 6), 5..9);
        assert_eq!(word_range(text, 0), 0..4);
        assert_eq!(word_range(text, text.len()), 10..13);
    }

    #[test]
    fn triple_click_selects_the_line_under_the_pointer() {
        let text = "Summary\n\nFirst line\nsecond";
        assert_eq!(line_range(text, 3), 0..7);
        assert_eq!(line_range(text, 8), 8..8);
        assert_eq!(line_range(text, 12), 9..19);
        assert_eq!(line_range(text, text.len()), 20..26);
    }
}
