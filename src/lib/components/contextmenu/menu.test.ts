import { describe, expect, it } from "vitest";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { FileDiff } from "$lib/bindings/FileDiff";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { NodeKind } from "$lib/bindings/NodeKind";
import type { PrSummary } from "$lib/bindings/PrSummary";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import {
  buildMenu,
  type ChangeTarget,
  type DiffTarget,
  type MenuContext,
  type MenuItem,
  type MenuTarget,
} from "./menu";

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
    tags: [],
    isEmpty: false,
    hasConflict: false,
    isDivergent: false,
  };
}

function snapshot(nodes: GraphNode[] = []): RepoSnapshot {
  return {
    repoPath: "/tmp/repo",
    repoName: "repo",
    backend: "mock",
    view: { kind: "workstreams" },
    truncated: false,
    trunkBookmark: "",
    workingCopy: "",
    workspaces: [],
    workstreams: [],
    nodes,
    bookmarks: [],
    gitRemotes: [],
    conflicts: [],
    operations: [],
    resolveTool: null,
  };
}

function bookmark(overrides: Partial<BookmarkState> = {}): BookmarkState {
  return {
    name: "feature",
    target: "aaaa",
    remote: null,
    sync: "localOnly",
    isTrunk: false,
    isLocal: true,
    tracked: false,
    isMerged: false,
    targetDescription: "",
    targetAuthor: "",
    targetTimestamp: "",
    ...overrides,
  };
}

function pr(overrides: Partial<PrSummary> = {}): PrSummary {
  return {
    number: 7n,
    title: "a change",
    url: "https://github.com/o/r/pull/7",
    state: "open",
    isDraft: false,
    headBranch: "feature",
    headCommit: "abc",
    headOwner: "o",
    baseBranch: "main",
    body: null,
    review: "none",
    checks: "passing",
    ...overrides,
  };
}

function file(overrides: Partial<FileDiff> = {}): FileDiff {
  return {
    path: "src/lib.rs",
    status: "modified",
    renamedFrom: null,
    hasConflict: false,
    content: {
      kind: "text",
      hunks: [
        {
          oldStart: 1,
          newStart: 1,
          context: null,
          lines: [
            { kind: "context", segments: [{ text: "a", changed: false }], pair: null },
            { kind: "added", segments: [{ text: "b", changed: true }], pair: null },
          ],
        },
      ],
      truncated: false,
    },
    ...overrides,
  };
}

function changeTarget(
  target: GraphNode,
  overrides: Partial<Omit<ChangeTarget, "kind" | "node">> = {},
): ChangeTarget {
  return {
    kind: "change",
    node: target,
    compare: { trunk: false, base: false, trunkName: "main" },
    landBookmark: null,
    shippable: false,
    ...overrides,
  };
}

function diffTarget(overrides: Partial<Omit<DiffTarget, "kind">> = {}): DiffTarget {
  return {
    kind: "diff",
    file: file(),
    changeId: "aaaa",
    mutable: true,
    line: { old: 1, new: 1 },
    resolveTool: null,
    ...overrides,
  };
}

function ctx(
  target: MenuTarget,
  overrides: Partial<Omit<MenuContext, "target">> = {},
): MenuContext {
  return {
    snapshot: snapshot(),
    selection: "",
    target,
    editors: [],
    canUndo: false,
    ...overrides,
  };
}

function ids(sections: MenuItem[][]): string[][] {
  return sections.map((section) => section.map((item) => item.id));
}

function find(sections: MenuItem[][], id: string): MenuItem | undefined {
  for (const section of sections) {
    for (const item of section) {
      if (item.id === id) return item;
      const nested = item.children && find(item.children, id);
      if (nested) return nested;
    }
  }
  return undefined;
}

describe("buildMenu: text fields", () => {
  it("gives a text field only editing commands", () => {
    const sections = buildMenu(
      ctx(
        { kind: "editable", hasSelection: true, readOnly: false },
        { selection: "picked text" },
      ),
    );
    expect(ids(sections)).toEqual([
      ["edit.cut", "edit.copy", "edit.paste"],
      ["edit.selectAll"],
    ]);
    expect(sections.flat().every((item) => !item.disabled)).toBe(true);
  });

  it("disables cut/copy without a selection and cut/paste when read-only", () => {
    const noSelection = buildMenu(
      ctx({ kind: "editable", hasSelection: false, readOnly: false }),
    );
    expect(find(noSelection, "edit.cut")?.disabled).toBe(true);
    expect(find(noSelection, "edit.copy")?.disabled).toBe(true);
    expect(find(noSelection, "edit.paste")?.disabled).toBeFalsy();

    const readOnly = buildMenu(
      ctx({ kind: "editable", hasSelection: true, readOnly: true }),
    );
    expect(find(readOnly, "edit.cut")?.disabled).toBe(true);
    expect(find(readOnly, "edit.paste")?.disabled).toBe(true);
  });
});

