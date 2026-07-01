import { describe, expect, it } from "vitest";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { NodeKind } from "$lib/bindings/NodeKind";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";
import {
  buildFocusModel,
  buildGraphModel,
  emphasizedStreamId,
  resolveFocusedWorkstream,
  selectableIds,
  streamOfNode,
  type NodeRow,
} from "./graph";

function node(
  id: string,
  kind: NodeKind,
  parents: string[] = [],
  timestamp = "2026-06-10T12:00:00Z",
  elidedParents: string[] = [],
): GraphNode {
  return {
    id,
    changeId: id,
    commitId: `c-${id}`,
    description: `change ${id}`,
    author: "test",
    timestamp,
    kind,
    parents,
    elidedParents,
    bookmarks: [],
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
    trunkBookmark: bookmarks.find((b) => b.isTrunk)?.name ?? "",
    workingCopy: nodes.find((n) => n.kind === "workingCopy")?.id ?? "",
    workspaces: [],
    workstreams,
    nodes,
    bookmarks,
    conflicts: [],
    operations: [],
  };
}

function bookmark(
  name: string,
  target: string,
  opts: Partial<BookmarkState> = {},
): BookmarkState {
  return {
    name,
    target,
    remote: null,
    sync: "localOnly",
    isTrunk: false,
    isLocal: true,
    ...opts,
  };
}

function nodeRows(model: ReturnType<typeof buildGraphModel>): NodeRow[] {
  return model.rows.filter((row): row is NodeRow => row.type === "node");
}

function rowIds(model: ReturnType<typeof buildGraphModel>): string[] {
  return model.rows.map((row) => (row.type === "node" ? row.node.id : row.id));
}

