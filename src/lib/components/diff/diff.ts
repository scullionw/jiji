// Pure shaping logic for the continuous multi-file diff: line numbering,
// stats, hunk gaps, and gutter sizing. No Svelte.

import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffLineKind } from "$lib/bindings/DiffLineKind";
import type { DiffSegment } from "$lib/bindings/DiffSegment";
import type { FileDiff } from "$lib/bindings/FileDiff";

export interface NumberedLine {
  kind: DiffLineKind;
  oldNo: number | null;
  newNo: number | null;
  segments: DiffSegment[];
  /// Emphasize changed segments only when the line also has unchanged
  /// ones; a fully added/removed line reads better as a flat tint.
  intraline: boolean;
}

// Hunks carry only their starting line numbers; per-line numbers follow
// from the line kinds (context advances both sides, removed only the old
// side, added only the new side).
export function numberLines(hunk: DiffHunk): NumberedLine[] {
  let oldNo = hunk.oldStart;
  let newNo = hunk.newStart;
  return hunk.lines.map((line) => {
    const intraline =
      line.segments.some((s) => s.changed) &&
      line.segments.some((s) => !s.changed);
    const base = { kind: line.kind, segments: line.segments, intraline };
    switch (line.kind) {
      case "context":
        return { ...base, oldNo: oldNo++, newNo: newNo++ };
      case "removed":
        return { ...base, oldNo: oldNo++, newNo: null };
      case "added":
        return { ...base, oldNo: null, newNo: newNo++ };
    }
  });
}

export type DiffLayout = "unified" | "split";

export interface SplitCell {
  no: number;
  kind: DiffLineKind;
  segments: DiffSegment[];
  intraline: boolean;
}

/// One aligned side-by-side row: old text on the left, new on the right.
/// Context fills both sides; an unpaired add/remove leaves the other null.
export interface SplitRow {
  left: SplitCell | null;
  right: SplitCell | null;
}

// Pairs each removed run with the added run that follows it, so replacement
// lines sit opposite each other; the longer run's tail rows keep one side
// absent.
export function splitRows(hunk: DiffHunk): SplitRow[] {
  const rows: SplitRow[] = [];
  const removed: SplitCell[] = [];
  const added: SplitCell[] = [];
  function flush() {
    for (let i = 0; i < Math.max(removed.length, added.length); i++) {
      rows.push({ left: removed[i] ?? null, right: added[i] ?? null });
    }
    removed.length = 0;
    added.length = 0;
  }
  for (const line of numberLines(hunk)) {
    const cell = {
      kind: line.kind,
      segments: line.segments,
      intraline: line.intraline,
    };
    if (line.kind === "removed") {
      removed.push({ ...cell, no: line.oldNo! });
    } else if (line.kind === "added") {
      added.push({ ...cell, no: line.newNo! });
    } else {
      flush();
      rows.push({
        left: { ...cell, no: line.oldNo! },
        right: { ...cell, no: line.newNo! },
      });
    }
  }
  flush();
  return rows;
}

/// One row of a rendered diff body: a real line, or the "N unchanged lines"
/// divider standing in for an elided gap. Every row renders at exactly one
/// row height, so a run of rows has a height the renderer can compute
/// without laying it out — that arithmetic is what lets off-screen blocks
/// collapse to fixed-height placeholders (see FileDiffCard).
export type UnifiedRow = NumberedLine | { gap: number };
export type SplitLayoutRow = SplitRow | { gap: number };

export function unifiedRows(hunks: DiffHunk[]): UnifiedRow[] {
  const rows: UnifiedRow[] = [];
  hunks.forEach((hunk, index) => {
    const gap = gapBefore(hunks, index);
    if (gap !== null) rows.push({ gap });
    rows.push(...numberLines(hunk));
  });
  return rows;
}