describe("buildMenu: changes", () => {
  it("offers a mutable change its full command set plus copyable ids", () => {
    const target = node("aaaa", "mutable", ["bbbb"]);
    const snap = snapshot([target, node("bbbb", "mutable")]);
    const sections = buildMenu(ctx(changeTarget(target), { snapshot: snap }));
    expect(ids(sections)).toEqual([
      [
        "change.describe",
        "change.new",
        "change.edit",
        "change.duplicate",
        "change.revert",
        "change.bookmark",
        "change.rebase",
        "change.split",
        "change.squash",
        "change.absorb",
        "change.abandon",
      ],
      ["change.compare"],
      ["change.copyChangeId", "change.copyCommitId", "change.copyDescription"],
    ]);
    expect(find(sections, "change.abandon")?.danger).toBe(true);
    expect(find(sections, "change.copyChangeId")?.action).toEqual({
      type: "copyText",
      text: "aaaa",
    });
  });

  it("offers an immutable change only the non-rewriting commands", () => {
    const target = node("base", "immutable");
    const sections = buildMenu(
      ctx(changeTarget(target), { snapshot: snapshot([target]) }),
    );
    expect(ids(sections)[0]).toEqual([
      "change.new",
      "change.duplicate",
      "change.revert",
      "change.bookmark",
    ]);
  });

  it("builds the compare submenu from what resolves", () => {
    const target = node("aaaa", "mutable");
    const sections = buildMenu(
      ctx(changeTarget(target, { compare: { trunk: true, base: false, trunkName: "main" } }), {
        snapshot: snapshot([target]),
      }),
    );
    const compare = find(sections, "change.compare");
    expect(compare?.children).toBeDefined();
    expect(ids(compare?.children ?? [])).toEqual([
      ["change.compare.parent", "change.compare.trunk", "change.compare.pick"],
    ]);
    expect(find(sections, "change.compare.trunk")?.label).toBe("Against main");
  });

  it("offers land and ship on stack heads", () => {
    const target = node("aaaa", "mutable");
    const sections = buildMenu(
      ctx(changeTarget(target, { landBookmark: "feature", shippable: true }), {
        snapshot: snapshot([target]),
      }),
    );
    expect(find(sections, "change.land")?.label).toBe("Land feature…");
    expect(find(sections, "change.ship")?.action).toEqual({
      type: "intent",
      intent: { kind: "ship", head: "aaaa" },
    });
  });

  it("skips absorb and revert on empty changes", () => {
    const target = { ...node("aaaa", "mutable"), isEmpty: true };
    const sections = buildMenu(
      ctx(changeTarget(target), { snapshot: snapshot([target]) }),
    );
    expect(find(sections, "change.absorb")).toBeUndefined();
    expect(find(sections, "change.revert")).toBeUndefined();
  });
});

