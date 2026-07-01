import { describe, expect, it } from "vitest";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { NodeKind } from "$lib/bindings/NodeKind";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";
import { paletteResults, type PaletteContext } from "./palette";

function node(id: string, kind: NodeKind, parents: string[] = []): GraphNode {
  return {
    id,
    changeId: id,
    commitId: `c${id}`,
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

function bookmark(
  name: string,
  target: string,
  isTrunk = false,
): BookmarkState {
  return {
    name,
    target,
    remote: null,
    sync: "localOnly",
    isTrunk,
    isLocal: true,
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
    trunkBookmark: bookmarks.find((b) => b.isTrunk)?.name ?? "",
    workingCopy: "",
    workspaces: [],
    workstreams,
    nodes,
    bookmarks,
    conflicts: [],
    operations: [],
  };
}

function ctx(overrides: Partial<PaletteContext> = {}): PaletteContext {
  return {
    snapshot: null,
    selected: null,
    recentRepos: [],
    canUndo: false,
    registered: false,
    themes: [],
    ...overrides,
  };
}

// A stack on trunk: wc → mid → base(main).
function stackContext(): PaletteContext {
  const nodes = [
    node("wxyz", "workingCopy", ["mnop"]),
    { ...node("mnop", "mutable", ["qrst"]), bookmarks: ["feature"] },
    node("qrst", "mutable", ["base"]),
    { ...node("base", "immutable"), bookmarks: ["main"] },
  ];
  const snap = snapshot(
    nodes,
    [workstream("ws", ["wxyz", "mnop", "qrst"])],
    [bookmark("main", "base", true), bookmark("feature", "mnop")],
  );
  return ctx({ snapshot: snap, selected: nodes[1] });
}

const ids = (context: PaletteContext, query = "") =>
  paletteResults(context, query).map((item) => item.id);

describe("paletteResults availability", () => {
  it("offers only repository commands before a repo is open", () => {
    const items = ids(
      ctx({
        recentRepos: [
          { path: "/tmp/other", name: "other", lastOpenedAt: 1 },
        ],
      }),
    );
    expect(items).toEqual(["repo.open", "repo.recent./tmp/other"]);
  });

  it("offers the full action set on a mutable change with a mutable parent", () => {
    const items = ids(stackContext());
    for (const id of [
      "change.describe",
      "change.new",
      "change.edit",
      "change.bookmark",
      "change.rebase",
      "change.squash",
      "change.abandon",
      "compare.parent",
      "compare.trunk",
      "compare.base",
      "compare.pick",
      "view.graph",
      "view.focus",
      "layout.unified",
      "layout.split",
      "section.operations",
      "repo.open",
      "repo.refresh",
    ]) {
      expect(items).toContain(id);
    }
  });

  it("offers only the non-rewriting actions on an immutable change", () => {
    const context = stackContext();
    context.selected = context.snapshot!.nodes[3];
    const items = ids(context);
    expect(items).toContain("change.new");
    expect(items).toContain("change.bookmark");
    for (const id of [
      "change.describe",
      "change.edit",
      "change.rebase",
      "change.squash",
      "change.abandon",
    ]) {
      expect(items).not.toContain(id);
    }
  });

  it("withholds squash when the parent is immutable, edit when already the working copy", () => {
    const context = stackContext();
    // qrst sits directly on immutable base.
    context.selected = context.snapshot!.nodes[2];
    expect(ids(context)).not.toContain("change.squash");
    // The working copy cannot be `jj edit`ed into again.
    context.selected = context.snapshot!.nodes[0];
    const items = ids(context);
    expect(items).not.toContain("change.edit");
    expect(items).toContain("change.squash");
  });

  it("drops compare presets that cannot resolve", () => {
    const context = stackContext();
    // No trunk bookmark: no trunk preset. No workstream claim: no base.
    context.snapshot = snapshot(context.snapshot!.nodes, [], []);
    context.selected = context.snapshot.nodes[1];
    const items = ids(context);
    expect(items).not.toContain("compare.trunk");
    expect(items).not.toContain("compare.base");
    expect(items).toContain("compare.parent");
    expect(items).toContain("compare.pick");
  });

  it("offers undo only while the breadcrumb's operation is around", () => {
    expect(ids(stackContext())).not.toContain("repo.undo");
    expect(ids({ ...stackContext(), canUndo: true })).toContain("repo.undo");
  });
});

describe("paletteResults query", () => {
  it("hides secondary rows until the query matches them", () => {
    const context = {
      ...stackContext(),
      registered: true,
      themes: [{ id: "ember", label: "Ember", scheme: "dark" as const }],
      recentRepos: [{ path: "/tmp/other", name: "other", lastOpenedAt: 1 }],
    };
    const unqueried = ids(context);
    expect(unqueried).not.toContain("theme.ember");
    expect(unqueried).not.toContain("repo.recent./tmp/other");
    expect(ids(context, "ember")).toContain("theme.ember");
    expect(ids(context, "other")).toContain("repo.recent./tmp/other");
  });

  it("offers no theme rows on an unregistered copy", () => {
    const context = {
      ...stackContext(),
      themes: [{ id: "ember", label: "Ember", scheme: "dark" as const }],
    };
    expect(ids(context, "ember")).not.toContain("theme.ember");
  });

  it("ranks title matches above keyword matches", () => {
    // "diff" is a title word for the layout rows but only a keyword for
    // the compare presets.
    const items = ids(stackContext(), "diff");
    expect(items[0]).toBe("layout.unified");
    expect(items).toContain("compare.parent");
    expect(items.indexOf("compare.parent")).toBeGreaterThan(
      items.indexOf("layout.split"),
    );
  });

  it("drops commands the query does not match", () => {
    const items = ids(stackContext(), "zzzz-no-such");
    expect(items).toEqual([]);
  });
});

describe("paletteResults go-to-change", () => {
  it("matches changes by id prefix, bookmark, and title, excluding the selection", () => {
    const context = stackContext();
    expect(ids(context, "qrst")).toContain("goto.qrst");
    // "feature" points at the selected change itself — excluded.
    expect(ids(context, "feature")).not.toContain("goto.mnop");
    context.selected = context.snapshot!.nodes[0];
    expect(ids(context, "feature")).toContain("goto.mnop");
    expect(ids(context, "change qrst")).toContain("goto.qrst");
  });

  it("caps jump rows and keeps snapshot order", () => {
    const many = Array.from({ length: 12 }, (_, i) =>
      node(`node${String(i).padStart(2, "0")}`, "mutable"),
    );
    const context = ctx({ snapshot: snapshot(many), selected: null });
    const jumps = ids(context, "node").filter((id) => id.startsWith("goto."));
    expect(jumps).toHaveLength(8);
    expect(jumps[0]).toBe("goto.node00");
  });
});
