// Pure shaping logic for the continuous multi-file diff: line numbering,
// stats, hunk gaps, and gutter sizing. No Svelte.

import type { ConflictSection } from "$lib/bindings/ConflictSection";
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
  /// This line's index in the hunk plus the backend's similarity-pair
  /// link (also a hunk-line index): what the split layout aligns on.
  index: number;
  pair: number | null;
}

// Hunks carry only their starting line numbers; per-line numbers follow
// from the line kinds (context advances both sides, removed only the old
// side, added only the new side).
export function numberLines(hunk: DiffHunk): NumberedLine[] {
  let oldNo = hunk.oldStart;
  let newNo = hunk.newStart;
  return hunk.lines.map((line, index) => {
    const intraline =
      line.segments.some((s) => s.changed) &&
      line.segments.some((s) => !s.changed);
    const base = {
      kind: line.kind,
      segments: line.segments,
      intraline,
      index,
      pair: line.pair,
    };
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

// Aligns each changed block on the backend's similarity pairs: a paired
// removed/added line shares one row, so what changed reads straight
// across, and the unpaired stretches between anchors pack side by side.
// A block with no pairs degenerates to positional packing.
export function splitRows(hunk: DiffHunk): SplitRow[] {
  const rows: SplitRow[] = [];
  const removed: { cell: SplitCell; index: number; pair: number | null }[] = [];
  const added: { cell: SplitCell; index: number }[] = [];
  function flush() {
    const posOfAdded = new Map(added.map((a, pos) => [a.index, pos]));
    let i = 0;
    let j = 0;
    const pack = (endI: number, endJ: number) => {
      for (let k = 0; k < Math.max(endI - i, endJ - j); k++) {
        rows.push({
          left: i + k < endI ? removed[i + k].cell : null,
          right: j + k < endJ ? added[j + k].cell : null,
        });
      }
      i = endI;
      j = endJ;
    };
    for (let r = 0; r < removed.length; r++) {
      const pair = removed[r].pair;
      if (pair === null) continue;
      const a = posOfAdded.get(pair);
      // Pairs are monotonic by construction; skip any link a truncated
      // hunk or stale cache left dangling rather than misalign.
      if (a === undefined || r < i || a < j) continue;
      pack(r, a);
      rows.push({ left: removed[i++].cell, right: added[j++].cell });
    }
    pack(removed.length, added.length);
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
      removed.push({
        cell: { ...cell, no: line.oldNo! },
        index: line.index,
        pair: line.pair,
      });
    } else if (line.kind === "added") {
      added.push({ cell: { ...cell, no: line.newNo! }, index: line.index });
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

/// One row of a rendered diff body: a real line, or the elision band
/// standing in for a skipped run — the skipped count plus the enclosing
/// declaration of the hunk below (the backend's git-style "funcname"), so
/// a reader landing mid-file knows what they are inside. Lines never wrap
/// (long ones ride the horizontal scroll), so a line row is exactly one
/// row height and a band one and a half — a run of rows has an exact
/// height without laying it out, which sizes the off-screen block
/// placeholders (see FileDiffCard).
export type GapRow = { gap: number; context: string | null };
export type UnifiedRow = NumberedLine | GapRow;
export type SplitLayoutRow = SplitRow | GapRow;

export function unifiedRows(hunks: DiffHunk[]): UnifiedRow[] {
  const rows: UnifiedRow[] = [];
  hunks.forEach((hunk, index) => {
    const gap = gapBefore(hunks, index);
    if (gap !== null) rows.push({ gap, context: hunk.context ?? null });
    rows.push(...numberLines(hunk));
  });
  return rows;
}

export function splitLayoutRows(hunks: DiffHunk[]): SplitLayoutRow[] {
  const rows: SplitLayoutRow[] = [];
  hunks.forEach((hunk, index) => {
    const gap = gapBefore(hunks, index);
    if (gap !== null) rows.push({ gap, context: hunk.context ?? null });
    rows.push(...splitRows(hunk));
  });
  return rows;
}

/// Pseudo-hunks over a conflicted file's sections, in render order, so the
/// conflict body reuses the machinery keyed on hunk lines: the syntax
/// highlighter walks the same segments arrays the rows render, and
/// maxLineCh sizes the shared code column. Coordinates are meaningless for
/// conflict content and stay 0.
export function conflictHunks(sections: ConflictSection[]): DiffHunk[] {
  const hunks: DiffHunk[] = [];
  for (const section of sections) {
    if (section.kind === "resolved") {
      hunks.push({ oldStart: 0, newStart: 0, context: null, lines: section.lines });
    } else if (section.kind === "region") {
      for (const side of section.sides) {
        hunks.push({ oldStart: 0, newStart: 0, context: null, lines: side.lines });
      }
    }
  }
  return hunks;
}

/// Slice rows into equal render blocks (the last one may run short).
export function chunkRows<T>(rows: T[], size: number): T[][] {
  const blocks: T[][] = [];
  for (let i = 0; i < rows.length; i += size) {
    blocks.push(rows.slice(i, i + size));
  }
  return blocks;
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

/// The hunk whose old/new range covers a clicked line, for the context
/// menu's copy-hunk-as-patch. Matches whichever side the row carries.
export function hunkAt(
  hunks: DiffHunk[],
  line: { old: number | null; new: number | null },
): DiffHunk | null {
  for (const hunk of hunks) {
    const oldLines = hunk.lines.filter((l) => l.kind !== "added").length;
    const newLines = hunk.lines.filter((l) => l.kind !== "removed").length;
    const inNew =
      line.new !== null &&
      line.new >= hunk.newStart &&
      line.new < hunk.newStart + newLines;
    const inOld =
      line.old !== null &&
      line.old >= hunk.oldStart &&
      line.old < hunk.oldStart + oldLines;
    if (inNew || inOld) return hunk;
  }
  return null;
}

/// The clicked line's index into its hunk's rendered lines — the
/// coordinate `SplitHunk.lines` speaks — matched on whichever side the
/// row carries. null when the click landed on a context line (no change
/// to pick) or outside the hunk.
export function lineIndexAt(
  hunk: DiffHunk,
  line: { old: number | null; new: number | null },
): number | null {
  let oldNo = hunk.oldStart;
  let newNo = hunk.newStart;
  for (const [index, l] of hunk.lines.entries()) {
    const hit =
      (line.old !== null && l.kind !== "added" && oldNo === line.old) ||
      (line.new !== null && l.kind !== "removed" && newNo === line.new);
    if (hit) return l.kind === "context" ? null : index;
    if (l.kind !== "added") oldNo += 1;
    if (l.kind !== "removed") newNo += 1;
  }
  return null;
}

const PATCH_PREFIX: Record<DiffLineKind, string> = {
  context: " ",
  removed: "-",
  added: "+",
};

function hunkPatchLines(hunk: DiffHunk): string[] {
  const oldCount = hunk.lines.filter((l) => l.kind !== "added").length;
  const newCount = hunk.lines.filter((l) => l.kind !== "removed").length;
  const header = `@@ -${hunk.oldStart},${oldCount} +${hunk.newStart},${newCount} @@`;
  return [
    header,
    ...hunk.lines.map(
      (line) =>
        PATCH_PREFIX[line.kind] + line.segments.map((s) => s.text).join(""),
    ),
  ];
}

/// One file's diff — or a single hunk of it — serialized as a unified
/// patch, for pasting into reviews and chats. Empty for non-text content.
export function unifiedPatch(file: FileDiff, hunk?: DiffHunk): string {
  if (file.content.kind !== "text") return "";
  const hunks = hunk ? [hunk] : file.content.hunks;
  const oldName =
    file.status === "added"
      ? "/dev/null"
      : `a/${file.renamedFrom ?? file.path}`;
  const newName = file.status === "removed" ? "/dev/null" : `b/${file.path}`;
  const lines = [`--- ${oldName}`, `+++ ${newName}`];
  for (const h of hunks) lines.push(...hunkPatchLines(h));
  return lines.join("\n") + "\n";
}

/// Human-readable byte size, decimal units: "847 B", "24.1 KB", "3.2 MB".
export function formatBytes(bytes: number): string {
  if (bytes < 1000) return `${bytes} B`;
  let value = bytes;
  for (const unit of ["KB", "MB", "GB", "TB"]) {
    value /= 1000;
    if (value < 1000 || unit === "TB") {
      const shown =
        value >= 100 ? Math.round(value) : Math.round(value * 10) / 10;
      return `${shown} ${unit}`;
    }
  }
  return `${bytes} B`; // unreachable
}

/// The one line a binary file's diff gets: what the sizes say about the
/// change. A missing side means the file was added or removed.
export function binaryNote(content: {
  oldSize: number | null;
  newSize: number | null;
}): string {
  const { oldSize, newSize } = content;
  if (oldSize !== null && newSize !== null) {
    if (oldSize === newSize) return `Binary file — ${formatBytes(newSize)}`;
    const delta = newSize - oldSize;
    const sign = delta > 0 ? "+" : "−";
    return `Binary file — ${formatBytes(oldSize)} → ${formatBytes(newSize)} (${sign}${formatBytes(Math.abs(delta))})`;
  }
  if (newSize !== null) return `Binary file — ${formatBytes(newSize)}`;
  if (oldSize !== null) return `Binary file — was ${formatBytes(oldSize)}`;
  return "Binary file — no text diff";
}

/// The widest line in one file, in ch (tabs counted at tab-size 4). Lines
/// render without wrapping, so this sizes both layouts' code columns: every
/// block shares one column width and the row fills end at one edge, with
/// the surplus reachable by horizontal scroll.
export function maxLineCh(hunks: DiffHunk[]): number {
  let max = 0;
  for (const hunk of hunks) {
    for (const line of hunk.lines) {
      let width = 0;
      for (const segment of line.segments) {
        const text = segment.text;
        width += text.length;
        for (let i = 0; i < text.length; i++) {
          if (text.charCodeAt(i) === 9) width += 3;
        }
      }
      if (width > max) max = width;
    }
  }
  return max;
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
