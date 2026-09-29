// Pure model for the custom right-click menu: what one right-click target
// offers. The component resolves the DOM under the pointer into a
// `MenuTarget`, this module turns it into sections, and the component
// executes the returned actions; nothing here touches Svelte or app state
// (context comes in as a plain value so tests can drive every shape).
//
// One subject owns the menu: a text field gets only editing commands (like
// a native macOS field), a graph row gets its change's commands, a diff
// region its file's, and the bare surface app-level commands. A text
// selection adds Copy on top of whatever else is offered.

import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { FileDiff } from "$lib/bindings/FileDiff";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { PrSummary } from "$lib/bindings/PrSummary";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SplitHunk } from "$lib/bindings/SplitHunk";
import type { EditorInfo } from "$lib/api";
import type { IconName } from "$lib/components/ui/icons";
import type { Section, UiIntent } from "$lib/state/app.svelte";
import { actionAvailability } from "$lib/components/inspector/inspect";
import { mergeToolLabel } from "$lib/components/conflicts/conflicts";
import { canRerun, reviewBookmark } from "$lib/components/publish/review";
import { hunkAt, lineIndexAt } from "$lib/components/diff/diff";
import { canSelectHunks, canSelectLines, hunkCoords } from "$lib/components/diff/split";

// What runs when an item is picked. `clipboard` ops act on the text field
// that was right-clicked; `copyPatch` serializes from the diff target the
// component holds (lazily — a whole-file patch can be large); `intent`
// rows hand off to the surface that owns the matching UI, exactly like
// the palette's do — the right-clicked row becomes the selection first.
export type MenuAction =
  | { type: "clipboard"; op: "cut" | "copy" | "paste" | "selectAll" }
  | { type: "copyText"; text: string }
  | { type: "copyPatch"; scope: "hunk" | "file" }
  | { type: "newChild"; id: string }
  | { type: "edit"; id: string }
  | { type: "duplicate"; id: string }
  | { type: "revert"; id: string }
  | { type: "absorb"; id: string }
  | { type: "intent"; intent: UiIntent }
  | { type: "palette" }
  | { type: "refresh" }
  | { type: "chooseRepo" }
  | { type: "undo" }
  | { type: "section"; section: Section }
  | { type: "openUrl"; url: string }
  | { type: "openInEditor"; editor: string; path: string; line: number | null }
  | { type: "revealInFinder"; path: string }
  | { type: "openInTerminal"; path: string }
  | { type: "resolveConflict"; changeId: string; path: string }
  | { type: "annotateFile"; changeId: string; path: string }
  | { type: "restoreFile"; changeId: string; path: string }
  | { type: "restoreHunk"; changeId: string; path: string; hunk: SplitHunk }
  | { type: "restoreOperation"; opId: string }
  | { type: "revertOperation"; opId: string }
  | { type: "pushBookmark"; name: string }
  | { type: "trackBookmark"; name: string; remote: string }
  | { type: "untrackBookmark"; name: string; remote: string }
  | { type: "deleteBookmark"; name: string }
  | { type: "rerunCi"; number: number | bigint }
  | { type: "fetchPr"; remote: string; number: number | bigint; bookmark: string };

export interface MenuItem {
  /** Stable id, used as the render key and by tests. */
  id: string;
  label: string;
  icon?: IconName;
  shortcut?: string;
  danger?: boolean;
  disabled?: boolean;
  /** Absent on submenu parents — they open `children` instead. */
  action?: MenuAction;
  /** Flyout submenu; renders with a chevron, opens on hover/ArrowRight. */
  children?: MenuSection[];
}

/** Items render in order; a separator is drawn between sections. */
export type MenuSection = MenuItem[];

// ── Targets: what the pointer was over, resolved by the component ──

export interface ChangeTarget {
  kind: "change";
  node: GraphNode;
  /** Which compare presets resolve for this node (palette's rule). */
  compare: { trunk: boolean; base: boolean; trunkName: string };
  /** Set when the node heads a landable stack: the bookmark to land. */
  landBookmark: string | null;
  /** True when the node heads a shippable stack. */
  shippable: boolean;
}

