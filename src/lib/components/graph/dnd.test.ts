import { describe, expect, it } from "vitest";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { NodeKind } from "$lib/bindings/NodeKind";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import { canDrag, planDrop } from "./dnd";

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

function snapshot(nodes: GraphNode[], workingCopy = ""): RepoSnapshot {
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
    conflicts: [],
    operations: [],
  };
}

// A stack (top → mid → low → base) beside a sibling on the same base.
const stacked = snapshot(
  [
    node("top", "workingCopy", ["mid"]),
    node("mid", "mutable", ["low"]),
    node("low", "mutable", ["base"]),
    node("side", "mutable", ["base"]),
    node("base", "immutable"),
  ],
  "top",
);

describe("canDrag", () => {
  it("allows mutable and working-copy rows, never immutable ones", () => {
    expect(canDrag(node("a", "mutable"))).toBe(true);
    expect(canDrag(node("a", "workingCopy"))).toBe(true);
    expect(canDrag(node("a", "immutable"))).toBe(false);
  });
});

describe("planDrop", () => {
  it("reparents with descendants by default, onto immutable targets too", () => {
    const plan = planDrop(stacked, "low", "side", false);
    expect(plan).toMatchObject({
      allowed: true,
      op: "rebase",
      forcedAlone: false,
      summary: "Rebase onto side — with 2 descendants",
    });
    // Rebasing onto trunk is the everyday gesture; immutable is fine.
    expect(planDrop(stacked, "mid", "base", false)).toMatchObject({
      allowed: true,
      op: "rebase",
    });
  });

  it("moves alone when ⌥ is held, leaving descendants behind", () => {
    const plan = planDrop(stacked, "low", "side", true);
    expect(plan).toMatchObject({
      allowed: true,
      op: "move",
      forcedAlone: false,
      summary: "Move onto side — only this change",
    });
    if (!plan.allowed) throw new Error("unreachable");
    expect(plan.consequences).toContain(
      "2 descendants stay behind, reparented onto base.",
    );
    expect(plan.consequences).toContain("The working copy stays behind.");
  });

  it("ignores ⌥ when there are no descendants — scope is meaningless", () => {
    expect(planDrop(stacked, "top", "side", true)).toMatchObject({
      allowed: true,
      op: "rebase",
      summary: "Rebase onto side",
    });
  });

  it("degrades to a lone move when the target is inside the dragged stack", () => {
    const plan = planDrop(stacked, "low", "mid", false);
    expect(plan).toMatchObject({
      allowed: true,
      op: "move",
      forcedAlone: true,
      summary: "Move onto mid — swapping their order",
    });
    if (!plan.allowed) throw new Error("unreachable");
    expect(plan.consequences[0]).toBe(
      "mid is inside the dragged stack, so this change moves alone.",
    );
    // A deeper descendant reads as reordering, not swapping.
    expect(planDrop(stacked, "low", "top", true)).toMatchObject({
      allowed: true,
      forcedAlone: false,
      summary: "Move onto top — reordering the stack",
    });
  });

  it("refuses self-drops and no-op drops onto the sole parent", () => {
    expect(planDrop(stacked, "mid", "mid", false)).toMatchObject({
      allowed: false,
      reason: "A change cannot become its own parent",
    });
    expect(planDrop(stacked, "top", "mid", false)).toMatchObject({
      allowed: false,
      reason: "Already the parent of top — nothing would move",
    });
  });

  it("treats ⌥-dropping onto the sole parent as extracting the change", () => {
    const plan = planDrop(stacked, "low", "base", true);
    expect(plan).toMatchObject({
      allowed: true,
      op: "move",
      summary: "Extract low — its descendants skip past it",
    });
    if (!plan.allowed) throw new Error("unreachable");
    expect(plan.consequences).toContain(
      "It stays on base; 2 descendants reparent onto base.",
    );
  });

  it("names the dissolved merge and the moving working copy", () => {
    const merged = snapshot(
      [
        node("wc", "workingCopy", ["mid", "side"]),
        node("mid", "mutable", ["base"]),
        node("side", "mutable", ["base"]),
        node("base", "immutable"),
      ],
      "wc",
    );
    const plan = planDrop(merged, "wc", "base", false);
    expect(plan).toMatchObject({ allowed: true, op: "rebase" });
    if (!plan.allowed) throw new Error("unreachable");
    expect(plan.consequences).toContain(
      "Its 2 parents are replaced by the destination — the merge is dissolved.",
    );
    expect(plan.consequences).toContain("The working copy moves with it.");
  });

  it("notes the working copy following a stack rebase", () => {
    const plan = planDrop(stacked, "low", "side", false);
    if (!plan.allowed) throw new Error("unreachable");
    expect(plan.consequences).toContain("The working copy follows the rebase.");
  });

  it("refuses immutable sources and targets that left the snapshot", () => {
    expect(planDrop(stacked, "base", "side", false)).toMatchObject({
      allowed: false,
      reason: "Immutable changes cannot be rebased",
    });
    expect(planDrop(stacked, "low", "gone", false)).toMatchObject({
      allowed: false,
      reason: "That change left the snapshot",
    });
  });
});
