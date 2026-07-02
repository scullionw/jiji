import { describe, expect, it } from "vitest";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { NodeKind } from "$lib/bindings/NodeKind";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";
import {
  actionAvailability,
  bookmarksAt,
  childrenOf,
  combinedDescription,
  descendantsOf,
  divergentSiblings,
  findNode,
  isAncestor,
  moveDirection,
  rebaseDestinations,
  resolveCompareFrom,
  splitPath,
  stackBaseOf,
  stackPosition,
} from "./inspect";

function node(id: string, kind: NodeKind, parents: string[] = []): GraphNode {
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
    isEmpty: false,
    hasConflict: false,
    isDivergent: false,
  };
}

function workstream(id: string, nodeIds: string[]): WorkstreamSummary {
  return {
    id,
    title: id,
    nodeIds,
    bookmark: null,
    isActive: false,
    behindTrunk: 0,
  };
}

function snapshot(
  nodes: GraphNode[],
  workstreams: WorkstreamSummary[] = [],
  bookmarks: BookmarkState[] = [],
): RepoSnapshot {
  return {
    repoPath: "/tmp/repo",
    repoName: "repo",
    backend: "mock",
    trunkBookmark: "",
    workingCopy: "",
    workspaces: [],
    workstreams,
    nodes,
    bookmarks,
    conflicts: [],
    operations: [],
    resolveTool: null,
  };
}

describe("childrenOf", () => {
  it("finds every node that lists the id as a parent", () => {
    const snap = snapshot([
      node("a", "workingCopy", ["b"]),
      node("merge", "mutable", ["b", "side"]),
      node("b", "mutable", ["base"]),
      node("side", "mutable", ["base"]),
      node("base", "immutable"),
    ]);
    expect(childrenOf(snap, "b").map((n) => n.id)).toEqual(["a", "merge"]);
    expect(childrenOf(snap, "a")).toEqual([]);
  });
});

describe("descendantsOf", () => {
  it("walks transitive children, nearest first, across forks", () => {
    const snap = snapshot([
      node("top", "workingCopy", ["mid"]),
      node("merge", "mutable", ["mid", "side"]),
      node("mid", "mutable", ["base"]),
      node("side", "mutable", ["base"]),
      node("base", "immutable"),
    ]);
    expect(descendantsOf(snap, "base").map((n) => n.id)).toEqual([
      "mid",
      "side",
      "top",
      "merge",
    ]);
    // A node reachable through two paths is reported once.
    expect(descendantsOf(snap, "mid").map((n) => n.id)).toEqual([
      "top",
      "merge",
    ]);
    expect(descendantsOf(snap, "top")).toEqual([]);
  });
});

describe("combinedDescription", () => {
  it("yields the other side when one is empty", () => {
    expect(combinedDescription("", "child text")).toBe("child text");
    expect(combinedDescription("parent text", "")).toBe("parent text");
  });

  it("concatenates destination-first when both are real", () => {
    expect(combinedDescription("parent text\n", "child text")).toBe(
      "parent text\n\nchild text",
    );
  });
});

describe("stackPosition", () => {
  it("locates a node inside its workstream", () => {
    const snap = snapshot(
      [node("top", "workingCopy", ["low"]), node("low", "mutable")],
      [workstream("ws-a", ["top", "low"]), workstream("ws-b", ["other"])],
    );
    expect(stackPosition(snap, "low")).toMatchObject({
      workstream: { id: "ws-a" },
      index: 1,
    });
  });

  it("returns null for nodes outside every workstream", () => {
    const snap = snapshot(
      [node("base", "immutable")],
      [workstream("ws-a", ["top"])],
    );
    expect(stackPosition(snap, "base")).toBeNull();
  });
});