export interface DiffTarget {
  kind: "diff";
  file: FileDiff;
  /** The change whose diff is on screen. */
  changeId: string;
  /** The change is mutable (restore/move-file act on it). */
  mutable: boolean;
  /** Line under the pointer, when the click landed on a code row. */
  line: { old: number | null; new: number | null } | null;
  /** Merge tool label when this file's conflict can be resolved. */
  resolveTool: string | null;
}

export interface BookmarkTarget {
  kind: "bookmark";
  bookmark: BookmarkState;
  pr: PrSummary | null;
  hasRemotes: boolean;
}

export interface PrTarget {
  kind: "pr";
  pr: PrSummary;
  /** Remote to fetch the head from, when known (forge's remote). */
  remote: string | null;
  /** A local bookmark already answering to the review name. */
  reviewBookmarkTaken: boolean;
}

export interface OperationTarget {
  kind: "operation";
  opId: string;
  isCurrent: boolean;
}

export interface ConflictTarget {
  kind: "conflict";
  changeId: string | null;
  path: string;
  isWorkingCopy: boolean;
  /** Merge tool label when resolvable (mutable change, tool configured). */
  resolveTool: string | null;
}

export type MenuTarget =
  | { kind: "editable"; hasSelection: boolean; readOnly: boolean }
  | ChangeTarget
  | DiffTarget
  | BookmarkTarget
  | PrTarget
  | OperationTarget
  | ConflictTarget
  | { kind: "breadcrumb" }
  | { kind: "surface" };

export interface MenuContext {
  snapshot: RepoSnapshot | null;
  /** The window's text selection ("" when none or when in a field). */
  selection: string;
  target: MenuTarget;
  /** Detected editor CLIs, last-used first. Empty hides editor items. */
  editors: EditorInfo[];
  /** Whether the breadcrumb's undo target is still around. */
  canUndo: boolean;
}

// ── Shared pieces ──

function copyItem(id: string, label: string, text: string): MenuItem {
  return { id, label, icon: "copy", action: { type: "copyText", text } };
}

/// "Open in Zed" for the preferred editor, plus an "Open With" flyout
/// when several are installed. Empty when none are (or the file is gone).
function editorItems(
  editors: EditorInfo[],
  idPrefix: string,
  path: string,
  line: number | null,
): MenuItem[] {
  if (editors.length === 0) return [];
  const open = (editor: EditorInfo): MenuAction => ({
    type: "openInEditor",
    editor: editor.id,
    path,
    line,
  });
  const items: MenuItem[] = [
    {
      id: `${idPrefix}.openEditor`,
      label: `Open in ${editors[0].label}`,
      icon: "code",
      action: open(editors[0]),
    },
  ];
  if (editors.length > 1) {
    items.push({
      id: `${idPrefix}.openWith`,
      label: "Open With",
      icon: "code",
      children: [
        editors.map((editor) => ({
          id: `${idPrefix}.openWith.${editor.id}`,
          label: editor.label,
          action: open(editor),
        })),
      ],
    });
  }
  return items;
}

// ── Per-target builders ──

function editableMenu(editable: {
  hasSelection: boolean;
  readOnly: boolean;
}): MenuSection[] {
  const { hasSelection, readOnly } = editable;
  return [
    [
      {
        id: "edit.cut",
        label: "Cut",
        icon: "scissors",
        shortcut: "⌘X",
        disabled: !hasSelection || readOnly,
        action: { type: "clipboard", op: "cut" },
      },
      {
        id: "edit.copy",
        label: "Copy",
        icon: "copy",
        shortcut: "⌘C",
        disabled: !hasSelection,
        action: { type: "clipboard", op: "copy" },
      },
      {
        id: "edit.paste",
        label: "Paste",
        icon: "clipboard",
        shortcut: "⌘V",
        disabled: readOnly,
        action: { type: "clipboard", op: "paste" },
      },
    ],
    [
      {
        id: "edit.selectAll",
        label: "Select All",
        icon: "textSelect",
        shortcut: "⌘A",
        action: { type: "clipboard", op: "selectAll" },
      },
    ],
  ];
}

