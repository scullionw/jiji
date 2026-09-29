// Pure selection logic for the split panel: which files and hunks are
// checked, what that means for the two halves, and the payload the backend
// verifies. No Svelte.

import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffLine } from "$lib/bindings/DiffLine";
import type { FileDiff } from "$lib/bindings/FileDiff";
import type { SplitHunk } from "$lib/bindings/SplitHunk";
import type { SplitSelection } from "$lib/bindings/SplitSelection";

/// One hunk's checked state within a partially checked file: the whole
/// hunk, or a set of its picked line indices — indices into the hunk's
/// rendered lines, the coordinate space `SplitHunk.lines` speaks. Only
/// added/removed lines are pickable; context carries no change.
export type HunkPick = "all" | ReadonlySet<number>;

/// One file's checked state: the whole file, or a map of hunk indices
/// into its rendered diff to their picks. Absent from the map =
/// unchecked. The toggles normalize upward — a line set covering every
/// changed line becomes the whole hunk, a map of every hunk checked whole
/// becomes the whole file — so a map-valued pick is always genuinely
/// partial.
export type SplitPick = "all" | ReadonlyMap<number, HunkPick>;

// Hunk-level selection only makes sense where the rendered hunks are the
// file's complete, exact diff: text content, no conflict markers
// materialized into it, nothing trimmed by the diff budget — and at least
// two hunks, because checking a file's only hunk is the whole file.
export function canSelectHunks(file: FileDiff): boolean {
  return (
    file.content.kind === "text" &&
    !file.hasConflict &&
    !file.content.truncated &&
    file.content.hunks.length >= 2
  );
}

export function fileHunks(file: FileDiff): DiffHunk[] {
  return file.content.kind === "text" ? file.content.hunks : [];
}

/// The pickable lines: indices of the hunk's added/removed rows, in the
/// index space the backend verifies (context rows count but cannot be
/// picked).
export function changedLineIndices(hunk: DiffHunk): number[] {
  const out: number[] = [];
  hunk.lines.forEach((line, index) => {
    if (line.kind !== "context") out.push(index);
  });
  return out;
}

// Line-level selection only makes sense inside a hunk with at least two
// changed lines — picking a hunk's only change is the whole hunk.
export function canSelectLines(hunk: DiffHunk): boolean {
  return changedLineIndices(hunk).length >= 2;
}

// The coordinates the backend re-derives and verifies: 1-based starts from
// the hunk itself, per-side line counts from its line kinds (context
// counts on both sides). `lines: null` means the whole hunk; the split
// panel's per-line checkboxes (G1.3b) will narrow it to picked indices.
export function hunkCoords(hunk: DiffHunk): SplitHunk {
  return {
    oldStart: hunk.oldStart,
    newStart: hunk.newStart,
    oldLines: hunk.lines.filter((l) => l.kind !== "added").length,
    newLines: hunk.lines.filter((l) => l.kind !== "removed").length,
    lines: null,
  };
}

/// The unified header for a hunk row: `@@ -12,7 +12,9`.
export function hunkLabel(hunk: DiffHunk): string {
  const c = hunkCoords(hunk);
  return `@@ -${c.oldStart},${c.oldLines} +${c.newStart},${c.newLines}`;
}

export function hunkStats(hunk: DiffHunk): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const line of hunk.lines) {
    if (line.kind === "added") added += 1;
    else if (line.kind === "removed") removed += 1;
  }
  return { added, removed };
}

/// The first changed line, as a scan hint on the hunk row.
export function hunkPreview(hunk: DiffHunk): string {
  const line = hunk.lines.find((l) => l.kind !== "context");
  if (!line) return "";
  return `${line.kind === "added" ? "+" : "-"} ${lineText(line).trimStart()}`;
}

/// A line's full text, for the per-line checkbox rows.
export function lineText(line: DiffLine): string {
  return line.segments.map((s) => s.text).join("");
}

// Toggling the file row: anything less than a full check completes to the
// whole file; a full check clears.
export function toggleFilePick(current: SplitPick | undefined): SplitPick | undefined {
  return current === "all" ? undefined : "all";
}

// A file pick as an explicit, mutable hunk map — "all" spelled out as
// every hunk checked whole.
function materializePick(
  current: SplitPick | undefined,
  hunkCount: number,
): Map<number, HunkPick> {
  if (current === "all") {
    return new Map(Array.from({ length: hunkCount }, (_, i) => [i, "all"]));
  }
  return new Map(current ?? []);
}

// The inverse: an emptied map unchecks the file; every hunk checked whole
// normalizes back to "all" — the fast path, and what keeps
// every-file-fully-checked detectable as the degenerate selection.
function normalizePick(
  map: Map<number, HunkPick>,
  hunkCount: number,
): SplitPick | undefined {
  if (map.size === 0) return undefined;
  if (map.size === hunkCount && [...map.values()].every((pick) => pick === "all")) {
    return "all";
  }
  return map;
}