describe("bookmarksAt", () => {
  it("returns bookmark states targeting the node, trunk first", () => {
    const main: BookmarkState = {
      name: "main",
      target: "n1",
      remote: "origin",
      sync: "synced",
      isTrunk: true,
      isLocal: true,
    };
    const feature: BookmarkState = {
      name: "feature",
      target: "n1",
      remote: null,
      sync: "localOnly",
      isTrunk: false,
      isLocal: true,
    };
    const other: BookmarkState = {
      name: "other",
      target: "n2",
      remote: null,
      sync: "localOnly",
      isTrunk: false,
      isLocal: true,
    };
    const snap = snapshot([node("n1", "immutable")], [], [feature, main, other]);
    expect(bookmarksAt(snap, "n1").map((b) => b.name)).toEqual([
      "main",
      "feature",
    ]);
  });
});

describe("isAncestor and moveDirection", () => {
  // top → mid → base (elided link), side → base: a stack with elided
  // history plus an unrelated sibling.
  const elided = { ...node("mid", "mutable"), elidedParents: ["base"] };
  const snap = snapshot([
    node("top", "workingCopy", ["mid"]),
    elided,
    node("side", "mutable", ["base"]),
    node("base", "immutable"),
  ]);

  it("walks parent and elided-parent links", () => {
    expect(isAncestor(snap, "base", "top")).toBe(true);
    expect(isAncestor(snap, "mid", "top")).toBe(true);
    expect(isAncestor(snap, "top", "base")).toBe(false);
    expect(isAncestor(snap, "side", "top")).toBe(false);
  });

  it("labels bookmark moves like the backend summary", () => {
    expect(moveDirection(snap, "base", "top")).toBe("forward");
    expect(moveDirection(snap, "top", "base")).toBe("backwards");
    expect(moveDirection(snap, "side", "top")).toBe("sideways");
    // Either end outside the snapshot: no direction to state.
    expect(moveDirection(snap, "gone", "top")).toBeNull();
  });
});

describe("rebaseDestinations", () => {
  // top → mid → base; side → base: a stack plus an unrelated sibling.
  const snap = snapshot([
    node("top", "workingCopy", ["mid"]),
    node("mid", "mutable", ["base"]),
    node("side", "mutable", ["base"]),
    node("base", "immutable"),
  ]);

  it("excludes the change, its descendants, and its sole parent when descendants come along", () => {
    expect(rebaseDestinations(snap, "mid", true).map((n) => n.id)).toEqual([
      "side",
    ]);
  });

  it("keeps descendants as lone-move destinations (how adjacent changes swap)", () => {
    // mid still has descendants to extract, so its parent stays offered.
    expect(rebaseDestinations(snap, "mid", false).map((n) => n.id)).toEqual([
      "top",
      "side",
      "base",
    ]);
  });

  it("excludes the sole parent for a lone move with nothing to extract", () => {
    expect(rebaseDestinations(snap, "top", false).map((n) => n.id)).toEqual([
      "side",
      "base",
    ]);
    expect(rebaseDestinations(snap, "top", true).map((n) => n.id)).toEqual([
      "side",
      "base",
    ]);
  });

  it("returns nothing for unknown changes", () => {
    expect(rebaseDestinations(snap, "gone", true)).toEqual([]);
  });
});