describe("buildGraphModel", () => {
  it("lays out two stacks merging onto a shared trunk base", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["a1"]),
        node("a1", "mutable", ["trunk"]),
        node("b2", "mutable", ["b1"]),
        node("b1", "mutable", ["trunk"]),
        node("trunk", "immutable"),
      ],
      [
        workstream("ws-a", ["wc", "a1"], { isActive: true }),
        workstream("ws-b", ["b2", "b1"], { behindTrunk: 2 }),
      ],
      [
        bookmark("main", "trunk", { remote: "origin", sync: "synced", isTrunk: true }),
        bookmark("feature", "b2", { sync: "ahead", remote: "origin" }),
      ],
    );
    const model = buildGraphModel(snap);

    // Working stack first, sibling next, trunk last, gap marker below it.
    expect(rowIds(model)).toEqual(["wc", "a1", "b2", "b1", "trunk", "~trunk"]);

    const [wc, a1, b2, b1, trunk] = nodeRows(model);
    expect(wc.column).toBe(0);
    expect(wc.isWorkingCopy).toBe(true);
    expect(wc.isStackHead).toBe(true);
    expect(a1.column).toBe(0);
    expect(b2.column).toBe(1);
    expect(b2.isStackHead).toBe(true);
    expect(b2.behindTrunk).toBe(2);
    expect(b2.bookmarks.map((b) => b.name)).toEqual(["feature"]);

    // The sibling rows let the working stack's rail pass through.
    expect(b2.passThrough).toEqual([{ col: 0, stream: "ws-a" }]);
    expect(b1.passThrough).toEqual([{ col: 0, stream: "ws-a" }]);

    // Both stacks converge on trunk: a straight rail and a fork-in.
    expect(trunk.column).toBe(0);
    expect(trunk.stream).toBeNull();
    expect(trunk.edgesIn).toEqual([
      { col: 0, stream: "ws-a" },
      { col: 1, stream: "ws-b" },
    ]);
    expect(trunk.edgesOut).toEqual([]);
    expect(trunk.bookmarks.map((b) => b.name)).toEqual(["main"]);

    const elision = model.rows.at(-1)!;
    expect(elision.type).toBe("elision");
    if (elision.type === "elision") {
      expect(elision.marks).toEqual([{ col: 0, stream: null }]);
      expect(elision.continues).toBe(false);
      expect(elision.passThrough).toEqual([]);
    }

    expect(model.columnCount).toBe(2);
    expect(model.trunkName).toBe("main");
  });

  it("interleaves a sibling that forks off the middle of the working stack", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["x"]),
        node("x", "mutable", ["low"]),
        node("low", "mutable", ["trunk"]),
        node("s1", "mutable", ["x"]),
        node("trunk", "immutable"),
      ],
      [
        workstream("ws-a", ["wc", "x", "low"], { isActive: true }),
        workstream("ws-b", ["s1"]),
      ],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const model = buildGraphModel(snap);

    // s1 is a child of x, so topology forces it above x even though the
    // working stack has priority.
    expect(rowIds(model)).toEqual(["wc", "s1", "x", "low", "trunk", "~trunk"]);

    const x = nodeRows(model)[2];
    expect(x.column).toBe(0);
    expect(x.edgesIn).toEqual([
      { col: 0, stream: "ws-a" },
      { col: 1, stream: "ws-b" },
    ]);
  });

  it("routes a merge's second parent into a column already waiting for it", () => {
    const snap = snapshot(
      [
        node("t1", "mutable", ["p2"]),
        node("m", "mutable", ["p1", "p2"]),
        node("p1", "mutable", []),
        node("p2", "mutable", []),
      ],
      [
        workstream("ws-1", ["t1"], { isActive: true }),
        workstream("ws-2", ["m", "p1"]),
      ],
    );
    const model = buildGraphModel(snap);

    expect(rowIds(model)).toEqual(["t1", "m", "p1", "p2"]);
    const m = nodeRows(model)[1];
    expect(m.column).toBe(1);
    // First parent keeps the node's column; the second one merges into
    // ws-1's column, which is already waiting for p2.
    expect(m.edgesOut).toEqual([
      { col: 1, stream: "ws-2" },
      { col: 0, stream: "ws-1" },
    ]);

    const p2 = nodeRows(model)[3];
    expect(p2.column).toBe(0);
    expect(p2.edgesIn).toEqual([{ col: 0, stream: "ws-1" }]);
    // p2 is mutable and parentless: the rail ends without an elision mark.
    expect(model.rows.filter((row) => row.type === "elision")).toEqual([]);
  });

  it("opens a second column for a merge parent nothing waits for yet", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["a1", "b1"]),
        node("a1", "mutable", ["trunk"]),
        node("b1", "mutable", ["trunk"]),
        node("trunk", "immutable"),
      ],
      [workstream("ws-a", ["wc", "a1"], { isActive: true }), workstream("ws-b", ["b1"])],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const model = buildGraphModel(snap);

    const wc = nodeRows(model)[0];
    expect(wc.edgesOut).toEqual([
      { col: 0, stream: "ws-a" },
      { col: 1, stream: "ws-a" },
    ]);
    // b1 lands on the column the merge opened for it.
    const b1 = nodeRows(model)[2];
    expect(b1.column).toBe(1);
    expect(b1.edgesIn).toEqual([{ col: 1, stream: "ws-a" }]);
  });

  it("interleaves stacks with their bases by recency, jj-log style", () => {
    // The working stack sits on an old base; a sibling sits on trunk. Each
    // base must land right under the work that grew from it, newest first,
    // instead of all bases pooling at the bottom.
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["old"], "2026-06-11T10:00:00Z"),
        node("b1", "mutable", ["trunk"], "2026-06-10T11:00:00Z"),
        node("trunk", "immutable", [], "2026-06-10T08:00:00Z"),
        node("old", "immutable", [], "2026-06-01T12:00:00Z"),
      ],
      [
        workstream("ws-a", ["wc"], { isActive: true, behindTrunk: 5 }),
        workstream("ws-b", ["b1"]),
      ],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const model = buildGraphModel(snap);

    expect(rowIds(model)).toEqual(["wc", "b1", "trunk", "~trunk", "old", "~old"]);
    // While trunk's gap renders, the working stack's rail keeps passing by.
    const trunkElision = model.rows[3];
    expect(trunkElision.type).toBe("elision");
    expect(trunkElision.passThrough).toEqual([{ col: 0, stream: "ws-a" }]);
    const wc = nodeRows(model)[0];
    expect(wc.behindTrunk).toBe(5);
  });

  it("orders sibling stacks by their head's recency, not listing order", () => {
    const snap = snapshot(
      [
        node("o1", "mutable", ["base"], "2026-06-01T10:00:00Z"),
        node("n1", "mutable", ["base"], "2026-06-10T10:00:00Z"),
        node("base", "immutable", [], "2026-05-01T10:00:00Z"),
      ],
      // Listed oldest-first on purpose; recency must win.
      [workstream("ws-old", ["o1"]), workstream("ws-new", ["n1"])],
    );
    const model = buildGraphModel(snap);

    expect(rowIds(model)).toEqual(["n1", "o1", "base", "~base"]);
  });

  it("links bases through elided history as one continuing spine", () => {
    // trunk reaches old only through commits the snapshot omits; the rail
    // continues through a `~` row instead of ending, and the sibling on old
    // merges into the same spine.
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["trunk"], "2026-06-11T10:00:00Z"),
        node("b1", "mutable", ["old"], "2026-06-09T10:00:00Z"),
        node("trunk", "immutable", [], "2026-06-10T08:00:00Z", ["old"]),
        node("old", "immutable", [], "2026-06-01T12:00:00Z"),
      ],
      [
        workstream("ws-a", ["wc"], { isActive: true }),
        workstream("ws-b", ["b1"]),
      ],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const model = buildGraphModel(snap);

    expect(rowIds(model)).toEqual(["wc", "trunk", "~trunk", "b1", "old", "~old"]);

    // The elided edge renders dashed and keeps trunk's column alive.
    const trunk = nodeRows(model)[1];
    expect(trunk.edgesOut).toEqual([{ col: 0, stream: null, elided: true }]);
    const gap = model.rows[2];
    expect(gap.type).toBe("elision");
    if (gap.type === "elision") {
      expect(gap.marks).toEqual([{ col: 0, stream: null }]);
      expect(gap.continues).toBe(true);
    }

    // Both the spine and the sibling stack converge on the old base.
    const old = nodeRows(model)[3];
    expect(old.column).toBe(0);
    expect(old.edgesIn).toEqual([
      { col: 0, stream: null },
      { col: 1, stream: "ws-b" },
    ]);
    // Shown history truly ends below old.
    const terminal = model.rows.at(-1)!;
    expect(terminal.type).toBe("elision");
    if (terminal.type === "elision") expect(terminal.continues).toBe(false);
  });

  it("draws directly linked bases without a gap row", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["t2"], "2026-06-11T10:00:00Z"),
        node("b1", "mutable", ["t1"], "2026-06-09T10:00:00Z"),
        node("t2", "immutable", ["t1"], "2026-06-10T08:00:00Z"),
        node("t1", "immutable", [], "2026-06-01T12:00:00Z"),
      ],
      [
        workstream("ws-a", ["wc"], { isActive: true }),
        workstream("ws-b", ["b1"]),
      ],
    );
    const model = buildGraphModel(snap);

    // t2 → t1 is a drawn parent: solid rail, no `~` between them.
    expect(rowIds(model)).toEqual(["wc", "t2", "b1", "t1", "~t1"]);
    const t2 = nodeRows(model)[1];
    expect(t2.edgesOut).toEqual([{ col: 0, stream: null }]);
  });

  it("frees merged columns for reuse by later heads", () => {
    const snap = snapshot(
      [
        node("a2", "mutable", ["a1"]),
        node("a1", "mutable", ["base"]),
        node("b1", "mutable", ["a1"]),
        node("c1", "mutable", ["base"]),
        node("base", "immutable"),
      ],
      [
        workstream("ws-a", ["a2", "a1"], { isActive: true }),
        workstream("ws-b", ["b1"]),
        workstream("ws-c", ["c1"]),
      ],
    );
    const model = buildGraphModel(snap);

    expect(rowIds(model)).toEqual(["a2", "b1", "a1", "c1", "base", "~base"]);
    // b1's column (1) merges into a1; c1 reuses the freed slot 1.
    const c1 = nodeRows(model)[3];
    expect(c1.column).toBe(1);
    expect(model.columnCount).toBe(2);
  });

  it("ignores workstream node ids missing from the snapshot", () => {
    const snap = snapshot(
      [node("wc", "workingCopy", [])],
      [workstream("ws", ["gone", "wc"], { isActive: true })],
    );
    const model = buildGraphModel(snap);

    expect(rowIds(model)).toEqual(["wc"]);
    const wc = nodeRows(model)[0];
    // "gone" was the nominal head; the first present node stands in as the
    // visible top of the stack.
    expect(wc.isStackHead).toBe(true);
    expect(wc.stream).toBe("ws");
  });
});

