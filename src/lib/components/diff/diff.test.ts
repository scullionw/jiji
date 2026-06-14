import { describe, expect, it } from "vitest";
import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffLine } from "$lib/bindings/DiffLine";
import type { DiffLineKind } from "$lib/bindings/DiffLineKind";
import type { FileDiff } from "$lib/bindings/FileDiff";
import {
  chunkRows,
  fileStats,
  gapBefore,
  gutterDigits,
  lineCols,
  maxLineCols,
  numberLines,
  splitLayoutRows,
  splitRows,
  totalStats,
  unifiedRows,
} from "./diff";

function line(kind: DiffLineKind, text = "x"): DiffLine {
  return { kind, segments: [{ text, changed: kind !== "context" }] };
}

function hunk(
  oldStart: number,
  newStart: number,
  lines: DiffLine[],
): DiffHunk {
  return { oldStart, newStart, lines };
}

function textFile(hunks: DiffHunk[]): FileDiff {
  return {
    path: "src/a.ts",
    status: "modified",
    renamedFrom: null,
    hasConflict: false,
    content: { kind: "text", hunks, truncated: false },
  };
}

describe("numberLines", () => {
  it("advances the right side counters per line kind", () => {
    const numbered = numberLines(
      hunk(10, 12, [
        line("context"),
        line("removed"),
        line("removed"),
        line("added"),
        line("context"),
      ]),
    );
    expect(
      numbered.map((l) => [l.oldNo, l.newNo]),
    ).toEqual([
      [10, 12],
      [11, null],
      [12, null],
      [null, 13],
      [13, 14],
    ]);
  });

  it("marks intraline emphasis only for mixed-segment lines", () => {
    const mixed: DiffLine = {
      kind: "removed",
      segments: [
        { text: "let x = ", changed: false },
        { text: "1", changed: true },
      ],
    };
    const flat = line("added", "let x = 2");
    const context = line("context", "}");
    const numbered = numberLines(hunk(1, 1, [mixed, flat, context]));
    expect(numbered.map((l) => l.intraline)).toEqual([true, false, false]);
  });
});

describe("stats", () => {
  it("counts added and removed lines across hunks", () => {
    const file = textFile([
      hunk(1, 1, [line("removed"), line("added"), line("added")]),
      hunk(9, 10, [line("context"), line("added")]),
    ]);
    expect(fileStats(file)).toEqual({ added: 3, removed: 1 });
  });

  it("treats non-text content as zero and sums totals", () => {
    const binary: FileDiff = {
      path: "logo.png",
      status: "added",
      renamedFrom: null,
      hasConflict: false,
      content: { kind: "binary" },
    };
    const file = textFile([hunk(1, 1, [line("added")])]);
    expect(fileStats(binary)).toEqual({ added: 0, removed: 0 });
    expect(totalStats([binary, file, file])).toEqual({ added: 2, removed: 0 });
  });
});

describe("splitRows", () => {
  it("fills both sides for context lines with their own numbers", () => {
    const rows = splitRows(hunk(10, 12, [line("context"), line("context")]));
    expect(rows.map((r) => [r.left?.no, r.right?.no])).toEqual([
      [10, 12],
      [11, 13],
    ]);
    expect(rows.every((r) => r.left?.kind === "context")).toBe(true);
  });

  it("pairs a removed run with the added run that follows it", () => {
    const rows = splitRows(
      hunk(5, 5, [
        line("context"),
        line("removed", "old1"),
        line("removed", "old2"),
        line("added", "new1"),
        line("context"),
      ]),
    );
    expect(
      rows.map((r) => [r.left?.no ?? null, r.right?.no ?? null]),
    ).toEqual([
      [5, 5],
      [6, 6],
      [7, null],
      [8, 7],
    ]);
    expect(rows[1].left?.segments[0].text).toBe("old1");
    expect(rows[1].right?.segments[0].text).toBe("new1");
    expect(rows[2].right).toBeNull();
  });

  it("keeps the left side absent for a pure insertion", () => {
    const rows = splitRows(
      hunk(3, 3, [line("context"), line("added"), line("added")]),
    );
    expect(rows.map((r) => [r.left?.no ?? null, r.right?.no ?? null])).toEqual([
      [3, 3],
      [null, 4],
      [null, 5],
    ]);
  });

  it("does not pair runs across a context line", () => {
    const rows = splitRows(
      hunk(1, 1, [line("removed"), line("context"), line("added")]),
    );
    expect(rows.map((r) => [r.left?.kind ?? null, r.right?.kind ?? null])).toEqual([
      ["removed", null],
      ["context", "context"],
      [null, "added"],
    ]);
  });

  it("carries intraline emphasis onto both paired cells", () => {
    const mixed: DiffLine = {
      kind: "removed",
      segments: [
        { text: "let x = ", changed: false },
        { text: "1", changed: true },
      ],
    };
    const rows = splitRows(hunk(1, 1, [mixed, line("added", "let x = 2")]));
    expect(rows).toHaveLength(1);
    expect(rows[0].left?.intraline).toBe(true);
    expect(rows[0].right?.intraline).toBe(false);
  });
});