export function splitLayoutRows(hunks: DiffHunk[]): SplitLayoutRow[] {
  const rows: SplitLayoutRow[] = [];
  hunks.forEach((hunk, index) => {
    const gap = gapBefore(hunks, index);
    if (gap !== null) rows.push({ gap });
    rows.push(...splitRows(hunk));
  });
  return rows;
}

/// Slice rows into equal render blocks (the last one may run short).
export function chunkRows<T>(rows: T[], size: number): T[][] {
  const blocks: T[][] = [];
  for (let i = 0; i < rows.length; i += size) {
    blocks.push(rows.slice(i, i + size));
  }
  return blocks;
}

// East Asian Wide / Fullwidth blocks plus emoji and the supplementary
// planes: glyphs a monospace font sets at two cells.
function isWide(cp: number): boolean {
  return (
    (cp >= 0x1100 && cp <= 0x115f) ||
    (cp >= 0x2e80 && cp <= 0xa4cf) ||
    (cp >= 0xac00 && cp <= 0xd7a3) ||
    (cp >= 0xf900 && cp <= 0xfaff) ||
    (cp >= 0xfe30 && cp <= 0xfe4f) ||
    (cp >= 0xff00 && cp <= 0xff60) ||
    (cp >= 0xffe0 && cp <= 0xffe6) ||
    cp >= 0x1f000
  );
}

/// Rendered cell count of one line: tab stops every 4 (matching tab-size),
/// double-width glyphs counted as two. An estimate in `ch` units — it sizes
/// the code column up front so the layout never depends on max-content
/// measurement across every row of a large diff.
export function lineCols(segments: DiffSegment[]): number {
  let cols = 0;
  for (const segment of segments) {
    for (const char of segment.text) {
      const cp = char.codePointAt(0)!;
      if (cp === 0x09) cols += 4 - (cols % 4);
      else cols += isWide(cp) ? 2 : 1;
    }
  }
  return cols;
}

/// Widest line of a file in cells, for pre-sizing the code column.
export function maxLineCols(hunks: DiffHunk[]): number {
  let max = 0;
  for (const hunk of hunks) {
    for (const line of hunk.lines) {
      max = Math.max(max, lineCols(line.segments));
    }
  }
  return max;
}

export interface DiffStats {
  added: number;
  removed: number;
}

export function fileStats(file: FileDiff): DiffStats {
  const stats = { added: 0, removed: 0 };
  if (file.content.kind !== "text") return stats;
  for (const hunk of file.content.hunks) {
    for (const line of hunk.lines) {
      if (line.kind === "added") stats.added++;
      else if (line.kind === "removed") stats.removed++;
    }
  }
  return stats;
}

export function totalStats(files: FileDiff[]): DiffStats {
  const total = { added: 0, removed: 0 };
  for (const file of files) {
    const stats = fileStats(file);
    total.added += stats.added;
    total.removed += stats.removed;
  }
  return total;
}

/// Unchanged lines skipped before `hunks[index]`: above the first hunk, or
/// between it and its predecessor. null when nothing was skipped.
export function gapBefore(hunks: DiffHunk[], index: number): number | null {
  const start = hunks[index].newStart;
  if (index === 0) return start > 1 ? start - 1 : null;
  const prev = hunks[index - 1];
  const prevNewLines = prev.lines.filter((l) => l.kind !== "removed").length;
  const skipped = start - (prev.newStart + prevNewLines);
  return skipped > 0 ? skipped : null;
}

/// Characters needed by the widest line number in one file, for sizing the
/// gutter columns.
export function gutterDigits(hunks: DiffHunk[]): number {
  let max = 1;
  for (const hunk of hunks) {
    const oldLines = hunk.lines.filter((l) => l.kind !== "added").length;
    const newLines = hunk.lines.filter((l) => l.kind !== "removed").length;
    max = Math.max(
      max,
      hunk.oldStart + oldLines - 1,
      hunk.newStart + newLines - 1,
    );
  }
  return Math.max(2, String(max).length);
}