describe("buildFocusModel", () => {
  it("draws one workstream on its base, single column, terminal gap", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["a1"]),
        node("a1", "mutable", ["trunk"]),
        node("b2", "mutable", ["b1"]),
        node("b1", "mutable", ["trunk"]),
        node("trunk", "immutable"),
      ],
      [
        workstream("ws-a", ["wc", "a1"], { isActive: true }),
        workstream("ws-b", ["b2", "b1"], { behindTrunk: 2 }),
      ],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const focus = buildFocusModel(snap, snap.workstreams[1]);

    // Only the focused chain and its base — the sibling stack stays out, so
    // the lane is one straight rail.
    expect(rowIds(focus.graph)).toEqual(["b2", "b1", "trunk", "~trunk"]);
    expect(focus.graph.columnCount).toBe(1);
    expect(nodeRows(focus.graph).every((row) => row.column === 0)).toBe(true);

    // Trunk is the base we draw, so the stale-base count stays quiet.
    expect(focus.trunkOnBase).toBe(true);
    expect(focus.behindTrunk).toBe(0);
    expect(focus.trunkName).toBe("main");
  });

  it("keeps the stale-base count when the stack sits behind trunk", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["old"]),
        node("trunk", "immutable"),
        node("old", "immutable"),
      ],
      [workstream("ws-a", ["wc"], { isActive: true, behindTrunk: 5 })],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const focus = buildFocusModel(snap, snap.workstreams[0]);

    expect(rowIds(focus.graph)).toEqual(["wc", "old", "~old"]);
    expect(focus.trunkOnBase).toBe(false);
    expect(focus.behindTrunk).toBe(5);
  });

  it("ignores elided links so the lane ends at its base", () => {
    // In the full graph trunk continues to old through a `~` row; the lane
    // stops at trunk with a terminal gap instead.
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["trunk"]),
        node("trunk", "immutable", [], "2026-06-10T08:00:00Z", ["old"]),
        node("old", "immutable"),
      ],
      [workstream("ws-a", ["wc"], { isActive: true })],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const focus = buildFocusModel(snap, snap.workstreams[0]);

    expect(rowIds(focus.graph)).toEqual(["wc", "trunk", "~trunk"]);
    const gap = focus.graph.rows.at(-1)!;
    expect(gap.type).toBe("elision");
    if (gap.type === "elision") expect(gap.continues).toBe(false);
  });

  it("drops sibling-claimed parents: a stack forked off another stack", () => {
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["x"]),
        node("x", "mutable", ["trunk"]),
        node("s1", "mutable", ["x"]),
        node("trunk", "immutable"),
      ],
      [
        workstream("ws-a", ["wc", "x"], { isActive: true }),
        workstream("ws-b", ["s1"]),
      ],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const focus = buildFocusModel(snap, snap.workstreams[1]);

    // s1's parent is mutable and belongs to ws-a: no base zone, the rail
    // just ends, like the old lane did.
    expect(rowIds(focus.graph)).toEqual(["s1"]);
    expect(focus.graph.rows.filter((row) => row.type === "elision")).toEqual([]);
  });
});

