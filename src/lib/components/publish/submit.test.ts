import { describe, expect, it } from "vitest";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SubmitAction } from "$lib/bindings/SubmitAction";
import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";
import { actionRow, publishableStacks } from "./submit";

function node(id: string, bookmarks: string[] = []): GraphNode {
  return {
    id,
    changeId: id,
    commitId: `c-${id}`,
    description: `change ${id}`,
    author: "t@example.com",
    timestamp: "2026-07-01T12:00:00Z",
    kind: "mutable",
    parents: [],
    elidedParents: [],
    bookmarks,
    isEmpty: false,
    hasConflict: false,
    isDivergent: false,
  };
}

function bookmark(
  name: string,
  overrides: Partial<BookmarkState> = {},
): BookmarkState {
  return {
    name,
    target: "x",
    remote: null,
    sync: "localOnly",
    isTrunk: false,
    isLocal: true,
    ...overrides,
  };
}

function stream(
  id: string,
  nodeIds: string[],
  isActive = false,
): WorkstreamSummary {
  return {
    id,
    title: `Stream ${id}`,
    nodeIds,
    bookmark: null,
    isActive,
    behindTrunk: 0,
  };
}

function snapshot(
  nodes: GraphNode[],
  bookmarks: BookmarkState[],
  workstreams: WorkstreamSummary[],
): RepoSnapshot {
  return {
    repoPath: "/tmp/repo",
    repoName: "repo",
    backend: "test",
    trunkBookmark: "main",
    workingCopy: nodes[0]?.id ?? "",
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

describe("publishableStacks", () => {
  it("finds the top-most bookmarked change and counts from it", () => {
    const snap = snapshot(
      // wc on top (unbookmarked), the bookmark one below, two more under.
      [node("wc"), node("b", ["feat"]), node("c"), node("d")],
      [bookmark("feat"), bookmark("main", { isTrunk: true })],
      [stream("s1", ["wc", "b", "c", "d"], true)],
    );
    expect(publishableStacks(snap, new Set())).toEqual([
      {
        workstreamId: "s1",
        title: "Stream s1",
        headBookmark: "feat",
        isActive: true,
        changeCount: 3,
      },
    ]);
  });

  it("skips bookmarkless streams and lists the active stack first", () => {
    const snap = snapshot(
      [node("a", ["one"]), node("b"), node("c", ["two"])],
      [bookmark("one"), bookmark("two")],
      [
        stream("anon", ["b"]),
        stream("other", ["a"]),
        stream("active", ["c"], true),
      ],
    );
    const stacks = publishableStacks(snap, new Set());
    expect(stacks.map((s) => s.workstreamId)).toEqual(["active", "other"]);
  });

  it("ignores trunk and non-local names; PR-known names win shared changes", () => {
    const snap = snapshot(
      [node("a", ["main", "zeta", "beta"])],
      [
        bookmark("main", { isTrunk: true }),
        bookmark("zeta"),
        bookmark("beta"),
      ],
      [stream("s1", ["a"], true)],
    );
    // Alphabetical without PR knowledge…
    expect(publishableStacks(snap, new Set())[0].headBookmark).toBe("beta");
    // …but a branch GitHub already has a PR for wins.
    expect(publishableStacks(snap, new Set(["zeta"]))[0].headBookmark).toBe(
      "zeta",
    );
  });
});

describe("actionRow", () => {
  it("phrases each action kind", () => {
    const push: SubmitAction = { kind: "push", bookmark: "feat", create: true };
    expect(actionRow(push, "origin").text).toContain("new branch on origin");
    const update: SubmitAction = {
      kind: "push",
      bookmark: "feat",
      create: false,
    };
    expect(actionRow(update, "origin").text).toBe("Push feat to origin");
    const create: SubmitAction = {
      kind: "createPr",
      bookmark: "feat",
      base: "main",
      title: "feat: thing",
      body: "",
    };
    expect(actionRow(create, "origin").text).toContain("feat → main");
    const retarget: SubmitAction = {
      kind: "retargetPr",
      number: 7n,
      bookmark: "feat",
      fromBase: "main",
      toBase: "auth",
    };
    expect(actionRow(retarget, "origin").text).toBe(
      "Retarget #7: base main → auth",
    );
  });

  it("phrases text updates by what changes", () => {
    const bodyOnly: SubmitAction = {
      kind: "updatePrText",
      number: 7n,
      bookmark: "feat",
      title: null,
      body: "wrapped",
      seed: false,
    };
    expect(actionRow(bodyOnly, "origin").text).toBe(
      "Update #7’s description from feat’s commit text",
    );
    const withTitle: SubmitAction = { ...bodyOnly, title: "feat: renamed" };
    expect(actionRow(withTitle, "origin").text).toContain(
      "title and description",
    );
    const seed: SubmitAction = { ...bodyOnly, seed: true };
    expect(actionRow(seed, "origin").text).toContain("Adopt #7’s description");
  });

  it("phrases comment syncs for existing and not-yet-created PRs", () => {
    const onExisting: SubmitAction = {
      kind: "syncStackComment",
      bookmark: "feat",
      number: 7n,
      create: false,
    };
    expect(actionRow(onExisting, "origin").text).toBe(
      "Update the stack comment on #7",
    );
    const onCreated: SubmitAction = {
      kind: "syncStackComment",
      bookmark: "feat",
      number: null,
      create: true,
    };
    expect(actionRow(onCreated, "origin").text).toBe(
      "Post the stack comment on feat’s new pull request",
    );
    const firstOnExisting: SubmitAction = {
      kind: "syncStackComment",
      bookmark: "feat",
      number: 7n,
      create: true,
    };
    expect(actionRow(firstOnExisting, "origin").text).toBe(
      "Post the stack comment on #7",
    );
  });
});