describe("buildMenu: diff pane", () => {
  const editors = [
    { id: "zed", label: "Zed" },
    { id: "vscode", label: "VS Code" },
  ];

  it("opens the file at the clicked line", () => {
    const snap = snapshot([node("aaaa", "workingCopy")]);
    const sections = buildMenu(ctx(diffTarget(), { snapshot: snap, editors }));
    expect(find(sections, "diff.openEditor")?.action).toEqual({
      type: "openInEditor",
      editor: "zed",
      path: "/tmp/repo/src/lib.rs",
      line: 1,
    });
    // Both installed editors appear in the Open With flyout.
    expect(ids(find(sections, "diff.openWith")?.children ?? [])).toEqual([
      ["diff.openWith.zed", "diff.openWith.vscode"],
    ]);
  });

  it("falls back to the old line number on deleted rows and omits editor items for removed files", () => {
    const snap = snapshot([node("aaaa", "mutable")]);
    const deleted = buildMenu(
      ctx(diffTarget({ line: { old: 5, new: null } }), { snapshot: snap, editors }),
    );
    expect(find(deleted, "diff.openEditor")?.action).toMatchObject({ line: 5 });

    const removed = buildMenu(
      ctx(diffTarget({ file: file({ status: "removed" }) }), {
        snapshot: snap,
        editors,
      }),
    );
    expect(find(removed, "diff.openEditor")).toBeUndefined();
    expect(find(removed, "diff.copyPath")).toBeDefined();
  });

  it("offers patch copies, file moves, and discard on a mutable change", () => {
    const snap = snapshot([node("aaaa", "mutable")]);
    const sections = buildMenu(ctx(diffTarget(), { snapshot: snap }));
    expect(find(sections, "diff.copyHunk")?.action).toEqual({
      type: "copyPatch",
      scope: "hunk",
    });
    expect(find(sections, "diff.copyFilePatch")).toBeDefined();
    expect(find(sections, "diff.moveFile")?.action).toEqual({
      type: "intent",
      intent: { kind: "split", into: true, path: "src/lib.rs" },
    });
    const discard = find(sections, "diff.restoreFile");
    expect(discard?.danger).toBe(true);
    expect(discard?.action).toEqual({
      type: "restoreFile",
      changeId: "aaaa",
      path: "src/lib.rs",
    });
  });

  it("offers a per-hunk discard only where hunk coordinates are trustworthy", () => {
    const snap = snapshot([node("aaaa", "mutable")]);
    const hunks = [
      {
        oldStart: 1,
        newStart: 1,
        context: null,
        lines: [
          { kind: "context" as const, segments: [{ text: "a", changed: false }], pair: null },
          { kind: "added" as const, segments: [{ text: "b", changed: true }], pair: null },
        ],
      },
      {
        oldStart: 10,
        newStart: 11,
        context: null,
        lines: [
          { kind: "context" as const, segments: [{ text: "c", changed: false }], pair: null },
          { kind: "removed" as const, segments: [{ text: "d", changed: true }], pair: null },
          { kind: "added" as const, segments: [{ text: "e", changed: true }], pair: null },
        ],
      },
    ];
    const twoHunks = file({
      content: { kind: "text", hunks, truncated: false },
    });

    // A click on the second hunk's added row embeds that hunk's rendered
    // coordinates, which the backend re-verifies before discarding.
    const sections = buildMenu(
      ctx(diffTarget({ file: twoHunks, line: { old: null, new: 12 } }), {
        snapshot: snap,
      }),
    );
    const discard = find(sections, "diff.restoreHunk");
    expect(discard?.danger).toBe(true);
    expect(discard?.action).toEqual({
      type: "restoreHunk",
      changeId: "aaaa",
      path: "src/lib.rs",
      hunk: { oldStart: 10, newStart: 11, oldLines: 2, newLines: 2, lines: null },
    });

    // The clicked row itself — the added line at index 2 of the hunk's
    // rendered lines — rides the same action as a line pick.
    const discardLine = find(sections, "diff.restoreLine");
    expect(discardLine?.danger).toBe(true);
    expect(discardLine?.action).toEqual({
      type: "restoreHunk",
      changeId: "aaaa",
      path: "src/lib.rs",
      hunk: { oldStart: 10, newStart: 11, oldLines: 2, newLines: 2, lines: [2] },
    });

    // A hunk with a single changed line offers no line discard (the line
    // is the hunk); a context row carries no change to pick, though the
    // hunk item still shows for the hunk under the pointer.
    const firstHunk = buildMenu(
      ctx(diffTarget({ file: twoHunks, line: { old: null, new: 2 } }), {
        snapshot: snap,
      }),
    );
    expect(find(firstHunk, "diff.restoreHunk")).toBeDefined();
    expect(find(firstHunk, "diff.restoreLine")).toBeUndefined();
    const contextRow = buildMenu(
      ctx(diffTarget({ file: twoHunks, line: { old: 10, new: 11 } }), {
        snapshot: snap,
      }),
    );
    expect(find(contextRow, "diff.restoreHunk")).toBeDefined();
    expect(find(contextRow, "diff.restoreLine")).toBeUndefined();

    // No offer on a single-hunk file (that is the whole file), on a
    // truncated diff (coordinates untrustworthy), or off-row.
    expect(
      find(buildMenu(ctx(diffTarget(), { snapshot: snap })), "diff.restoreHunk"),
    ).toBeUndefined();
    const truncated = file({
      content: { kind: "text", hunks, truncated: true },
    });
    expect(
      find(
        buildMenu(
          ctx(diffTarget({ file: truncated, line: { old: null, new: 12 } }), {
            snapshot: snap,
          }),
        ),
        "diff.restoreHunk",
      ),
    ).toBeUndefined();
    expect(
      find(
        buildMenu(ctx(diffTarget({ file: twoHunks, line: null }), { snapshot: snap })),
        "diff.restoreHunk",
      ),
    ).toBeUndefined();
  });

  it("keeps an immutable change's diff read-only and skips hunk copy off-row", () => {
    const snap = snapshot([node("aaaa", "immutable")]);
    const sections = buildMenu(
      ctx(diffTarget({ mutable: false, line: null }), {
        snapshot: snap,
      }),
    );
    expect(find(sections, "diff.restoreFile")).toBeUndefined();
    expect(find(sections, "diff.moveFile")).toBeUndefined();
    expect(find(sections, "diff.copyHunk")).toBeUndefined();
    expect(find(sections, "diff.copyFilePatch")).toBeDefined();
  });

  it("offers resolve on a conflicted file with a tool", () => {
    const snap = snapshot([node("aaaa", "mutable")]);
    const sections = buildMenu(
      ctx(diffTarget({ file: file({ hasConflict: true }), resolveTool: "meld" }), {
        snapshot: snap,
      }),
    );
    expect(find(sections, "diff.resolve")?.action).toEqual({
      type: "resolveConflict",
      changeId: "aaaa",
      path: "src/lib.rs",
    });
  });

  it("offers annotate on text-shaped files that exist in the change", () => {
    const snap = snapshot([node("aaaa", "mutable")]);
    expect(
      find(buildMenu(ctx(diffTarget(), { snapshot: snap })), "diff.annotate")
        ?.action,
    ).toEqual({ type: "annotateFile", changeId: "aaaa", path: "src/lib.rs" });

    // A read verb: immutable changes offer it too.
    const immutable = snapshot([node("aaaa", "immutable")]);
    expect(
      find(
        buildMenu(ctx(diffTarget({ mutable: false }), { snapshot: immutable })),
        "diff.annotate",
      ),
    ).toBeDefined();

    // "omitted" is text whose diff ran over the render budget — the file
    // itself still annotates.
    expect(
      find(
        buildMenu(
          ctx(diffTarget({ file: file({ content: { kind: "omitted" } }) }), {
            snapshot: snap,
          }),
        ),
        "diff.annotate",
      ),
    ).toBeDefined();

    // Nothing to annotate: the file is gone from this change's tree, or
    // its content is not line-shaped (the backend would refuse).
    expect(
      find(
        buildMenu(
          ctx(diffTarget({ file: file({ status: "removed" }) }), {
            snapshot: snap,
          }),
        ),
        "diff.annotate",
      ),
    ).toBeUndefined();
    expect(
      find(
        buildMenu(
          ctx(
            diffTarget({
              file: file({
                content: { kind: "binary", oldSize: 1, newSize: 2 },
              }),
            }),
            { snapshot: snap },
          ),
        ),
        "diff.annotate",
      ),
    ).toBeUndefined();
  });
});

