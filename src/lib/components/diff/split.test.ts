import { describe, expect, it } from "vitest";
import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffLine } from "$lib/bindings/DiffLine";
import type { DiffLineKind } from "$lib/bindings/DiffLineKind";
import type { FileDiff } from "$lib/bindings/FileDiff";
import {
  canSelectHunks,
  hunkCoords,
  hunkLabel,
  hunkPreview,
  movable,
  selectionReady,
  splitPayload,
  splitSummary,
  splittable,
  toggleFilePick,
  toggleHunkPick,
  type SplitPick,
} from "./split";

function line(kind: DiffLineKind, text = "x"): DiffLine {
  return { kind, segments: [{ text, changed: kind !== "context" }] };
}

function hunk(oldStart: number, newStart: number, lines: DiffLine[]): DiffHunk {
  return { oldStart, newStart, lines };
}

const twoHunks = [
  hunk(1, 1, [line("context"), line("removed", "old"), line("added", "new"), line("context")]),
  hunk(10, 10, [line("context"), line("added", "tail")]),
];

function textFile(path: string, hunks: DiffHunk[], overrides: Partial<FileDiff> = {}): FileDiff {
  return {
    path,
    status: "modified",
    renamedFrom: null,
    hasConflict: false,
    content: { kind: "text", hunks, truncated: false },
    ...overrides,
  };
}

describe("canSelectHunks", () => {
  it("offers hunks only for complete multi-hunk text diffs", () => {
    expect(canSelectHunks(textFile("a.ts", twoHunks))).toBe(true);
    // A single hunk is the whole file; nothing to pick apart.
    expect(canSelectHunks(textFile("a.ts", [twoHunks[0]]))).toBe(false);
    // Materialized conflict markers are not real file content.
    expect(canSelectHunks(textFile("a.ts", twoHunks, { hasConflict: true }))).toBe(false);
    // A budget-trimmed hunk list is not the exact diff the backend will see.
    expect(
      canSelectHunks({
        ...textFile("a.ts", twoHunks),
        content: { kind: "text", hunks: twoHunks, truncated: true },
      }),
    ).toBe(false);
    expect(
      canSelectHunks({ ...textFile("a.bin", []), content: { kind: "binary" } }),
    ).toBe(false);
  });
});

describe("hunk coordinates and labels", () => {
  it("counts context on both sides and derives the unified header", () => {
    expect(hunkCoords(twoHunks[0])).toEqual({
      oldStart: 1,
      newStart: 1,
      oldLines: 3,
      newLines: 3,
    });
    expect(hunkCoords(twoHunks[1])).toEqual({
      oldStart: 10,
      newStart: 10,
      oldLines: 1,
      newLines: 2,
    });
    expect(hunkLabel(twoHunks[1])).toBe("@@ -10,1 +10,2");
    expect(hunkPreview(twoHunks[0])).toBe("- old");
    expect(hunkPreview(twoHunks[1])).toBe("+ tail");
  });
});

describe("selection toggles", () => {
  it("cycles the file pick and normalizes hunk sets", () => {
    expect(toggleFilePick(undefined)).toBe("all");
    expect(toggleFilePick(new Set([0]))).toBe("all");
    expect(toggleFilePick("all")).toBeUndefined();

    // From nothing: pick one hunk.
    const one = toggleHunkPick(undefined, 0, 2);
    expect([...(one as Set<number>)]).toEqual([0]);
    // Completing the set normalizes to the whole-file fast path…
    expect(toggleHunkPick(one, 1, 2)).toBe("all");
    // …and unticking one hunk from "all" materializes the rest.
    const rest = toggleHunkPick("all", 0, 3);
    expect([...(rest as Set<number>)].sort()).toEqual([1, 2]);
    // Emptying the set unchecks the file.
    expect(toggleHunkPick(new Set([1]), 1, 2)).toBeUndefined();
  });
});

