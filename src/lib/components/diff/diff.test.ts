import { describe, expect, it } from "vitest";
import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffLine } from "$lib/bindings/DiffLine";
import type { DiffLineKind } from "$lib/bindings/DiffLineKind";
import type { FileDiff } from "$lib/bindings/FileDiff";
import type { ConflictSection } from "$lib/bindings/ConflictSection";
import {
  binaryNote,
  chunkRows,
  conflictHunks,
  fileStats,
  formatBytes,
  gapBefore,
  gutterDigits,
  hunkAt,
  lineIndexAt,
  maxLineCh,
  numberLines,
  splitLayoutRows,
  splitRows,
  totalStats,
  unifiedPatch,
  unifiedRows,
} from "./diff";

function line(kind: DiffLineKind, text = "x", pair: number | null = null): DiffLine {
  return { kind, segments: [{ text, changed: kind !== "context" }], pair };
}

function hunk(
  oldStart: number,
  newStart: number,
  lines: DiffLine[],
): DiffHunk {
  return { oldStart, newStart, context: null, lines };
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
      pair: 1,
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
      content: { kind: "binary", oldSize: null, newSize: 2048 },
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
      pair: 1,
    };
    const rows = splitRows(hunk(1, 1, [mixed, line("added", "let x = 2", 0)]));
    expect(rows).toHaveLength(1);
    expect(rows[0].left?.intraline).toBe(true);
    expect(rows[0].right?.intraline).toBe(false);
  });

  it("anchors a pair on one row, packing unpaired lines above and below", () => {
    // Four removed lines, one added; the third removed line pairs with
    // it. The pair must share a row, the rest of the block staying
    // left-only — never the positional top-alignment.
    const rows = splitRows(
      hunk(1, 1, [
        line("removed", "// gone comment"),
        line("removed", "// more comment"),
        line("removed", "unsafe impl Old {}", 4),
        line("removed", "struct Zzz;"),
        line("added", "unsafe impl New {}", 2),
      ]),
    );
    expect(
      rows.map((r) => [r.left?.segments[0].text ?? null, r.right?.segments[0].text ?? null]),
    ).toEqual([
      ["// gone comment", null],
      ["// more comment", null],
      ["unsafe impl Old {}", "unsafe impl New {}"],
      ["struct Zzz;", null],
    ]);
  });

  it("packs unpaired added lines before their block's pair", () => {
    // One removed line whose replacement is the last of three added
    // lines (two new comments precede it): comments pack right-only,
    // then the pair aligns.
    const rows = splitRows(
      hunk(1, 1, [
        line("removed", "old call", 3),
        line("added", "// why"),
        line("added", "// because"),
        line("added", "new call", 0),
      ]),
    );
    expect(
      rows.map((r) => [r.left?.segments[0].text ?? null, r.right?.segments[0].text ?? null]),
    ).toEqual([
      [null, "// why"],
      [null, "// because"],
      ["old call", "new call"],
    ]);
  });

  it("aligns multiple anchors and packs the stretches between them", () => {
    const rows = splitRows(
      hunk(1, 1, [
        line("removed", "a", 3),
        line("removed", "b"),
        line("removed", "c", 5),
        line("added", "a2", 0),
        line("added", "mid"),
        line("added", "c2", 2),
        line("added", "tail"),
      ]),
    );
    expect(
      rows.map((r) => [r.left?.segments[0].text ?? null, r.right?.segments[0].text ?? null]),
    ).toEqual([
      ["a", "a2"],
      ["b", "mid"],
      ["c", "c2"],
      [null, "tail"],
    ]);
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

describe("maxLineCh", () => {
  it("finds the widest line across hunks, counting tabs at width 4", () => {
    const hunks = [
      hunk(1, 1, [line("context", "short"), line("added", "a".repeat(40))]),
      hunk(9, 9, [line("removed", "\tindented")]),
    ];
    expect(maxLineCh(hunks)).toBe(40);
    expect(maxLineCh([hunk(1, 1, [line("added", "\t\tab")])])).toBe(10);
  });

  it("sums a line's segments", () => {
    const segmented: DiffLine = {
      kind: "added",
      segments: [
        { text: "one", changed: false },
        { text: "twotwo", changed: true },
      ],
      pair: null,
    };
    expect(maxLineCh([hunk(1, 1, [segmented])])).toBe(9);
  });
});

describe("hunkAt", () => {
  const hunks = [
    hunk(1, 1, [line("context"), line("added")]),
    hunk(10, 11, [line("removed"), line("context")]),
  ];

  it("finds the hunk covering a line on either side", () => {
    expect(hunkAt(hunks, { old: null, new: 2 })).toBe(hunks[0]);
    expect(hunkAt(hunks, { old: 10, new: null })).toBe(hunks[1]);
  });

  it("answers null between hunks", () => {
    expect(hunkAt(hunks, { old: 5, new: 5 })).toBeNull();
  });
});

describe("lineIndexAt", () => {
  // Rendered as old 10 / new 12: [context, removed@10…11, added@13, context].
  const mixed = hunk(10, 12, [
    line("context"),
    line("removed"),
    line("removed"),
    line("added"),
    line("context"),
  ]);

  it("resolves a changed row to its index in the hunk's rendered lines", () => {
    expect(lineIndexAt(mixed, { old: 11, new: null })).toBe(1);
    expect(lineIndexAt(mixed, { old: 12, new: null })).toBe(2);
    expect(lineIndexAt(mixed, { old: null, new: 13 })).toBe(3);
  });

  it("answers null on context rows and outside the hunk", () => {
    // The leading context line carries both numbers; no change to pick.
    expect(lineIndexAt(mixed, { old: 10, new: 12 })).toBeNull();
    expect(lineIndexAt(mixed, { old: null, new: 99 })).toBeNull();
  });
});

describe("unifiedPatch", () => {
  const change = hunk(3, 3, [
    line("context", "keep"),
    line("removed", "before"),
    line("added", "after"),
  ]);

  it("serializes a file's hunks with headers and prefixes", () => {
    expect(unifiedPatch(textFile([change]))).toBe(
      [
        "--- a/src/a.ts",
        "+++ b/src/a.ts",
        "@@ -3,2 +3,2 @@",
        " keep",
        "-before",
        "+after",
        "",
      ].join("\n"),
    );
  });

  it("scopes to one hunk when given, and marks adds/removes with /dev/null", () => {
    const two = textFile([change, hunk(10, 10, [line("added", "tail")])]);
    expect(unifiedPatch(two, two.content.kind === "text" ? two.content.hunks[1] : undefined))
      .toContain("@@ -10,0 +10,1 @@");

    const added = { ...textFile([change]), status: "added" as const };
    expect(unifiedPatch(added)).toContain("--- /dev/null");
    const removed = { ...textFile([change]), status: "removed" as const };
    expect(unifiedPatch(removed)).toContain("+++ /dev/null");
  });

  it("uses the rename source as the old name and is empty for non-text", () => {
    const renamed = { ...textFile([change]), renamedFrom: "src/old.ts" };
    expect(unifiedPatch(renamed)).toContain("--- a/src/old.ts");
    const binary: FileDiff = {
      path: "img.png",
      status: "modified",
      renamedFrom: null,
      hasConflict: false,
      content: { kind: "binary", oldSize: 100, newSize: 200 },
    };
    expect(unifiedPatch(binary)).toBe("");
  });
});

describe("formatBytes", () => {
  it("picks the unit and trims to a readable precision", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(847)).toBe("847 B");
    expect(formatBytes(1000)).toBe("1 KB");
    expect(formatBytes(24_140)).toBe("24.1 KB");
    expect(formatBytes(153_600)).toBe("154 KB");
    expect(formatBytes(3_212_000)).toBe("3.2 MB");
    expect(formatBytes(7_400_000_000)).toBe("7.4 GB");
  });
});