describe("buildMenu: bookmarks and PRs", () => {
  it("gives a local bookmark push, manage, copy, and delete", () => {
    const sections = buildMenu(
      ctx({
        kind: "bookmark",
        bookmark: bookmark(),
        pr: null,
        hasRemotes: true,
      }),
    );
    expect(ids(sections)).toEqual([
      ["bookmark.push", "bookmark.manage"],
      ["bookmark.copyName"],
      ["bookmark.delete"],
    ]);
  });

  it("gives a remote bookmark track/untrack instead, and trunk no delete", () => {
    const remote = buildMenu(
      ctx({
        kind: "bookmark",
        bookmark: bookmark({ isLocal: false, remote: "origin", tracked: false }),
        pr: null,
        hasRemotes: true,
      }),
    );
    expect(find(remote, "bookmark.track")).toBeDefined();
    expect(find(remote, "bookmark.push")).toBeUndefined();
    expect(find(remote, "bookmark.delete")).toBeUndefined();

    const trunk = buildMenu(
      ctx({
        kind: "bookmark",
        bookmark: bookmark({ isTrunk: true }),
        pr: null,
        hasRemotes: false,
      }),
    );
    expect(find(trunk, "bookmark.delete")).toBeUndefined();
    expect(find(trunk, "bookmark.push")).toBeUndefined();
  });

  it("leads a bookmark with its PR when one exists", () => {
    const sections = buildMenu(
      ctx({
        kind: "bookmark",
        bookmark: bookmark(),
        pr: pr(),
        hasRemotes: true,
      }),
    );
    expect(ids(sections)[0]).toEqual(["bookmark.openPr", "bookmark.copyPrUrl"]);
  });

  it("gives a PR badge open/copy plus re-run and fetch when they apply", () => {
    const sections = buildMenu(
      ctx({
        kind: "pr",
        pr: pr({ checks: "failing" }),
        remote: "origin",
        reviewBookmarkTaken: false,
      }),
    );
    expect(ids(sections)).toEqual([
      ["pr.open", "pr.copyUrl", "pr.copyNumber"],
      ["pr.rerunCi", "pr.fetch"],
    ]);
    expect(find(sections, "pr.fetch")?.action).toEqual({
      type: "fetchPr",
      remote: "origin",
      number: 7n,
      bookmark: "pr/7",
    });

    const passing = buildMenu(
      ctx({
        kind: "pr",
        pr: pr(),
        remote: null,
        reviewBookmarkTaken: false,
      }),
    );
    expect(find(passing, "pr.rerunCi")).toBeUndefined();
    expect(find(passing, "pr.fetch")).toBeUndefined();
  });
});

