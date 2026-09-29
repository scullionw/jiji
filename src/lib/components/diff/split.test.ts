import { describe, expect, it } from "vitest";
import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffLine } from "$lib/bindings/DiffLine";
import type { DiffLineKind } from "$lib/bindings/DiffLineKind";
import type { FileDiff } from "$lib/bindings/FileDiff";
import {
  canSelectHunks,
  canSelectLines,
  changedLineIndices,
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
  toggleLinePick,
  type HunkPick,
  type SplitPick,
} from "./split";

function line(kind: DiffLineKind, text = "x"): DiffLine {
  return { kind, segments: [{ text, changed: kind !== "context" }], pair: null };
}

function hunk(oldStart: number, newStart: number, lines: DiffLine[]): DiffHunk {
  return { oldStart, newStart, context: null, lines };
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
      canSelectHunks({
        ...textFile("a.bin", []),
        content: { kind: "binary", oldSize: null, newSize: 16 },
      }),
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
      lines: null,
    });
    expect(hunkCoords(twoHunks[1])).toEqual({
      oldStart: 10,
      newStart: 10,
      oldLines: 1,
      newLines: 2,
      lines: null,
    });
    expect(hunkLabel(twoHunks[1])).toBe("@@ -10,1 +10,2");
    expect(hunkPreview(twoHunks[0])).toBe("- old");
    expect(hunkPreview(twoHunks[1])).toBe("+ tail");
  });
});

// Readable literal for a map-valued pick.
function picks(entries: [number, HunkPick][]): SplitPick {
  return new Map(entries);
}

describe("selection toggles", () => {
  it("cycles the file pick and normalizes hunk maps", () => {
    expect(toggleFilePick(undefined)).toBe("all");
    expect(toggleFilePick(picks([[0, "all"]]))).toBe("all");
    expect(toggleFilePick("all")).toBeUndefined();

    // From nothing: pick one hunk.
    const one = toggleHunkPick(undefined, 0, 2);
    expect(one).toEqual(picks([[0, "all"]]));
    // Completing the map normalizes to the whole-file fast path…
    expect(toggleHunkPick(one, 1, 2)).toBe("all");
    // …and unticking one hunk from "all" materializes the rest.
    expect(toggleHunkPick("all", 0, 3)).toEqual(
      picks([
        [1, "all"],
        [2, "all"],
      ]),
    );
    // Emptying the map unchecks the file.
    expect(toggleHunkPick(picks([[1, "all"]]), 1, 2)).toBeUndefined();
    // A line-picked hunk completes to whole, like the file row does.
    expect(toggleHunkPick(picks([[0, new Set([1])]]), 0, 2)).toEqual(
      picks([[0, "all"]]),
    );
  });

  it("cycles line picks and normalizes through hunk and file", () => {
    // twoHunks[0] renders [context, removed, added, context]: picks 1, 2.
    expect(changedLineIndices(twoHunks[0])).toEqual([1, 2]);

    // From nothing: pick one line.
    const one = toggleLinePick(undefined, twoHunks, 0, 1);
    expect(one).toEqual(picks([[0, new Set([1])]]));
    // Completing the hunk's changed lines normalizes to the whole hunk…
    expect(toggleLinePick(one, twoHunks, 0, 2)).toEqual(picks([[0, "all"]]));
    // …and completing every hunk normalizes to the whole file.
    expect(
      toggleLinePick(picks([[0, new Set([1])], [1, "all"]]), twoHunks, 0, 2),
    ).toBe("all");
    // Unticking a line from "all" materializes the rest of the file.
    expect(toggleLinePick("all", twoHunks, 0, 2)).toEqual(
      picks([
        [0, new Set([1])],
        [1, "all"],
      ]),
    );
    // Emptying the hunk's line set unchecks it; the last hunk, the file.
    expect(toggleLinePick(one, twoHunks, 0, 1)).toBeUndefined();
    expect(toggleLinePick(picks([[0, new Set([1])], [1, "all"]]), twoHunks, 0, 1)).toEqual(
      picks([[1, "all"]]),
    );
  });
});

describe("canSelectLines", () => {
  it("offers lines only where a hunk has two or more changed lines", () => {
    expect(canSelectLines(twoHunks[0])).toBe(true);
    // A single changed line is the whole hunk; nothing to narrow.
    expect(canSelectLines(twoHunks[1])).toBe(false);
  });
});

describe("splitSummary", () => {
  const files = [textFile("a.ts", twoHunks), textFile("b.ts", [twoHunks[0]])];

  it("requires something checked and something left over", () => {
    expect(splitSummary(files, new Map()).valid).toBe(false);
    expect(splitSummary(null, new Map([["a.ts", "all" as SplitPick]])).valid).toBe(false);

    const partialOnly = splitSummary(files, new Map([["a.ts", picks([[1, "all"]])]]));
    expect(partialOnly).toMatchObject({ whole: 0, partial: 1, hunks: 1, lines: 0, valid: true });

    // Line picks count separately from whole hunks.
    const linePicked = splitSummary(
      files,
      new Map([["a.ts", picks([[0, new Set([1, 2])], [1, "all"]])]]),
    );
    expect(linePicked).toMatchObject({ whole: 0, partial: 1, hunks: 1, lines: 2, valid: true });

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
        ["a.ts", picks([[0, "all"]])],
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
        ["a.ts", picks([[1, "all"], [0, "all"]])],
      ]),
    );
    // File order follows the diff, and indices sort into diff order.
    expect(payload).toEqual([
      {
        path: "a.ts",
        hunks: [
          { oldStart: 1, newStart: 1, oldLines: 3, newLines: 3, lines: null },
          { oldStart: 10, newStart: 10, oldLines: 1, newLines: 2, lines: null },
        ],
      },
      { path: "b.ts", hunks: null },
    ]);
  });

  it("narrows line-picked hunks to their sorted indices beside whole picks", () => {
    const files = [textFile("a.ts", twoHunks), textFile("b.ts", [twoHunks[0]])];
    const payload = splitPayload(
      files,
      new Map<string, SplitPick>([
        ["a.ts", picks([[1, "all"], [0, new Set([2, 1])]])],
        ["b.ts", "all"],
      ]),
    );
    expect(payload).toEqual([
      {
        path: "a.ts",
        hunks: [
          // The line-picked hunk keeps its coordinates and adds the picks…
          { oldStart: 1, newStart: 1, oldLines: 3, newLines: 3, lines: [1, 2] },
          // …the whole-checked hunk stays on the lines: null fast path.
          { oldStart: 10, newStart: 10, oldLines: 1, newLines: 2, lines: null },
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