function changeMenu(snapshot: RepoSnapshot, target: ChangeTarget): MenuSection[] {
  const { node, compare } = target;
  const avail = actionAvailability(snapshot, node);
  const change: MenuSection = [];
  if (avail.describe) {
    change.push({
      id: "change.describe",
      label: node.description.trim() ? "Edit Description…" : "Describe Change…",
      icon: "edit",
      action: { type: "intent", intent: { kind: "describe" } },
    });
  }
  change.push({
    id: "change.new",
    label: "New Child Change",
    icon: "plus",
    action: { type: "newChild", id: node.id },
  });
  if (avail.edit) {
    change.push({
      id: "change.edit",
      label: "Edit Change",
      icon: "atSign",
      action: { type: "edit", id: node.id },
    });
  }
  change.push({
    id: "change.duplicate",
    label: "Duplicate Change",
    icon: "copy",
    action: { type: "duplicate", id: node.id },
  });
  // Revert backs out the change's diff as a new change on `@` — pointless
  // for a change with no diff.
  if (!node.isEmpty) {
    change.push({
      id: "change.revert",
      label: "Revert Change",
      icon: "undo",
      action: { type: "revert", id: node.id },
    });
  }
  change.push({
    id: "change.bookmark",
    label: "Bookmark…",
    icon: "bookmark",
    action: { type: "intent", intent: { kind: "bookmark" } },
  });
  if (avail.rebase) {
    change.push({
      id: "change.rebase",
      label: "Rebase…",
      icon: "rebase",
      action: { type: "intent", intent: { kind: "rebase" } },
    });
  }
  if (avail.split) {
    change.push({
      id: "change.split",
      label: "Split Change…",
      icon: "split",
      action: { type: "intent", intent: { kind: "split" } },
    });
  }
  if (avail.squash) {
    change.push({
      id: "change.squash",
      label: "Squash into Parent",
      icon: "squash",
      action: { type: "intent", intent: { kind: "squash" } },
    });
  }
  // Absorb sinks the change's hunks into the mutable ancestors that last
  // touched those lines — pointless for an empty change.
  if (node.kind !== "immutable" && !node.isEmpty) {
    change.push({
      id: "change.absorb",
      label: "Absorb into Ancestors",
      icon: "squash",
      action: { type: "absorb", id: node.id },
    });
  }
  if (avail.abandon) {
    change.push({
      id: "change.abandon",
      label: "Abandon Change",
      icon: "trash",
      danger: true,
      action: { type: "intent", intent: { kind: "abandon" } },
    });
  }

  const compareChildren: MenuSection = [
    {
      id: "change.compare.parent",
      label: "Against Parent",
      action: {
        type: "intent",
        intent: { kind: "compare", mode: { kind: "parent" } },
      },
    },
  ];
  if (compare.trunk) {
    compareChildren.push({
      id: "change.compare.trunk",
      label: `Against ${compare.trunkName || "trunk"}`,
      action: {
        type: "intent",
        intent: { kind: "compare", mode: { kind: "trunk" } },
      },
    });
  }
  if (compare.base) {
    compareChildren.push({
      id: "change.compare.base",
      label: "Against Stack Base",
      action: {
        type: "intent",
        intent: { kind: "compare", mode: { kind: "base" } },
      },
    });
  }
  compareChildren.push({
    id: "change.compare.pick",
    label: "Against Any Change…",
    action: { type: "intent", intent: { kind: "compare" } },
  });
  const view: MenuSection = [
    {
      id: "change.compare",
      label: "Compare",
      icon: "compare",
      children: [compareChildren],
    },
  ];

  const publish: MenuSection = [];
  if (target.landBookmark) {
    publish.push({
      id: "change.land",
      label: `Land ${target.landBookmark}…`,
      icon: "publish",
      action: {
        type: "intent",
        intent: { kind: "land", bookmark: target.landBookmark },
      },
    });
  }
  if (target.shippable) {
    publish.push({
      id: "change.ship",
      label: "Ship to Trunk…",
      icon: "publish",
      action: { type: "intent", intent: { kind: "ship", head: node.id } },
    });
  }

  const copies: MenuSection = [
    copyItem("change.copyChangeId", "Copy Change ID", node.changeId),
    copyItem("change.copyCommitId", "Copy Commit ID", node.commitId),
  ];
  if (node.description.trim()) {
    copies.push(
      copyItem("change.copyDescription", "Copy Description", node.description),
    );
  }

  return [change, view, publish, copies].filter((s) => s.length > 0);
}

