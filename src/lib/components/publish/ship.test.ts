import { describe, expect, it } from "vitest";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { NodeKind } from "$lib/bindings/NodeKind";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";
import { shipActionRow, shippableStacks } from "./ship";

function node(
  id: string,
  kind: NodeKind,
  description = `change ${id}`,
  isEmpty = false,
): GraphNode {
  return {
    id,
    changeId: id,
    commitId: `c${id}`,
    description,
    author: "test",
    timestamp: "2026-07-01T12:00:00Z",
    kind,
    parents: [],
    elidedParents: [],
    bookmarks: [],
    isEmpty,
    hasConflict: false,
    isDivergent: false,
  };
}

function workstream(
  id: string,
  nodeIds: string[],
  isActive = false,
): WorkstreamSummary {
  return {
    id,
    title: `stream ${id}`,
    nodeIds,
    bookmark: null,
    isActive,
    behindTrunk: 0,
  };
}

function snapshot(
  nodes: GraphNode[],
  workstreams: WorkstreamSummary[],
  workingCopy = "",
): RepoSnapshot {
  return {
    repoPath: "/tmp/repo",
    repoName: "repo",
    backend: "mock",
    trunkBookmark: "main",
    workingCopy,
    workspaces: [],
    workstreams,
    nodes,
    bookmarks: [],
    gitRemotes: [],
    conflicts: [],
    operations: [],
    resolveTool: null,
  };
}

describe("shippableStacks", () => {
  it("offers every workstream, bookmark or not, active first", () => {
    const snap = snapshot(
      [node("a1", "mutable"), node("b1", "workingCopy"), node("b2", "mutable")],
      [workstream("a", ["a1"]), workstream("b", ["b1", "b2"], true)],
      "b1",
    );
    const stacks = shippableStacks(snap);
    expect(stacks.map((s) => s.workstreamId)).toEqual(["b", "a"]);
    expect(stacks[0].headChange).toBe("b1");
    expect(stacks[0].changeCount).toBe(2);
  });

  it("skips an undescribed working copy on top — content or not — and a draft-only stream entirely", () => {
    const snap = snapshot(
      [
        // Undescribed with real edits: still a draft, not a ship head.
        node("wc", "workingCopy", "", false),
        node("b1", "mutable", "feat: real work"),
      ],
      [workstream("b", ["wc", "b1"], true)],
      "wc",
    );
    const stacks = shippableStacks(snap);
    expect(stacks).toHaveLength(1);
    expect(stacks[0].headChange).toBe("b1");
    expect(stacks[0].headTitle).toBe("feat: real work");
    expect(stacks[0].changeCount).toBe(1);

    const draftOnly = snapshot(
      [node("lone", "workingCopy", "", true)],
      [workstream("l", ["lone"], true)],
      "lone",
    );
    expect(shippableStacks(draftOnly)).toHaveLength(0);
  });

  it("ships a described working copy", () => {
    const described = snapshot(
      [node("wc", "workingCopy", "fix: on top", false)],
      [workstream("w", ["wc"], true)],
      "wc",
    );
    expect(shippableStacks(described)[0].headChange).toBe("wc");
  });
});

describe("shipActionRow", () => {
  it("phrases each action", () => {
    expect(
      shipActionRow({ kind: "rebaseOntoTrunk", rootChange: "abcd", moves: 2 })
        .text,
    ).toContain("its 1 descendant");
    expect(
      shipActionRow({ kind: "moveTrunk", bookmark: "main", to: "abcd" }).text,
    ).toContain("Point main at abcd");
    const push = shipActionRow({
      kind: "pushTrunk",
      bookmark: "main",
      remote: "origin",
    });
    expect(push.text).toContain("Push main to origin");
    expect(push.tone).toBe("ok");
    expect(
      shipActionRow({ kind: "newWorkingCopy", on: "abcd" }).text,
    ).toContain("fresh working copy");
  });
});
