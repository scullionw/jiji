import { describe, expect, it } from "vitest";
import type { DiffHunk } from "$lib/bindings/DiffHunk";
import type { DiffLineKind } from "$lib/bindings/DiffLineKind";
import type { FileDiff } from "$lib/bindings/FileDiff";
import { chunkRows, splitLayoutRows, unifiedRows } from "./diff";
import { findInDiff, locateRow, overlayMatches, rowOffsetPx } from "./find";
import type { HighlightSpan } from "./highlight";

function line(kind: DiffLineKind, text: string) {
  return { kind, segments: [{ text, changed: kind !== "context" }], pair: null };
}

function hunk(lines: ReturnType<typeof line>[], starts = [1, 1]): DiffHunk {
  return { oldStart: starts[0], newStart: starts[1], context: null, lines };
}

function textFile(path: string, hunks: DiffHunk[]): FileDiff {
  return {
    path,
    status: "modified",
    renamedFrom: null,
    hasConflict: false,
    content: { kind: "text", hunks, truncated: false },
  };
}

describe("findInDiff", () => {
  const files = [
    textFile("src/alpha.ts", [
      hunk([
        line("context", "const alpha = 1;"),
        line("removed", "needle here, needle there"),
        line("added", "nothing"),
      ]),
    ]),
    textFile("docs/needle.md", [hunk([line("added", "plain text")])]),
  ];

  it("matches lines and paths in reading order, case-insensitively", () => {
    const matches = findInDiff(files, "NEEDLE");
    expect(matches).toEqual([
      { file: 0, hunk: 0, line: 1, start: 0, end: 6 },
      { file: 0, hunk: 0, line: 1, start: 13, end: 19 },
      { file: 1, hunk: -1, line: -1, start: 0, end: 0 },
    ]);
  });

  it("treats the query as a literal, not a regex", () => {
    const tricky = [textFile("a.ts", [hunk([line("added", "value = a+b (sum)")])])];
    expect(findInDiff(tricky, "a+b (sum")).toHaveLength(1);
    expect(findInDiff(tricky, ".*")).toHaveLength(0);
  });

  it("matches the old path of a rename too", () => {
    const renamed: FileDiff = {
      ...textFile("src/new-name.ts", []),
      status: "renamed",
      renamedFrom: "src/old-needle.ts",
    };
    expect(findInDiff([renamed], "needle")).toEqual([
      { file: 0, hunk: -1, line: -1, start: 0, end: 0 },
    ]);
  });

  it("skips non-text content but still matches its path", () => {
    const binary: FileDiff = {
      path: "assets/logo.png",
      status: "modified",
      renamedFrom: null,
      hasConflict: false,
      content: { kind: "binary", oldSize: 1, newSize: 2 },
    };
    expect(findInDiff([binary], "logo")).toHaveLength(1);
    expect(findInDiff([binary], "zzz")).toHaveLength(0);
  });

  it("answers nothing for an empty query", () => {
    expect(findInDiff(files, "")).toEqual([]);
  });
});

describe("overlayMatches", () => {
  const spans: HighlightSpan[] = [
    { text: "let ", changed: false, cls: "syn-kw" },
    { text: "needle", changed: true, cls: null },
    { text: " = 1;", changed: false, cls: null },
  ];

  it("splits spans at match boundaries and keeps their styling", () => {
    // "t need" straddles the first two spans.
    const pieces = overlayMatches(spans, [{ start: 2, end: 8 }], null);
    expect(pieces.map((p) => [p.text, p.hit ?? false])).toEqual([
      ["le", false],
      ["t ", true],
      ["need", true],
      ["le", false],
      [" = 1;", false],
    ]);
    // Styling survives the split.
    expect(pieces[1].cls).toBe("syn-kw");
    expect(pieces[2].changed).toBe(true);
    // Texts concatenate back to the line.
    expect(pieces.map((p) => p.text).join("")).toBe("let needle = 1;");
  });

  it("flags only the range starting at currentStart as current", () => {
    const ranges = [
      { start: 0, end: 3 },
      { start: 4, end: 10 },
    ];
    const pieces = overlayMatches(spans, ranges, 4);
    const current = pieces.filter((p) => p.current);
    expect(current.map((p) => p.text).join("")).toBe("needle");
    const hits = pieces.filter((p) => p.hit && !p.current);
    expect(hits.map((p) => p.text).join("")).toBe("let");
  });
});

describe("locateRow and rowOffsetPx", () => {
  // Two hunks with a gap between them, chunked small to span blocks.
  const hunks = [
    hunk([line("context", "a"), line("added", "b")], [1, 1]),
    hunk([line("context", "c"), line("removed", "target")], [10, 10]),
  ];

  it("finds a line's row in unified blocks by identity", () => {
    const blocks = chunkRows(unifiedRows(hunks), 3);
    const target = hunks[1].lines[1].segments;
    const at = locateRow(blocks, target);
    expect(at).not.toBeNull();
    const row = blocks[at!.block][at!.row];
    expect("segments" in row && row.segments === target).toBe(true);
  });

  it("finds a line's row in split blocks on either side", () => {
    const blocks = chunkRows(splitLayoutRows(hunks), 3);
    const removed = hunks[1].lines[1].segments;
    const at = locateRow(blocks, removed);
    expect(at).not.toBeNull();
    const row = blocks[at!.block][at!.row];
    expect("left" in row && row.left?.segments === removed).toBe(true);
    expect(locateRow(blocks, [])).toBeNull();
  });

  it("prices gap rows at one and a half row heights", () => {
    const rows = unifiedRows(hunks);
    // rows: line, line, gap, line, line — the gap sits at index 2.
    expect("gap" in rows[2]).toBe(true);
    expect(rowOffsetPx(rows, 2, 10)).toBe(20);
    expect(rowOffsetPx(rows, 3, 10)).toBe(35);
    expect(rowOffsetPx(rows, 4, 10)).toBe(45);
  });
});