describe("binaryNote", () => {
  it("tells the size story for each add/remove/modify shape", () => {
    expect(binaryNote({ oldSize: null, newSize: 2048 })).toBe(
      "Binary file — 2 KB",
    );
    expect(binaryNote({ oldSize: 2048, newSize: null })).toBe(
      "Binary file — was 2 KB",
    );
    expect(binaryNote({ oldSize: 47_000, newSize: 47_900 })).toBe(
      "Binary file — 47 KB → 47.9 KB (+900 B)",
    );
    expect(binaryNote({ oldSize: 5000, newSize: 1200 })).toBe(
      "Binary file — 5 KB → 1.2 KB (−3.8 KB)",
    );
    expect(binaryNote({ oldSize: 512, newSize: 512 })).toBe(
      "Binary file — 512 B",
    );
    expect(binaryNote({ oldSize: null, newSize: null })).toBe(
      "Binary file — no text diff",
    );
  });
});

describe("conflictHunks", () => {
  const sections: ConflictSection[] = [
    { kind: "elided", lines: 4 },
    { kind: "resolved", lines: [line("context", "shared")] },
    {
      kind: "region",
      sides: [
        { label: null, lines: [line("removed", "base"), line("added", "ours")] },
        {
          label: "feat: theirs",
          lines: [line("removed", "base"), line("added", "a much longer theirs line")],
        },
      ],
    },
    { kind: "resolved", lines: [line("context", "tail")] },
  ];

  it("flattens resolved runs and region sides into render-ordered hunks", () => {
    const hunks = conflictHunks(sections);
    expect(hunks).toHaveLength(4);
    // Same arrays by reference — the identity the highlight cache keys on.
    expect(hunks[0].lines).toBe(
      (sections[1] as Extract<ConflictSection, { kind: "resolved" }>).lines,
    );
    expect(hunks[1].lines[1].segments[0].text).toBe("ours");
    expect(hunks[2].lines[1].segments[0].text).toBe(
      "a much longer theirs line",
    );
    expect(hunks[3].lines[0].segments[0].text).toBe("tail");
  });

  it("sizes the shared code column off every side's lines", () => {
    expect(maxLineCh(conflictHunks(sections))).toBe(
      "a much longer theirs line".length,
    );
  });
});