function diffMenu(
  snapshot: RepoSnapshot,
  target: DiffTarget,
  editors: EditorInfo[],
): MenuSection[] {
  const { file, line } = target;
  const absPath = `${snapshot.repoPath}/${file.path}`;
  const sections: MenuSection[] = [];

  // The file on disk is the working copy's, so for an older change's
  // diff (or a deleted row, which only has its old number) the clicked
  // line is approximate — a best-effort jump still beats the top.
  if (file.status !== "removed") {
    const jumpLine = line?.new ?? line?.old ?? null;
    const open = editorItems(editors, "diff", absPath, jumpLine);
    if (open.length > 0) {
      open.push({
        id: "diff.reveal",
        label: "Reveal in Finder",
        icon: "folder",
        action: { type: "revealInFinder", path: absPath },
      });
      sections.push(open);
    }
  }

  if (target.resolveTool) {
    sections.push([
      {
        id: "diff.resolve",
        label: `Resolve in ${mergeToolLabel(target.resolveTool)}…`,
        icon: "conflicts",
        action: {
          type: "resolveConflict",
          changeId: target.changeId,
          path: file.path,
        },
      },
    ]);
  }

  // Annotate reads the file as it stands in the change's tree, so a
  // removed file has nothing to annotate; the backend refuses binary,
  // conflicted, and oversized files, so only text-shaped content offers
  // it ("omitted" is text whose diff ran over the render budget — the
  // file itself annotates fine).
  if (
    file.status !== "removed" &&
    (file.content.kind === "text" || file.content.kind === "omitted")
  ) {
    sections.push([
      {
        id: "diff.annotate",
        label: "Annotate File",
        icon: "eye",
        action: {
          type: "annotateFile",
          changeId: target.changeId,
          path: file.path,
        },
      },
    ]);
  }

  const copies: MenuSection = [
    copyItem("diff.copyPath", "Copy Path", file.path),
    copyItem("diff.copyAbsPath", "Copy Absolute Path", absPath),
  ];
  if (file.content.kind === "text" && file.content.hunks.length > 0) {
    if (line) {
      copies.push({
        id: "diff.copyHunk",
        label: "Copy Hunk as Patch",
        icon: "copy",
        action: { type: "copyPatch", scope: "hunk" },
      });
    }
    copies.push({
      id: "diff.copyFilePatch",
      label: "Copy File as Patch",
      icon: "copy",
      action: { type: "copyPatch", scope: "file" },
    });
  }
  sections.push(copies);

  const node = snapshot.nodes.find((n) => n.id === target.changeId);
  if (target.mutable && node) {
    const mutate: MenuSection = [];
    if (actionAvailability(snapshot, node).split) {
      mutate.push({
        id: "diff.moveFile",
        label: "Move File to Another Change…",
        icon: "split",
        action: {
          type: "intent",
          intent: { kind: "split", into: true, path: file.path },
        },
      });
    }
    // Per-hunk discard, under the same trust rule as the split panel's
    // hunk checkboxes (complete exact text diff, ≥2 hunks — one hunk is
    // the whole file). The coordinates captured here are re-verified by
    // the backend against a fresh diff, so a change that moved since the
    // menu opened refuses instead of discarding unseen content.
    const hunk =
      line && file.content.kind === "text" && canSelectHunks(file)
        ? hunkAt(file.content.hunks, line)
        : null;
    if (hunk) {
      // Narrower still: the single changed line under the cursor, riding
      // the same restoreHunk action with a line pick the backend verifies
      // against the fresh hunk. Skipped when the hunk has only one
      // changed line — that line is the hunk.
      const lineIndex = line && canSelectLines(hunk) ? lineIndexAt(hunk, line) : null;
      if (lineIndex !== null) {
        mutate.push({
          id: "diff.restoreLine",
          label: "Discard This Line",
          icon: "undo",
          danger: true,
          action: {
            type: "restoreHunk",
            changeId: target.changeId,
            path: file.path,
            hunk: { ...hunkCoords(hunk), lines: [lineIndex] },
          },
        });
      }
      mutate.push({
        id: "diff.restoreHunk",
        label: "Discard This Hunk",
        icon: "undo",
        danger: true,
        action: {
          type: "restoreHunk",
          changeId: target.changeId,
          path: file.path,
          hunk: hunkCoords(hunk),
        },
      });
    }
    mutate.push({
      id: "diff.restoreFile",
      label: "Discard Changes to File",
      icon: "undo",
      danger: true,
      action: {
        type: "restoreFile",
        changeId: target.changeId,
        path: file.path,
      },
    });
    sections.push(mutate);
  }

  return sections;
}

