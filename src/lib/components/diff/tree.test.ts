import { describe, expect, it } from "vitest";
import type { FileDiff } from "$lib/bindings/FileDiff";
import type { FileStatus } from "$lib/bindings/FileStatus";
import {
  activeRowKey,
  buildTreeRows,
  visibleTreeRows,
  type TreeRow,
} from "./tree";

function file(
  path: string,
  opts: {
    status?: FileStatus;
    added?: number;
    removed?: number;
    conflict?: boolean;
  } = {},
): FileDiff {
  const lines = [
    ...Array.from({ length: opts.added ?? 0 }, () => ({
      kind: "added" as const,
      segments: [{ text: "x", changed: true }],
      pair: null,
    })),
    ...Array.from({ length: opts.removed ?? 0 }, () => ({
      kind: "removed" as const,
      segments: [{ text: "x", changed: true }],
      pair: null,
    })),
  ];
  return {
    path,
    status: opts.status ?? "modified",
    renamedFrom: null,
    hasConflict: opts.conflict ?? false,
    content: {
      kind: "text",
      hunks:
        lines.length > 0
          ? [{ oldStart: 1, newStart: 1, context: null, lines }]
          : [],
      truncated: false,
    },
  };
}

function shape(rows: TreeRow[]): string[] {
  return rows.map(
    (row) =>
      "  ".repeat(row.depth) +
      (row.kind === "dir" ? `${row.label}/` : row.name),
  );
}

describe("buildTreeRows", () => {
  it("nests files under their directories, dirs first, both sorted", () => {
    const rows = buildTreeRows([
      file("zebra.md"),
      file("src/main.rs"),
      file("src/lib.rs"),
      file("alpha.md"),
    ]);
    expect(shape(rows)).toEqual([
      "src/",
      "  lib.rs",
      "  main.rs",
      "alpha.md",
      "zebra.md",
    ]);
  });

  it("keeps each file's index into the diff's own order", () => {
    const rows = buildTreeRows([file("b.ts"), file("a.ts")]);
    const files = rows.filter((r) => r.kind === "file");
    expect(files.map((f) => [f.name, f.index])).toEqual([
      ["a.ts", 1],
      ["b.ts", 0],
    ]);
  });

  it("compacts single-child directory chains into one row", () => {
    const rows = buildTreeRows([
      file("src/lib/components/Button.svelte"),
      file("src/lib/components/Icon.svelte"),
    ]);
    expect(shape(rows)).toEqual([
      "src/lib/components/",
      "  Button.svelte",
      "  Icon.svelte",
    ]);
    expect(rows[0]).toMatchObject({ path: "src/lib/components", depth: 0 });
  });

  it("stops compacting where the tree branches or holds a file", () => {
    const rows = buildTreeRows([
      file("src/app/one.ts"),
      file("src/util/two.ts"),
      file("pkg/mod.rs"),
      file("pkg/deep/three.rs"),
    ]);
    expect(shape(rows)).toEqual([
      "pkg/",
      "  deep/",
      "    three.rs",
      "  mod.rs",
      "src/",
      "  app/",
      "    one.ts",
      "  util/",
      "    two.ts",
    ]);
  });

  it("aggregates counts, stats, and conflicts onto directory rows", () => {
    const rows = buildTreeRows([
      file("src/a.ts", { added: 3, removed: 1 }),
      file("src/deep/b.ts", { added: 2, conflict: true }),
      file("root.md", { added: 1 }),
    ]);
    const src = rows.find((r) => r.kind === "dir" && r.path === "src");
    expect(src).toMatchObject({
      fileCount: 2,
      stats: { added: 5, removed: 1 },
      hasConflict: true,
    });
  });
});

describe("visibleTreeRows", () => {
  const rows = buildTreeRows([
    file("a/one.ts"),
    file("a/b/two.ts"),
    file("c/three.ts"),
    file("root.md"),
  ]);
  // a/ · a/b/ · two.ts · one.ts · c/ · three.ts · root.md

  it("shows everything when nothing is collapsed", () => {
    expect(visibleTreeRows(rows, new Set())).toEqual(rows);
  });

  it("drops a collapsed directory's subtree but keeps its row", () => {
    const visible = visibleTreeRows(rows, new Set(["a"]));
    expect(shape(visible)).toEqual(["a/", "c/", "  three.ts", "root.md"]);
  });

  it("hides a collapsed directory nested inside a collapsed one", () => {
    const visible = visibleTreeRows(rows, new Set(["a", "a/b"]));
    expect(shape(visible)).toEqual(["a/", "c/", "  three.ts", "root.md"]);
  });

  it("collapsing a nested directory keeps its siblings", () => {
    const visible = visibleTreeRows(rows, new Set(["a/b"]));
    expect(shape(visible)).toEqual([
      "a/",
      "  b/",
      "  one.ts",
      "c/",
      "  three.ts",
      "root.md",
    ]);
  });
});

describe("activeRowKey", () => {
  const rows = buildTreeRows([file("a/b/two.ts"), file("root.md")]);

  it("is the file itself while its ancestors are expanded", () => {
    expect(activeRowKey(rows, new Set(), "a/b/two.ts")).toBe("a/b/two.ts");
  });

  it("falls back to the shallowest collapsed ancestor", () => {
    // "a/b" compacts into a single row keyed by its full path.
    expect(activeRowKey(rows, new Set(["a/b"]), "a/b/two.ts")).toBe("a/b");
  });

  it("ignores collapsed directories that are not ancestors", () => {
    expect(activeRowKey(rows, new Set(["a/b"]), "root.md")).toBe("root.md");
  });
});
