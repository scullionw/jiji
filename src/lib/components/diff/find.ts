// Pure matching for the diff pane's find bar (⌘F): where a query occurs
// in one rendered diff — file paths and hunk lines — plus the helpers the
// windowed renderer needs to decorate and reveal a match. No Svelte.

import type { DiffSegment } from "$lib/bindings/DiffSegment";
import type { FileDiff } from "$lib/bindings/FileDiff";
import type { HighlightSpan } from "./highlight";
import type { SplitLayoutRow, UnifiedRow } from "./diff";

export interface FindMatch {
  /// Index into the diff's files.
  file: number;
  /// Hunk index within the file; -1 for a path (file header) match, whose
  /// line/start/end are meaningless.
  hunk: number;
  /// Line index within the hunk's lines.
  line: number;
  /// Character range of the match within the line's text.
  start: number;
  end: number;
}

/// The query as a case-insensitive literal matcher. A regex with the
/// literal escaped keeps offsets honest — JS `toLowerCase` can change
/// string length (İ), which would drift highlight ranges.
function literalMatcher(query: string): RegExp {
  return new RegExp(query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi");
}

/// Every occurrence of `query` in the diff, in reading order: each file's
/// path first (the header is a match surface too — this is the filename
/// search), then its lines top to bottom. Case-insensitive literal.
export function findInDiff(files: FileDiff[], query: string): FindMatch[] {
  const matches: FindMatch[] = [];
  if (query.length === 0) return matches;
  const matcher = literalMatcher(query);
  const test = (text: string) => {
    matcher.lastIndex = 0;
    return matcher.test(text);
  };
  files.forEach((file, fileIndex) => {
    if (test(file.path) || (file.renamedFrom !== null && test(file.renamedFrom))) {
      matches.push({ file: fileIndex, hunk: -1, line: -1, start: 0, end: 0 });
    }
    if (file.content.kind !== "text") return;
    file.content.hunks.forEach((hunk, hunkIndex) => {
      hunk.lines.forEach((line, lineIndex) => {
        let text = "";
        for (const segment of line.segments) text += segment.text;
        matcher.lastIndex = 0;
        for (let m = matcher.exec(text); m !== null; m = matcher.exec(text)) {
          matches.push({
            file: fileIndex,
            hunk: hunkIndex,
            line: lineIndex,
            start: m.index,
            end: m.index + m[0].length,
          });
          // A zero-length query is excluded above, but never spin.
          if (m[0].length === 0) matcher.lastIndex++;
        }
      });
    });
  });
  return matches;
}

/// A highlight span with the find overlay applied. Plain spans pass
/// through with the flags absent, so undecorated lines cost nothing.
export interface FindSpan extends HighlightSpan {
  hit?: boolean;
  current?: boolean;
}

/// Split one line's highlight spans at the match boundaries so the
/// renderer can wrap each matched stretch. Texts concatenate back to the
/// line; `changed`/`cls` follow the spans, `hit` the ranges, and the
/// range starting at `currentStart` is flagged as the active match.
export function overlayMatches(
  spans: HighlightSpan[],
  ranges: { start: number; end: number }[],
  currentStart: number | null,
): FindSpan[] {
  const sorted = [...ranges].sort((a, b) => a.start - b.start);
  const out: FindSpan[] = [];
  let pos = 0;
  let ri = 0;
  for (const span of spans) {
    const end = pos + span.text.length;
    let cur = pos;
    while (cur < end) {
      while (ri < sorted.length && sorted[ri].end <= cur) ri++;
      const range = ri < sorted.length ? sorted[ri] : null;
      let stop: number;
      let hit = false;
      if (!range || range.start >= end) {
        stop = end;
      } else if (range.start > cur) {
        stop = range.start;
      } else {
        stop = Math.min(range.end, end);
        hit = true;
      }
      out.push({
        text: span.text.slice(cur - pos, stop - pos),
        changed: span.changed,
        cls: span.cls,
        hit,
        current: hit && range !== null && range.start === currentStart,
      });
      cur = stop;
    }
    pos = end;
  }
  return out;
}

type AnyRow = UnifiedRow | SplitLayoutRow;

/// Where one line renders in a chunked row layout, found by segments-array
/// identity — rows carry the same segment arrays the hunks own, in both
/// layouts. null when the line is not in these blocks.
export function locateRow(
  blocks: AnyRow[][],
  segments: DiffSegment[],
): { block: number; row: number } | null {
  for (let b = 0; b < blocks.length; b++) {
    const rows = blocks[b];
    for (let r = 0; r < rows.length; r++) {
      const row = rows[r];
      if ("gap" in row) continue;
      if ("segments" in row) {
        if (row.segments === segments) return { block: b, row: r };
      } else if (
        row.left?.segments === segments ||
        row.right?.segments === segments
      ) {
        return { block: b, row: r };
      }
    }
  }
  return null;
}

/// Pixel offset of `index` within its block, given the row height: line
/// rows are one row height, gap bands one and a half — FileDiffCard's
/// exact geometry, valid without mounting because lines never wrap.
export function rowOffsetPx(rows: AnyRow[], index: number, rowHeight: number): number {
  let gaps = 0;
  for (let i = 0; i < index && i < rows.length; i++) {
    if ("gap" in rows[i]) gaps++;
  }
  return ((index - gaps) + gaps * 1.5) * rowHeight;
}