function bookmarkMenu(target: BookmarkTarget): MenuSection[] {
  const { bookmark, pr } = target;
  const sections: MenuSection[] = [];

  if (pr) {
    sections.push([
      {
        id: "bookmark.openPr",
        label: `Open PR #${pr.number} in Browser`,
        icon: "branch",
        action: { type: "openUrl", url: pr.url },
      },
      copyItem("bookmark.copyPrUrl", "Copy PR URL", pr.url),
    ]);
  }

  const ops: MenuSection = [];
  if (bookmark.isLocal && target.hasRemotes) {
    ops.push({
      id: "bookmark.push",
      label: "Push Bookmark",
      icon: "publish",
      action: { type: "pushBookmark", name: bookmark.name },
    });
  }
  if (!bookmark.isLocal && bookmark.remote) {
    ops.push(
      bookmark.tracked
        ? {
            id: "bookmark.untrack",
            label: `Untrack ${bookmark.name}@${bookmark.remote}`,
            icon: "close",
            action: {
              type: "untrackBookmark",
              name: bookmark.name,
              remote: bookmark.remote,
            },
          }
        : {
            id: "bookmark.track",
            label: `Track ${bookmark.name}@${bookmark.remote}`,
            icon: "eye",
            action: {
              type: "trackBookmark",
              name: bookmark.name,
              remote: bookmark.remote,
            },
          },
    );
  }
  ops.push({
    id: "bookmark.manage",
    label: "Manage in Bookmarks View",
    icon: "bookmark",
    action: { type: "section", section: "bookmarks" },
  });
  sections.push(ops);

  sections.push([copyItem("bookmark.copyName", "Copy Bookmark Name", bookmark.name)]);

  if (bookmark.isLocal && !bookmark.isTrunk) {
    sections.push([
      {
        id: "bookmark.delete",
        label: "Delete Bookmark",
        icon: "trash",
        danger: true,
        action: { type: "deleteBookmark", name: bookmark.name },
      },
    ]);
  }

  return sections;
}

function prMenu(target: PrTarget): MenuSection[] {
  const { pr } = target;
  const sections: MenuSection[] = [
    [
      {
        id: "pr.open",
        label: `Open PR #${pr.number} in Browser`,
        icon: "branch",
        action: { type: "openUrl", url: pr.url },
      },
      copyItem("pr.copyUrl", "Copy PR URL", pr.url),
      copyItem("pr.copyNumber", "Copy PR Number", String(pr.number)),
    ],
  ];

  const acts: MenuSection = [];
  if (canRerun(pr)) {
    acts.push({
      id: "pr.rerunCi",
      label: "Re-run Failed CI",
      icon: "refresh",
      action: { type: "rerunCi", number: pr.number },
    });
  }
  if (target.remote && pr.state === "open" && !target.reviewBookmarkTaken) {
    acts.push({
      id: "pr.fetch",
      label: `Fetch Head as ${reviewBookmark(pr.number)}`,
      icon: "arrowDown",
      action: {
        type: "fetchPr",
        remote: target.remote,
        number: pr.number,
        bookmark: reviewBookmark(pr.number),
      },
    });
  }
  if (acts.length > 0) sections.push(acts);
  return sections;
}

function operationMenu(target: OperationTarget): MenuSection[] {
  const acts: MenuSection = [];
  if (!target.isCurrent) {
    acts.push({
      id: "op.restore",
      label: "Restore Repo to This Operation",
      icon: "restore",
      action: { type: "restoreOperation", opId: target.opId },
    });
  }
  acts.push({
    id: "op.revert",
    label: target.isCurrent ? "Undo This Operation" : "Back Out This Operation",
    icon: "undo",
    action: { type: "revertOperation", opId: target.opId },
  });
  return [acts, [copyItem("op.copyId", "Copy Operation ID", target.opId)]];
}

