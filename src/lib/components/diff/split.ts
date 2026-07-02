// Pure selection logic for the split panel: which files and hunks are
// checked, what that means for the two halves, and the payload the backend
// verifies. No Svelte.

import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { FileDiff } from "$lib/bindings/FileDiff";
import type { SplitHunk } from "$lib/bindings/SplitHunk";
import type { SplitSelection } from "$lib/bindings/SplitSelection";

/// One file's checked state: the whole file, or a set of hunk indices into
/// its rendered diff. Absent from the map = unchecked.
export type SplitPick = "all" | ReadonlySet<number>;

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

// The coordinates the backend re-derives and verifies: 1-based starts from
// the hunk itself, per-side line counts from its line kinds (context
// counts on both sides).
export function hunkCoords(hunk: DiffHunk): SplitHunk {
  return {
    oldStart: hunk.oldStart,
    newStart: hunk.newStart,
    oldLines: hunk.lines.filter((l) => l.kind !== "added").length,
    newLines: hunk.lines.filter((l) => l.kind !== "removed").length,
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
  const text = line.segments.map((s) => s.text).join("");
  return `${line.kind === "added" ? "+" : "-"} ${text.trimStart()}`;
}

// Toggling the file row: anything less than a full check completes to the
// whole file; a full check clears.
export function toggleFilePick(current: SplitPick | undefined): SplitPick | undefined {
  return current === "all" ? undefined : "all";
}

// Toggling one hunk checkbox. "all" first materializes into the explicit
// index set; an emptied set unchecks the file; a completed set normalizes
// back to "all" — the fast path, and what keeps every-file-fully-checked
// detectable as the degenerate selection.
export function toggleHunkPick(
  current: SplitPick | undefined,
  index: number,
  hunkCount: number,
): SplitPick | undefined {
  const set = new Set<number>(
    current === "all" ? Array.from({ length: hunkCount }, (_, i) => i) : (current ?? []),
  );
  if (set.has(index)) set.delete(index);
  else set.add(index);
  if (set.size === 0) return undefined;
  if (set.size === hunkCount) return "all";
  return set;
}

export interface SplitSummary {
  /// Files checked whole.
  whole: number;
  /// Files with a subset of their hunks checked.
  partial: number;
  /// Hunks checked across the partial files.
  hunks: number;
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
  for (const file of files ?? []) {
    const pick = selection.get(file.path);
    if (pick === undefined) continue;
    if (pick === "all") {
      whole += 1;
    } else {
      partial += 1;
      hunks += pick.size;
    }
  }
  const allCovered = files !== null && files.length > 0 && whole === files.length;
  return {
    whole,
    partial,
    hunks,
    allCovered,
    valid: files !== null && whole + partial > 0 && !allCovered,
  };
}

// The payload the backend verifies. Hunk indices resolve to coordinates
// against the same rendered hunks the checkboxes showed.
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
        hunks: [...pick].sort((a, b) => a - b).map((i) => hunkCoords(all[i])),
      });
    }
  }
  return out;
}

// Whether this change can be split at all: two files, or one file whose
// diff offers hunk granularity.
export function splittable(files: FileDiff[]): boolean {
  return files.length >= 2 || (files.length === 1 && canSelectHunks(files[0]));
}