describe("selection helpers", () => {
  const snap = snapshot(
    [
      node("wc", "workingCopy", ["trunk"]),
      node("b1", "mutable", ["trunk"]),
      node("trunk", "immutable"),
    ],
    [
      workstream("ws-a", ["wc"], { isActive: true }),
      workstream("ws-b", ["b1"]),
    ],
    [bookmark("main", "trunk", { isTrunk: true })],
  );
  const model = buildGraphModel(snap);

  it("selectableIds skips elision rows", () => {
    expect(selectableIds(model)).toEqual(["wc", "b1", "trunk"]);
  });

  it("streamOfNode resolves ownership, null for bases and unknowns", () => {
    expect(streamOfNode(model, "b1")).toBe("ws-b");
    expect(streamOfNode(model, "trunk")).toBeNull();
    expect(streamOfNode(model, "nope")).toBeNull();
  });

  it("emphasizedStreamId prefers the selection's stream", () => {
    expect(emphasizedStreamId(model, snap, "b1", null)).toBe("ws-b");
    // A base selection keeps the focused/active stream hot.
    expect(emphasizedStreamId(model, snap, "trunk", "ws-b")).toBe("ws-b");
    expect(emphasizedStreamId(model, snap, null, null)).toBe("ws-a");
  });

  it("resolveFocusedWorkstream falls back focused → active → first", () => {
    expect(resolveFocusedWorkstream(snap, "ws-b")?.id).toBe("ws-b");
    expect(resolveFocusedWorkstream(snap, "missing")?.id).toBe("ws-a");
    expect(resolveFocusedWorkstream(snap, null)?.id).toBe("ws-a");
  });
});