describe("stackBaseOf and resolveCompareFrom", () => {
  // top → mid → trunk(main) ── old(elided); side sits on old, linking to
  // its base only through elided history.
  const sideNode = { ...node("side", "mutable"), elidedParents: ["old"] };
  const main: BookmarkState = {
    name: "main",
    target: "trunk",
    remote: "origin",
    sync: "synced",
    isTrunk: true,
    isLocal: true,
  };
  const snap = snapshot(
    [
      node("top", "workingCopy", ["mid"]),
      node("mid", "mutable", ["trunk"]),
      sideNode,
      node("trunk", "immutable"),
      node("old", "immutable"),
    ],
    [workstream("ws-a", ["top", "mid"]), workstream("ws-b", ["side"])],
    [main],
  );

  it("finds the (possibly elided) parent under the stack's bottom node", () => {
    expect(stackBaseOf(snap, "top")).toBe("trunk");
    expect(stackBaseOf(snap, "mid")).toBe("trunk");
    expect(stackBaseOf(snap, "side")).toBe("old");
    // Immutable bases belong to no workstream.
    expect(stackBaseOf(snap, "trunk")).toBeNull();
  });

  it("resolves each compare mode for a selection", () => {
    expect(resolveCompareFrom(snap, "top", { kind: "parent" })).toBeNull();
    expect(resolveCompareFrom(snap, "top", { kind: "trunk" })).toBe("trunk");
    expect(resolveCompareFrom(snap, "top", { kind: "base" })).toBe("trunk");
    expect(resolveCompareFrom(snap, "side", { kind: "base" })).toBe("old");
    expect(
      resolveCompareFrom(snap, "top", { kind: "change", id: "side" }),
    ).toBe("side");
  });

  it("falls back to null when the comparison cannot apply", () => {
    // Comparing trunk against itself, a node without a workstream, or a
    // picked change that left the snapshot.
    expect(resolveCompareFrom(snap, "trunk", { kind: "trunk" })).toBeNull();
    expect(resolveCompareFrom(snap, "trunk", { kind: "base" })).toBeNull();
    expect(
      resolveCompareFrom(snap, "top", { kind: "change", id: "gone" }),
    ).toBeNull();
    expect(
      resolveCompareFrom(snap, "top", { kind: "change", id: "top" }),
    ).toBeNull();
    // No trunk bookmark at all.
    const trunkless = snapshot([node("top", "workingCopy")]);
    expect(resolveCompareFrom(trunkless, "top", { kind: "trunk" })).toBeNull();
  });
});

describe("divergentSiblings", () => {
  // Divergent copies share changeId but key by commit id.
  const copyA = {
    ...node("b41c77d0", "mutable", ["trunk"]),
    changeId: "rzvqnkom",
    isDivergent: true,
  };
  const copyB = {
    ...node("e93d5a12", "mutable", ["trunk"]),
    changeId: "rzvqnkom",
    isDivergent: true,
  };
  const plain = node("side", "mutable", ["trunk"]);
  const snap = snapshot([copyA, copyB, plain, node("trunk", "immutable")]);

  it("finds the other visible copies of a divergent change", () => {
    expect(divergentSiblings(snap, copyA).map((n) => n.id)).toEqual([
      "e93d5a12",
    ]);
    expect(divergentSiblings(snap, copyB).map((n) => n.id)).toEqual([
      "b41c77d0",
    ]);
  });

  it("returns nothing for non-divergent nodes", () => {
    expect(divergentSiblings(snap, plain)).toEqual([]);
  });
});

describe("splitPath", () => {
  it("splits directory and file name", () => {
    expect(splitPath("src/lib/api.ts")).toEqual({
      dir: "src/lib/",
      name: "api.ts",
    });
  });

  it("handles root-level files", () => {
    expect(splitPath("README.md")).toEqual({ dir: "", name: "README.md" });
  });
});

describe("actionAvailability", () => {
  const snap = snapshot([
    node("wc", "workingCopy", ["mid"]),
    node("mid", "mutable", ["bottom"]),
    node("bottom", "mutable", ["base"]),
    node("merge", "mutable", ["mid", "bottom"]),
    node("base", "immutable"),
  ]);
  const at = (id: string) => actionAvailability(snap, findNode(snap, id)!);

  it("offers everything on a mutable change with a mutable parent", () => {
    expect(at("mid")).toEqual({
      describe: true,
      newChild: true,
      edit: true,
      bookmark: true,
      rebase: true,
      squash: true,
      abandon: true,
    });
  });

  it("offers only the non-rewriting pair on immutable changes", () => {
    expect(at("base")).toEqual({
      describe: false,
      newChild: true,
      edit: false,
      bookmark: true,
      rebase: false,
      squash: false,
      abandon: false,
    });
  });

  it("withholds squash without a single mutable parent", () => {
    // Immutable parent.
    expect(at("bottom").squash).toBe(false);
    // Merge: two parents.
    expect(at("merge").squash).toBe(false);
  });

  it("withholds edit on the working copy itself", () => {
    const wc = at("wc");
    expect(wc.edit).toBe(false);
    expect(wc.describe).toBe(true);
  });
});