describe("gapBefore", () => {
  const hunks = [
    hunk(5, 5, [line("context"), line("removed"), line("added")]),
    // previous hunk covers new lines 5..7 (context + added), so a hunk
    // starting at 8 is contiguous and one at 20 skips 12 lines.
    hunk(20, 20, [line("context")]),
  ];

  it("reports lines elided above the first hunk", () => {
    expect(gapBefore(hunks, 0)).toBe(4);
    expect(gapBefore([hunk(1, 1, [line("added")])], 0)).toBeNull();
  });

  it("reports lines elided between hunks on the new side", () => {
    expect(gapBefore(hunks, 1)).toBe(13);
    const contiguous = [hunks[0], hunk(7, 7, [line("context")])];
    expect(gapBefore(contiguous, 1)).toBeNull();
  });
});

describe("row models", () => {
  const hunks = [
    hunk(5, 5, [line("context"), line("removed"), line("added")]),
    hunk(20, 20, [line("context")]),
  ];

  it("interleaves gap rows with numbered lines for the unified layout", () => {
    const rows = unifiedRows(hunks);
    expect(rows.map((r) => ("gap" in r ? `gap:${r.gap}` : r.kind))).toEqual([
      "gap:4",
      "context",
      "removed",
      "added",
      "gap:13",
      "context",
    ]);
  });

  it("interleaves gap rows with paired rows for the split layout", () => {
    const rows = splitLayoutRows(hunks);
    expect(
      rows.map((r) => ("gap" in r ? `gap:${r.gap}` : [r.left?.no ?? null, r.right?.no ?? null])),
    ).toEqual(["gap:4", [5, 5], [6, 6], "gap:13", [20, 20]]);
  });

  it("omits gap rows when hunks are contiguous from line one", () => {
    expect(unifiedRows([hunk(1, 1, [line("added")])])).toHaveLength(1);
  });
});

describe("chunkRows", () => {
  it("slices rows into blocks with a short tail", () => {
    expect(chunkRows([1, 2, 3, 4, 5], 2)).toEqual([[1, 2], [3, 4], [5]]);
    expect(chunkRows([], 2)).toEqual([]);
  });
});

describe("lineCols", () => {
  const seg = (text: string) => [{ text, changed: false }];

  it("counts plain characters once", () => {
    expect(lineCols(seg("let x = 1;"))).toBe(10);
  });

  it("advances tabs to the next 4-column stop", () => {
    expect(lineCols(seg("\tx"))).toBe(5);
    expect(lineCols(seg("ab\tx"))).toBe(5);
    expect(lineCols(seg("abcd\tx"))).toBe(9);
  });

  it("counts wide glyphs as two cells across segment boundaries", () => {
    expect(lineCols([{ text: "名", changed: false }, { text: "前", changed: true }])).toBe(4);
  });

  it("reports the widest line of a file", () => {
    expect(
      maxLineCols([hunk(1, 1, [line("added", "xx"), line("context", "wider line")])]),
    ).toBe(10);
  });
});

describe("gutterDigits", () => {
  it("sizes to the widest line number on either side", () => {
    expect(gutterDigits([hunk(1, 1, [line("added")])])).toBe(2);
    const tall = hunk(
      996,
      8,
      [line("context"), line("removed"), line("context"), line("context")],
    );
    expect(gutterDigits([tall])).toBe(3);
  });
});