describe("splitSummary", () => {
  const files = [textFile("a.ts", twoHunks), textFile("b.ts", [twoHunks[0]])];

  it("requires something checked and something left over", () => {
    expect(splitSummary(files, new Map()).valid).toBe(false);
    expect(splitSummary(null, new Map([["a.ts", "all" as SplitPick]])).valid).toBe(false);

    const partialOnly = splitSummary(files, new Map([["a.ts", new Set([1]) as SplitPick]]));
    expect(partialOnly).toMatchObject({ whole: 0, partial: 1, hunks: 1, valid: true });

    // Every file checked whole → nothing for the remainder.
    const covered = splitSummary(
      files,
      new Map<string, SplitPick>([
        ["a.ts", "all"],
        ["b.ts", "all"],
      ]),
    );
    expect(covered).toMatchObject({ whole: 2, allCovered: true, valid: false });

    // …but a partial file keeps its unchecked hunks on the remainder side.
    const mixed = splitSummary(
      files,
      new Map<string, SplitPick>([
        ["a.ts", new Set([0])],
        ["b.ts", "all"],
      ]),
    );
    expect(mixed).toMatchObject({ whole: 1, partial: 1, allCovered: false, valid: true });
  });
});

describe("splitPayload", () => {
  it("maps whole files to the fast path and hunk indices to coordinates", () => {
    const files = [textFile("a.ts", twoHunks), textFile("b.ts", [twoHunks[0]])];
    const payload = splitPayload(
      files,
      new Map<string, SplitPick>([
        ["b.ts", "all"],
        ["a.ts", new Set([1, 0])],
      ]),
    );
    // File order follows the diff, and indices sort into diff order.
    expect(payload).toEqual([
      {
        path: "a.ts",
        hunks: [
          { oldStart: 1, newStart: 1, oldLines: 3, newLines: 3 },
          { oldStart: 10, newStart: 10, oldLines: 1, newLines: 2 },
        ],
      },
      { path: "b.ts", hunks: null },
    ]);
  });
});

describe("splittable", () => {
  it("admits multi-file changes and single files with hunk granularity", () => {
    expect(splittable([textFile("a.ts", twoHunks), textFile("b.ts", [])])).toBe(true);
    expect(splittable([textFile("a.ts", twoHunks)])).toBe(true);
    expect(splittable([textFile("a.ts", [twoHunks[0]])])).toBe(false);
    expect(splittable([])).toBe(false);
  });
});

describe("selectionReady", () => {
  const files = [textFile("a.ts", twoHunks), textFile("b.ts", [twoHunks[0]])];

  it("a new-change split must leave a remainder", () => {
    const some = splitSummary(files, new Map([["a.ts", "all" as SplitPick]]));
    expect(selectionReady(some, { kind: "new" })).toBe(true);
    expect(selectionReady(some, { kind: "into", id: null })).toBe(false);
    expect(selectionReady(some, { kind: "into", id: "dest" })).toBe(true);
  });

  it("a move into an existing change may take everything, but not nothing", () => {
    const all = splitSummary(
      files,
      new Map<string, SplitPick>([
        ["a.ts", "all"],
        ["b.ts", "all"],
      ]),
    );
    expect(all.allCovered).toBe(true);
    expect(selectionReady(all, { kind: "new" })).toBe(false);
    expect(selectionReady(all, { kind: "into", id: "dest" })).toBe(true);
    expect(selectionReady(splitSummary(files, new Map()), { kind: "into", id: "dest" })).toBe(
      false,
    );
    expect(selectionReady(splitSummary(null, new Map()), { kind: "into", id: "dest" })).toBe(
      false,
    );
  });
});

describe("movable", () => {
  it("needs only one changed file, where a split needs a remainder", () => {
    const single = [textFile("a.ts", [twoHunks[0]])];
    expect(splittable(single)).toBe(false);
    expect(movable(single)).toBe(true);
    expect(movable([])).toBe(false);
  });
});