function conflictMenu(
  repoPath: string,
  target: ConflictTarget,
  editors: EditorInfo[],
): MenuSection[] {
  const sections: MenuSection[] = [];
  if (target.resolveTool && target.changeId) {
    sections.push([
      {
        id: "conflict.resolve",
        label: `Resolve in ${mergeToolLabel(target.resolveTool)}…`,
        icon: "conflicts",
        action: {
          type: "resolveConflict",
          changeId: target.changeId,
          path: target.path,
        },
      },
    ]);
  }
  const open: MenuSection = target.isWorkingCopy
    ? editorItems(editors, "conflict", `${repoPath}/${target.path}`, null)
    : [];
  open.push(copyItem("conflict.copyPath", "Copy Path", target.path));
  sections.push(open);
  return sections;
}

function surfaceMenu(
  ctx: MenuContext,
  withRepoSection: boolean,
): MenuSection[] {
  const { snapshot } = ctx;
  const surface: MenuSection = [
    {
      id: "app.palette",
      label: "Command Palette…",
      icon: "command",
      shortcut: "⌘K",
      action: { type: "palette" },
    },
  ];
  if (snapshot) {
    surface.push({
      id: "app.refresh",
      label: "Refresh",
      icon: "refresh",
      shortcut: "⌘R",
      action: { type: "refresh" },
    });
  } else {
    surface.push({
      id: "app.openRepo",
      label: "Open Repository…",
      icon: "folder",
      shortcut: "⌘O",
      action: { type: "chooseRepo" },
    });
  }
  if (ctx.canUndo) {
    surface.push({
      id: "app.undo",
      label: "Undo Last Operation",
      icon: "undo",
      action: { type: "undo" },
    });
  }
  const sections: MenuSection[] = [surface];

  if (snapshot && withRepoSection) {
    const repo: MenuSection = [
      copyItem("app.copyRepoPath", "Copy Repository Path", snapshot.repoPath),
      {
        id: "app.reveal",
        label: "Reveal in Finder",
        icon: "folder",
        action: { type: "revealInFinder", path: snapshot.repoPath },
      },
      {
        id: "app.terminal",
        label: "Open in Terminal",
        icon: "terminal",
        action: { type: "openInTerminal", path: snapshot.repoPath },
      },
    ];
    if (ctx.editors.length > 0) {
      repo.push({
        id: "app.openRepoEditor",
        label: `Open Repo in ${ctx.editors[0].label}`,
        icon: "code",
        action: {
          type: "openInEditor",
          editor: ctx.editors[0].id,
          path: snapshot.repoPath,
          line: null,
        },
      });
    }
    sections.push(repo);
  }
  return sections;
}

function breadcrumbMenu(ctx: MenuContext): MenuSection[] {
  const items: MenuSection = [];
  if (ctx.canUndo) {
    items.push({
      id: "breadcrumb.undo",
      label: "Undo Last Operation",
      icon: "undo",
      action: { type: "undo" },
    });
  }
  items.push({
    id: "breadcrumb.ops",
    label: "Show in Operations Timeline",
    icon: "operations",
    action: { type: "section", section: "operations" },
  });
  return [items];
}

export function buildMenu(ctx: MenuContext): MenuSection[] {
  const { target, snapshot } = ctx;

  // A text field's menu is pure editing, like a native one.
  if (target.kind === "editable") return editableMenu(target);

  const sections: MenuSection[] = [];
  if (ctx.selection.trim() !== "") {
    sections.push([
      {
        id: "copy.selection",
        label: "Copy",
        icon: "copy",
        shortcut: "⌘C",
        action: { type: "copyText", text: ctx.selection },
      },
    ]);
  }

  switch (target.kind) {
    case "change":
      if (snapshot) sections.push(...changeMenu(snapshot, target));
      break;
    case "diff":
      if (snapshot) sections.push(...diffMenu(snapshot, target, ctx.editors));
      break;
    case "bookmark":
      sections.push(...bookmarkMenu(target));
      break;
    case "pr":
      sections.push(...prMenu(target));
      break;
    case "operation":
      sections.push(...operationMenu(target));
      break;
    case "conflict":
      if (snapshot) sections.push(...conflictMenu(snapshot.repoPath, target, ctx.editors));
      break;
    case "breadcrumb":
      sections.push(...breadcrumbMenu(ctx));
      break;
    case "surface":
      sections.push(...surfaceMenu(ctx, true));
      break;
  }

  return sections.filter((section) => section.length > 0);
}