describe("buildMenu: operations, conflicts, breadcrumb", () => {
  it("offers restore+revert on past operations, revert-only on the current", () => {
    const past = buildMenu(
      ctx({ kind: "operation", opId: "op1", isCurrent: false }),
    );
    expect(ids(past)).toEqual([["op.restore", "op.revert"], ["op.copyId"]]);

    const current = buildMenu(
      ctx({ kind: "operation", opId: "op2", isCurrent: true }),
    );
    expect(find(current, "op.restore")).toBeUndefined();
    expect(find(current, "op.revert")?.label).toBe("Undo This Operation");
  });

  it("offers resolve and copy path on conflict rows, editor only for the working copy", () => {
    const sections = buildMenu(
      ctx(
        {
          kind: "conflict",
          changeId: "aaaa",
          path: "src/x.rs",
          isWorkingCopy: true,
          resolveTool: "meld",
        },
        { editors: [{ id: "zed", label: "Zed" }] },
      ),
    );
    expect(find(sections, "conflict.resolve")).toBeDefined();
    expect(find(sections, "conflict.openEditor")?.action).toMatchObject({
      path: "/tmp/repo/src/x.rs",
    });

    const elsewhere = buildMenu(
      ctx(
        {
          kind: "conflict",
          changeId: "bbbb",
          path: "src/x.rs",
          isWorkingCopy: false,
          resolveTool: null,
        },
        { editors: [{ id: "zed", label: "Zed" }] },
      ),
    );
    expect(find(elsewhere, "conflict.openEditor")).toBeUndefined();
    expect(find(elsewhere, "conflict.resolve")).toBeUndefined();
    expect(find(elsewhere, "conflict.copyPath")).toBeDefined();
  });

  it("gives the breadcrumb undo and the operations timeline", () => {
    const sections = buildMenu(ctx({ kind: "breadcrumb" }, { canUndo: true }));
    expect(ids(sections)).toEqual([["breadcrumb.undo", "breadcrumb.ops"]]);
  });
});

describe("buildMenu: surface and selection", () => {
  it("gives the bare surface app commands plus the repo section", () => {
    const sections = buildMenu(
      ctx(
        { kind: "surface" },
        { editors: [{ id: "zed", label: "Zed" }], canUndo: true },
      ),
    );
    expect(ids(sections)).toEqual([
      ["app.palette", "app.refresh", "app.undo"],
      ["app.copyRepoPath", "app.reveal", "app.terminal", "app.openRepoEditor"],
    ]);
  });

  it("offers Open Repository before a repo is open", () => {
    const sections = buildMenu(ctx({ kind: "surface" }, { snapshot: null }));
    expect(ids(sections)).toEqual([["app.palette", "app.openRepo"]]);
  });

  it("prepends Copy for a text selection on any non-editable target", () => {
    const sections = buildMenu(
      ctx({ kind: "surface" }, { selection: "some diff text" }),
    );
    expect(ids(sections)[0]).toEqual(["copy.selection"]);
    expect(find(sections, "copy.selection")?.action).toEqual({
      type: "copyText",
      text: "some diff text",
    });
  });
});
