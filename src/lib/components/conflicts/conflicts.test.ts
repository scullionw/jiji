import { describe, expect, it } from "vitest";
import type { ConflictItem } from "$lib/bindings/ConflictItem";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { NodeKind } from "$lib/bindings/NodeKind";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import { canResolve, groupConflicts, mergeToolLabel } from "./conflicts";

function node(id: string, kind: NodeKind = "mutable"): GraphNode {
  return {
    id,
    changeId: id,
    commitId: `c-${id}`,
    description: `change ${id}`,
    author: "test",
    timestamp: "2026-06-10T12:00:00Z",
    kind,
    parents: [],
    elidedParents: [],
    bookmarks: [],
    isEmpty: false,
    hasConflict: false,
    isDivergent: false,
  };
}

function item(overrides: Partial<ConflictItem> & { id: string }): ConflictItem {
  return {
    kind: "file",
    summary: `conflict ${overrides.id}`,
    nodeId: null,
    paths: [],
    morePaths: 0,
    targets: [],
    ...overrides,
  };
}

function snapshot(
  conflicts: ConflictItem[],
  nodes: GraphNode[] = [],
  workingCopy = "wc",
  resolveTool: string | null = "smerge",
): RepoSnapshot {
  return {
    repoPath: "/tmp/repo",
    repoName: "repo",
    backend: "mock",
    trunkBookmark: "",
    workingCopy,
    workspaces: [],
    workstreams: [],
    nodes,
    bookmarks: [],
    conflicts,
    operations: [],
    resolveTool,
  };
}

describe("groupConflicts", () => {
  it("splits file conflicts on the working copy from those in commits", () => {
    const snap = snapshot(
      [
        item({ id: "file-a", nodeId: "a" }),
        item({ id: "file-wc", nodeId: "wc" }),
      ],
      [node("wc", "workingCopy"), node("a")],
    );
    const groups = groupConflicts(snap);
    expect(groups.map((g) => g.key)).toEqual(["working-copy", "commits"]);
    expect(groups[0].items.map((i) => i.id)).toEqual(["file-wc"]);
    expect(groups[1].items.map((i) => i.id)).toEqual(["file-a"]);
  });

  it("orders groups fixed and omits empty ones", () => {
    const snap = snapshot([
      item({ id: "workspace-review", kind: "staleWorkspace" }),
      item({ id: "bookmark-x", kind: "bookmark", targets: ["a", "b"] }),
    ]);
    expect(groupConflicts(snap).map((g) => g.key)).toEqual([
      "bookmarks",
      "workspaces",
    ]);
  });

  it("sorts commit conflicts by graph row order, undrawn changes last", () => {
    const snap = snapshot(
      [
        item({ id: "file-hidden", nodeId: "gone" }),
        item({ id: "file-low", nodeId: "low" }),
        item({ id: "file-high", nodeId: "high" }),
      ],
      [node("high"), node("low")],
    );
    const [commits] = groupConflicts(snap);
    expect(commits.key).toBe("commits");
    expect(commits.items.map((i) => i.id)).toEqual([
      "file-high",
      "file-low",
      "file-hidden",
    ]);
  });

  it("returns nothing for a conflict-free snapshot", () => {
    expect(groupConflicts(snapshot([]))).toEqual([]);
  });
});

describe("mergeToolLabel", () => {
  it("prettifies known tools and echoes unknown ones", () => {
    expect(mergeToolLabel("smerge")).toBe("Sublime Merge");
    expect(mergeToolLabel("vscode")).toBe("VS Code");
    expect(mergeToolLabel("meld")).toBe("Meld");
    expect(mergeToolLabel("my-house-tool")).toBe("my-house-tool");
  });
});

describe("canResolve", () => {
  it("requires a tool, a drawn node, and mutability", () => {
    const nodes = [
      node("wc", "workingCopy"),
      node("mut"),
      node("trunk", "immutable"),
    ];
    const snap = snapshot([], nodes);
    expect(canResolve(snap, "wc")).toBe(true);
    expect(canResolve(snap, "mut")).toBe(true);
    expect(canResolve(snap, "trunk")).toBe(false);
    expect(canResolve(snap, "gone")).toBe(false);
    expect(canResolve(snap, null)).toBe(false);
    // No usable merge tool configured: every affordance hides.
    const untooled = snapshot([], nodes, "wc", null);
    expect(canResolve(untooled, "mut")).toBe(false);
  });
});
