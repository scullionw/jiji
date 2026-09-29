import { describe, expect, it } from "vitest";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { NodeKind } from "$lib/bindings/NodeKind";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";
import { extendRange, rangeCompareFrom, validRange } from "./selection";

function node(
  id: string,
  kind: NodeKind,
  parents: string[] = [],
): GraphNode {
  return {
    id,
    changeId: id,
    commitId: `c-${id}`,
    description: `change ${id}`,
    author: "test",
    timestamp: "2026-06-10T12:00:00Z",
    kind,
    parents,
    elidedParents: [],
    bookmarks: [],
    tags: [],
    isEmpty: false,
    hasConflict: false,
    isDivergent: false,
  };
}

function workstream(
  id: string,
  nodeIds: string[],
  opts: Partial<WorkstreamSummary> = {},
): WorkstreamSummary {
  return {
    id,
    title: id,
    nodeIds,
    bookmark: null,
    isActive: false,
    behindTrunk: 0,
    ...opts,
  };
}

function snapshot(
  nodes: GraphNode[],
  workstreams: WorkstreamSummary[],
  bookmarks: BookmarkState[] = [],
): RepoSnapshot {
  return {
    repoPath: "/tmp/repo",
    repoName: "repo",
    backend: "mock",
    view: { kind: "workstreams" },
    truncated: false,
    trunkBookmark: "",
    workingCopy: nodes.find((n) => n.kind === "workingCopy")?.id ?? "",
    workspaces: [],
    workstreams,
    nodes,
    bookmarks,
    gitRemotes: [],
    conflicts: [],
    operations: [],
    resolveTool: null,
  };
}

// A three-change stack (wc → b → a) on an immutable trunk base, plus a
// sibling one-change stack on the same base.
function stackSnapshot(): RepoSnapshot {
  return snapshot(
    [
      node("wc", "workingCopy", ["b"]),
      node("b", "mutable", ["a"]),
      node("a", "mutable", ["trunk"]),
      node("s1", "mutable", ["trunk"]),
      node("trunk", "immutable", []),
    ],
    [workstream("ws-a", ["wc", "b", "a"], { isActive: true }), workstream("ws-s", ["s1"])],
  );
}

describe("extendRange", () => {
  it("extends downward from the anchor to the target, head-first", () => {
    expect(extendRange(stackSnapshot(), "wc", "a")).toEqual(["wc", "b", "a"]);
  });

  it("extends upward the same way — the ends can be picked in either order", () => {
    expect(extendRange(stackSnapshot(), "a", "wc")).toEqual(["wc", "b", "a"]);
  });

  it("covers a middle-to-end run without touching the rest of the lane", () => {
    expect(extendRange(stackSnapshot(), "b", "a")).toEqual(["b", "a"]);
  });

  it("collapses to a single-id run when anchor and target coincide", () => {
    expect(extendRange(stackSnapshot(), "b", "b")).toEqual(["b"]);
  });

  it("refuses a target in another workstream", () => {
    expect(extendRange(stackSnapshot(), "wc", "s1")).toBeNull();
  });

  it("refuses when either end is outside every workstream (immutable base)", () => {
    expect(extendRange(stackSnapshot(), "wc", "trunk")).toBeNull();
    expect(extendRange(stackSnapshot(), "trunk", "wc")).toBeNull();
  });

  it("refuses when the run's root has no parent to diff from", () => {
    const snap = snapshot(
      [node("wc", "workingCopy", ["a"]), node("a", "mutable", [])],
      [workstream("ws", ["wc", "a"], { isActive: true })],
    );
    expect(extendRange(snap, "wc", "a")).toBeNull();
  });
});

describe("rangeCompareFrom", () => {
  it("answers the root's first parent — the combined diff's base", () => {
    expect(rangeCompareFrom(stackSnapshot(), ["wc", "b", "a"])).toBe("trunk");
    expect(rangeCompareFrom(stackSnapshot(), ["wc", "b"])).toBe("a");
  });

  it("uses the first parent of a merge root", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["m"]),
        node("m", "mutable", ["a", "other"]),
        node("a", "immutable", []),
        node("other", "immutable", []),
      ],
      [workstream("ws", ["wc", "m"], { isActive: true })],
    );
    expect(rangeCompareFrom(snap, ["wc", "m"])).toBe("a");
  });

  it("is null when the head or root is not drawn in the snapshot", () => {
    expect(rangeCompareFrom(stackSnapshot(), ["ghost", "a"])).toBeNull();
    expect(rangeCompareFrom(stackSnapshot(), ["wc", "ghost"])).toBeNull();
    expect(rangeCompareFrom(stackSnapshot(), [])).toBeNull();
  });
});

describe("validRange", () => {
  it("holds for an unchanged contiguous run", () => {
    expect(validRange(stackSnapshot(), ["wc", "b", "a"])).toBe(true);
    expect(validRange(stackSnapshot(), ["b", "a"])).toBe(true);
  });

  it("never holds for fewer than two ids", () => {
    expect(validRange(stackSnapshot(), ["wc"])).toBe(false);
    expect(validRange(stackSnapshot(), [])).toBe(false);
  });

  it("drops when a member left the lane (abandoned mid-run)", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["a"]),
        node("a", "mutable", ["trunk"]),
        node("trunk", "immutable", []),
      ],
      [workstream("ws-a", ["wc", "a"], { isActive: true })],
    );
    expect(validRange(snap, ["wc", "b", "a"])).toBe(false);
  });

  it("drops when the lane was reordered underneath it", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["a"]),
        node("a", "mutable", ["b"]),
        node("b", "mutable", ["trunk"]),
        node("trunk", "immutable", []),
      ],
      [workstream("ws-a", ["wc", "a", "b"], { isActive: true })],
    );
    expect(validRange(snap, ["wc", "b", "a"])).toBe(false);
  });

  it("drops when the head no longer belongs to any workstream", () => {
    expect(validRange(stackSnapshot(), ["trunk", "a"])).toBe(false);
  });
});
