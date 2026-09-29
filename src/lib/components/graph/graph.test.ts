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
  fullLanesFor,
  gutterWidth,
  keyedElisionRails,
  keyedRails,
  laneTier,
  railInPath,
  railOutPath,
  railX,
  resolveFocusedWorkstream,
  selectableIds,
  streamOfNode,
  MAX_FULL_LANES,
  MIN_FULL_LANES,
  NARROW_LANES,
  type ElisionRow,
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
    trunkBookmark: bookmarks.find((b) => b.isTrunk)?.name ?? "",
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
    tracked: false,
    isMerged: false,
    targetDescription: "",
    targetAuthor: "",
    targetTimestamp: "",
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

    // The sibling's rail joins the working stack's rail — already waiting
    // for trunk — right at its bottom row (jj's ├─╯), wearing its own
    // stream; trunk then receives one rail for both.
    expect(b1.edgesOut).toEqual([{ col: 0, stream: "ws-b" }]);
    expect(trunk.column).toBe(0);
    expect(trunk.stream).toBeNull();
    expect(trunk.edgesIn).toEqual([{ col: 0, stream: "ws-a" }]);
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

    // s1 bends into the working stack's rail, which already waits for x.
    const [, s1, x] = nodeRows(model);
    expect(s1.column).toBe(1);
    expect(s1.edgesOut).toEqual([{ col: 0, stream: "ws-b" }]);
    expect(x.column).toBe(0);
    expect(x.edgesIn).toEqual([{ col: 0, stream: "ws-a" }]);
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
    // ws-1's column, which is already waiting for p2. Both rails leave m,
    // so both wear m's stream; ws-1's own rail keeps passing underneath.
    expect(m.edgesOut).toEqual([
      { col: 1, stream: "ws-2" },
      { col: 0, stream: "ws-2" },
    ]);
    expect(m.passThrough).toEqual([{ col: 0, stream: "ws-1" }]);

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

  it("keeps each stack together with the base it grew from", () => {
    // The working stack sits on an old base unconnected to trunk (nothing
    // links them in this snapshot); a sibling sits on trunk. Each base
    // lands right under the work that grew from it — the walk follows a
    // branch to its end before returning to the next head, git/jj
    // topo-order — instead of the older base being pushed below the
    // newer sibling by date.
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

    expect(rowIds(model)).toEqual(["wc", "old", "~old", "b1", "trunk", "~trunk"]);
    // One line at a time: nothing passes through the closed-off bases, and
    // the freed column serves the sibling next.
    const oldElision = model.rows[2];
    expect(oldElision.type).toBe("elision");
    expect(oldElision.passThrough).toEqual([]);
    const [wc, , b1] = nodeRows(model);
    expect(wc.behindTrunk).toBe(5);
    expect(b1.column).toBe(0);
    expect(model.columnCount).toBe(1);
  });

  it("follows a branch to its base before the next head, git topo-order", () => {
    // Lens views draw unclaimed history: h1's parent p1 is older than the
    // other head h2, yet stays with h1 — interleaving the two lines by
    // date is what makes deep graphs unreadable.
    const snap = snapshot(
      [
        node("h1", "immutable", ["p1"], "2026-06-10T10:00:00Z"),
        node("h2", "immutable", ["base"], "2026-06-08T10:00:00Z"),
        node("p1", "immutable", ["base"], "2026-06-05T10:00:00Z"),
        node("base", "immutable", [], "2026-06-01T10:00:00Z"),
      ],
      [],
    );
    expect(rowIds(buildGraphModel(snap))).toEqual([
      "h1",
      "p1",
      "h2",
      "base",
      "~base",
    ]);
  });

  it("joins the rail already waiting for the same base, however many stacks", () => {
    // Five stacks on trunk. Each sibling's rail bends into the working
    // stack's rail at its bottom row instead of running its own column
    // down to trunk: two columns, not six, and trunk gets one rail in.
    const heads = ["b", "c", "d", "e", "f"];
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["a1"]),
        node("a1", "mutable", ["trunk"]),
        ...heads.flatMap((h) => [
          node(`${h}2`, "mutable", [`${h}1`]),
          node(`${h}1`, "mutable", ["trunk"]),
        ]),
        node("trunk", "immutable"),
      ],
      [
        workstream("ws-a", ["wc", "a1"], { isActive: true }),
        ...heads.map((h) => workstream(`ws-${h}`, [`${h}2`, `${h}1`])),
      ],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const model = buildGraphModel(snap);

    expect(model.columnCount).toBe(2);
    const rows = nodeRows(model);
    for (const h of heads) {
      const bottom = rows.find((row) => row.node.id === `${h}1`)!;
      expect(bottom.column).toBe(1);
      expect(bottom.edgesOut).toEqual([{ col: 0, stream: `ws-${h}` }]);
      // The joined rail keeps passing through the row the bend lands in.
      expect(bottom.passThrough).toEqual([{ col: 0, stream: "ws-a" }]);
    }
    const trunk = rows.at(-1)!;
    expect(trunk.edgesIn).toEqual([{ col: 0, stream: "ws-a" }]);
  });

  it("joins teammates' immutable branches on one base the same way", () => {
    // A Team lens: remote branches are immutable but not trunk's spine, so
    // they converge just like stacks do.
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["trunk"], "2026-06-11T10:00:00Z"),
        node("r1", "immutable", ["trunk"], "2026-06-10T10:00:00Z"),
        node("s1", "immutable", ["trunk"], "2026-06-09T10:00:00Z"),
        node("trunk", "immutable", [], "2026-06-01T10:00:00Z"),
      ],
      [workstream("ws-a", ["wc"], { isActive: true })],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const model = buildGraphModel(snap);

    expect(model.columnCount).toBe(2);
    const [, r1, s1, trunk] = nodeRows(model);
    expect(r1.edgesOut).toEqual([{ col: 0, stream: null }]);
    expect(s1.column).toBe(1);
    expect(s1.edgesOut).toEqual([{ col: 0, stream: null }]);
    expect(trunk.edgesIn).toEqual([{ col: 0, stream: "ws-a" }]);
  });

  it("joins only leftward: the leftmost rail keeps its column", () => {
    // m merges p and q. p also has the older head r above it, so the walk
    // reaches q first: q waits for base on column 1. When p (column 0)
    // needs base too it does not bend rightward into that younger rail —
    // it keeps its column and the two merge at base as before. (r, on
    // column 2, does bend leftward into p's waiting rail.)
    const snap = snapshot(
      [
        node("m", "mutable", ["p", "q"], "2026-06-11T10:00:00Z"),
        node("r", "immutable", ["p"], "2026-06-08T10:00:00Z"),
        node("q", "immutable", ["base"], "2026-06-10T10:00:00Z"),
        node("p", "immutable", ["base"], "2026-06-09T10:00:00Z"),
        node("base", "immutable", [], "2026-06-01T10:00:00Z"),
      ],
      [workstream("ws-m", ["m"], { isActive: true })],
    );
    const model = buildGraphModel(snap);

    expect(rowIds(model)).toEqual(["m", "q", "r", "p", "base", "~base"]);
    const [, q, r, p, base] = nodeRows(model);
    expect(q.column).toBe(1);
    expect(r.column).toBe(2);
    expect(r.edgesOut).toEqual([{ col: 0, stream: null }]);
    expect(p.column).toBe(0);
    expect(p.edgesOut).toEqual([{ col: 0, stream: null }]);
    expect(base.edgesIn).toEqual([
      { col: 0, stream: null },
      { col: 1, stream: null },
    ]);
    expect(model.columnCount).toBe(3);
  });

  it("never bends trunk's spine into a waiting rail", () => {
    // The working stack waits for old on column 0; trunk, a head here,
    // reaches old through elided history. Any other rail would join the
    // stack's rail — the spine keeps its own column so it stays the line
    // everything else converges on.
    const snap = snapshot(
      [
        node("wc", "workingCopy", ["old"], "2026-06-11T10:00:00Z"),
        node("trunk", "immutable", [], "2026-06-10T08:00:00Z", ["old"]),
        node("old", "immutable", [], "2026-06-01T12:00:00Z"),
      ],
      [workstream("ws-a", ["wc"], { isActive: true })],
      [bookmark("main", "trunk", { isTrunk: true })],
    );
    const model = buildGraphModel(snap);

    expect(rowIds(model)).toEqual(["wc", "trunk", "~trunk", "old", "~old"]);
    const [, trunk, old] = nodeRows(model);
    expect(trunk.column).toBe(1);
    expect(trunk.edgesOut).toEqual([{ col: 1, stream: null, elided: true }]);
    const gap = model.rows[2];
    expect(gap.type).toBe("elision");
    if (gap.type === "elision") {
      expect(gap.marks).toEqual([{ col: 1, stream: null }]);
      expect(gap.passThrough).toEqual([{ col: 0, stream: "ws-a" }]);
    }
    expect(old.edgesIn).toEqual([
      { col: 0, stream: "ws-a" },
      { col: 1, stream: null },
    ]);
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

    // The sibling stack bends into the spine, which already waits for old
    // beneath the gap; old receives the spine alone.
    const b1 = nodeRows(model)[2];
    expect(b1.column).toBe(1);
    expect(b1.edgesOut).toEqual([{ col: 0, stream: "ws-b" }]);
    expect(b1.passThrough).toEqual([{ col: 0, stream: null }]);
    const old = nodeRows(model)[3];
    expect(old.column).toBe(0);
    expect(old.edgesIn).toEqual([{ col: 0, stream: null }]);
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

describe("rail morphing helpers", () => {
  // A two-change stack on column 1 beside a trunk spine on column 0,
  // bending into the spine at its bottom: enough shape to exercise every
  // role in one row.
  const snap = snapshot(
    [
      node("wc", "workingCopy", ["a"], "2026-06-10T12:00:00Z"),
      node("a", "mutable", ["trunk"], "2026-06-10T11:00:00Z"),
      node("b2", "mutable", ["b1"], "2026-06-09T13:00:00Z"),
      node("b1", "mutable", ["old"], "2026-06-09T12:00:00Z"),
      node("trunk", "immutable", ["old"], "2026-06-08T12:00:00Z"),
      node("old", "immutable", [], "2026-06-07T12:00:00Z"),
    ],
    [
      workstream("ws-a", ["wc", "a"], { isActive: true }),
      workstream("ws-b", ["b2", "b1"]),
    ],
    [bookmark("main", "trunk", { isTrunk: true })],
  );
  const model = buildGraphModel(snap);
  const rowOf = (id: string): NodeRow =>
    model.rows.find(
      (row): row is NodeRow => row.type === "node" && row.node.id === id,
    )!;

  it("keys rails by role and stream, not by column", () => {
    // b1's row: the spine passes, ws-b arrives from b2 and bends out into
    // the spine — stream-scoped keys, no column numbers anywhere in the
    // identity, so a column shift keeps the key.
    const keys = keyedRails(rowOf("b1")).map((k) => k.key);
    expect(keys).toContain("pass:·:0");
    expect(keys).toContain("in:ws-b:0");
    expect(keys).toContain("out:ws-b:0");
    expect(keyedRails(rowOf("old")).map((k) => k.key)).toEqual(["in:·:0"]);
  });

  it("gives every rail in a row a unique key", () => {
    for (const id of ["wc", "a", "b2", "b1", "trunk", "old"]) {
      const keys = keyedRails(rowOf(id)).map((k) => k.key);
      expect(new Set(keys).size).toBe(keys.length);
    }
  });

  it("keeps the paint order: pass, then in, then out", () => {
    // b1's row carries all three roles: the spine passing by, the stack
    // arriving, the bend leaving into the spine.
    const roles = keyedRails(rowOf("b1")).map((k) => k.role);
    expect(roles).toEqual(["pass", "in", "out"]);
  });

  it("keys elision rails including marks", () => {
    const elision = model.rows.find(
      (row): row is ElisionRow => row.type === "elision",
    );
    expect(elision).toBeDefined();
    const keyed = keyedElisionRails(elision!);
    expect(keyed.some((k) => k.role === "mark")).toBe(true);
    const keys = keyed.map((k) => k.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("railInPath matches the classic shapes at integral columns", () => {
    // Straight drop onto its own column.
    expect(railInPath(12, 12, 26, 6)).toBe("M 12 0 V 7");
    // One column to the right of the node, curving left into it.
    expect(railInPath(25, 12, 26, 6)).toBe("M 25 0 V 7 Q 25 13 19 13 H 18");
    // One column to the left, curving right.
    expect(railInPath(12, 25, 26, 6)).toBe("M 12 0 V 7 Q 12 13 18 13 H 19");
    // Working-copy clearance (7) leaves a zero-length H, like before.
    expect(railInPath(25, 12, 26, 7)).toBe("M 25 0 V 7 Q 25 13 19 13 H 19");
  });

  it("railOutPath matches the classic shapes at integral columns", () => {
    expect(railOutPath(12, 12, 26, 6)).toBe("M 12 19 V 26");
    expect(railOutPath(25, 12, 26, 6)).toBe("M 18 13 H 19 Q 25 13 25 19 V 26");
    expect(railOutPath(12, 25, 26, 6)).toBe("M 19 13 H 18 Q 12 13 12 19 V 26");
  });

  it("clamps mid-tween geometry instead of overshooting", () => {
    // 3px from the node: the corner shrinks to the gap and the horizontal
    // reach collapses to zero — never a backwards H segment.
    expect(railInPath(15, 12, 26, 6)).toBe("M 15 0 V 10 Q 15 13 12 13 H 12");
    expect(railOutPath(15, 12, 26, 6)).toBe("M 12 13 H 12 Q 15 13 15 16 V 26");
    // Sub-pixel from the node reads as arrived.
    expect(railInPath(12.2, 12, 26, 6)).toBe("M 12.2 0 V 7");
    // No NaNs anywhere across a sweep of fractional positions.
    for (let x = 0; x <= 38; x += 0.7) {
      expect(railInPath(x, 12, 26, 6)).not.toMatch(/NaN/);
      expect(railOutPath(x, 12, 26, 6)).not.toMatch(/NaN/);
    }
  });
});

describe("lane geometry", () => {
  it("draws the full-lane budget at full spacing, then a narrow band", () => {
    expect(railX(0)).toBe(12);
    expect(railX(5, 6)).toBe(77);
    // The first narrow lane steps in closer than a full lane would.
    expect(railX(6, 6)).toBe(86);
    expect(railX(7, 6)).toBe(91);
    // Half-pixel positions interpolate, so a tweened count moves smoothly.
    expect(railX(2.5, 6)).toBe(44.5);
    expect(railX(6.5, 6)).toBe(88.5);
  });

  it("saturates past the narrow band so the gutter is bounded", () => {
    const last = railX(6 + NARROW_LANES - 1, 6);
    expect(railX(6 + NARROW_LANES, 6)).toBe(last);
    expect(railX(500, 6)).toBe(last);
    expect(gutterWidth(500, 6)).toBe(last + 12);
  });

  it("keeps the classic gutter for graphs inside the budget", () => {
    expect(gutterWidth(1)).toBe(24);
    expect(gutterWidth(2)).toBe(37);
    expect(gutterWidth(0)).toBe(24);
  });

  it("names each column's tier under a budget", () => {
    expect(laneTier(0, 6)).toBe("full");
    expect(laneTier(5, 6)).toBe("full");
    expect(laneTier(6, 6)).toBe("narrow");
    expect(laneTier(6 + NARROW_LANES - 1, 6)).toBe("narrow");
    expect(laneTier(6 + NARROW_LANES, 6)).toBe("overflow");
  });

  it("spends the pane's spare width on full lanes, never the text's", () => {
    // Everything fits at full spacing: every lane is full.
    expect(fullLanesFor(460, 3)).toBe(MIN_FULL_LANES);
    expect(fullLanesFor(460, 10)).toBe(10);
    // Twenty lanes in the default pane: as many full lanes as leave the
    // text its reserve once the rest are packed into the narrow band.
    const full = fullLanesFor(460, 20);
    expect(full).toBe(9);
    expect(gutterWidth(20, full)).toBeLessThanOrEqual(460 - 270);
    expect(gutterWidth(20, full + 1)).toBeGreaterThan(460 - 270);
    // A squeezed pane still keeps the minimum; a wide one caps at the max.
    expect(fullLanesFor(340, 20)).toBe(MIN_FULL_LANES);
    expect(fullLanesFor(1200, 40)).toBe(MAX_FULL_LANES);
  });
});