// Toggling one hunk checkbox. Like the file row, anything less than a
// full check — including a partial line pick — completes to the whole
// hunk; a full check clears it.
export function toggleHunkPick(
  current: SplitPick | undefined,
  index: number,
  hunkCount: number,
): SplitPick | undefined {
  const map = materializePick(current, hunkCount);
  if (map.get(index) === "all") map.delete(index);
  else map.set(index, "all");
  return normalizePick(map, hunkCount);
}

// Toggling one line checkbox. A whole-hunk pick first materializes into
// the explicit set of its changed lines; an emptied set unchecks the
// hunk; a set covering every changed line normalizes back to the whole
// hunk (`SplitHunk.lines: null`), keeping the payload's fast paths.
export function toggleLinePick(
  current: SplitPick | undefined,
  hunks: readonly DiffHunk[],
  hunkIndex: number,
  lineIndex: number,
): SplitPick | undefined {
  const map = materializePick(current, hunks.length);
  const changed = changedLineIndices(hunks[hunkIndex]);
  const pick = map.get(hunkIndex);
  const set = new Set<number>(pick === "all" ? changed : (pick ?? []));
  if (set.has(lineIndex)) set.delete(lineIndex);
  else set.add(lineIndex);
  if (set.size === 0) map.delete(hunkIndex);
  else if (set.size === changed.length) map.set(hunkIndex, "all");
  else map.set(hunkIndex, set);
  return normalizePick(map, hunks.length);
}

export interface SplitSummary {
  /// Files checked whole.
  whole: number;
  /// Files with a subset of their hunks checked.
  partial: number;
  /// Hunks checked whole across the partial files.
  hunks: number;
  /// Single lines checked across the partial files' line-picked hunks.
  lines: number;
  /// Every changed file is checked whole — nothing left for the remainder.
  allCovered: boolean;
  valid: boolean;
}

export function splitSummary(
  files: FileDiff[] | null,
  selection: ReadonlyMap<string, SplitPick>,
): SplitSummary {
  let whole = 0;
  let partial = 0;
  let hunks = 0;
  let lines = 0;
  for (const file of files ?? []) {
    const pick = selection.get(file.path);
    if (pick === undefined) continue;
    if (pick === "all") {
      whole += 1;
    } else {
      partial += 1;
      for (const hunkPick of pick.values()) {
        if (hunkPick === "all") hunks += 1;
        else lines += hunkPick.size;
      }
    }
  }
  const allCovered = files !== null && files.length > 0 && whole === files.length;
  return {
    whole,
    partial,
    hunks,
    lines,
    allCovered,
    valid: files !== null && whole + partial > 0 && !allCovered,
  };
}

// The payload the backend verifies. Hunk indices resolve to coordinates
// against the same rendered hunks the checkboxes showed; a line-picked
// hunk narrows its coordinates to the picked indices.
export function splitPayload(
  files: FileDiff[],
  selection: ReadonlyMap<string, SplitPick>,
): SplitSelection[] {
  const out: SplitSelection[] = [];
  for (const file of files) {
    const pick = selection.get(file.path);
    if (pick === undefined) continue;
    if (pick === "all") {
      out.push({ path: file.path, hunks: null });
    } else {
      const all = fileHunks(file);
      out.push({
        path: file.path,
        hunks: [...pick.keys()]
          .sort((a, b) => a - b)
          .map((i) => {
            const coords = hunkCoords(all[i]);
            const hunkPick = pick.get(i)!;
            if (hunkPick !== "all") {
              coords.lines = [...hunkPick].sort((a, b) => a - b);
            }
            return coords;
          }),
      });
    }
  }
  return out;
}

// Where the checked selection goes: carved into a new described change
// (jj split — the panel's default), or moved into an existing change
// (jj squash --from --into).
export type SplitDestination = { kind: "new" } | { kind: "into"; id: string | null };

// Whether the current selection can go. A new-change split must leave a
// remainder behind; a move into an existing change needs a chosen
// destination and may take everything — the emptied change is abandoned.
export function selectionReady(
  summary: SplitSummary,
  dest: SplitDestination,
): boolean {
  if (dest.kind === "new") return summary.valid;
  return summary.whole + summary.partial > 0 && dest.id !== null;
}

// Whether this change can be split at all: two files, or one file whose
// diff offers hunk granularity.
export function splittable(files: FileDiff[]): boolean {
  return files.length >= 2 || (files.length === 1 && canSelectHunks(files[0]));
}

// Moving into an existing change only needs something to move and somewhere
// to put it — a one-file change qualifies even where a split would not.
export function movable(files: FileDiff[]): boolean {
  return files.length >= 1;
}
