// Pure command model for the command palette: what one context offers, and
// how a query filters and ranks it. The component executes the returned
// actions; nothing here touches Svelte or app state (context comes in as a
// plain value so tests can drive every shape).

import type { GraphNode } from "$lib/bindings/GraphNode";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { IconName } from "$lib/components/ui/icons";
import type { RecentRepo, Section, UiIntent } from "$lib/state/app.svelte";
import {
  actionAvailability,
  resolveCompareFrom,
} from "$lib/components/inspector/inspect";

// What runs when a row is picked. `intent` rows hand off to the surface
// that owns the matching UI (plan/confirm panels, layout, view mode) — the
// palette never duplicates a consequence-stating panel.
export type PaletteAction =
  | { type: "chooseRepo" }
  | { type: "openRecent"; path: string }
  | { type: "refresh" }
  | { type: "fetchUpstream" }
  | { type: "undo" }
  | { type: "newChild"; id: string }
  | { type: "edit"; id: string }
  | { type: "intent"; intent: UiIntent }
  | { type: "section"; section: Section }
  | { type: "goto"; id: string }
  | { type: "mode"; mode: "system" | "light" | "dark" }
  | { type: "theme"; id: string };

export interface PaletteItem {
  /** Stable id, used as the render key and by the visual harness. */
  id: string;
  group: string;
  title: string;
  /** Right-aligned context (a change title, a repo path, a scheme). */
  hint?: string;
  /** Extra match surface that should not render. */
  keywords?: string;
  icon?: IconName;
  /** jj node glyph rows use instead of an icon (go-to-change). */
  glyph?: string;
  glyphTone?: string;
  shortcut?: string;
  danger?: boolean;
  /** Hidden until the query matches (bulk entries like themes/recents). */
  secondary?: boolean;
  action: PaletteAction;
}

// The palette needs only these fields of the theme registry; the component
// passes the real ThemeDef list, tests pass minimal literals.
export interface PaletteThemeDef {
  id: string;
  label: string;
  scheme: "light" | "dark";
}

export interface PaletteContext {
  snapshot: RepoSnapshot | null;
  /** The workbench selection resolved to its node, if any. */
  selected: GraphNode | null;
  recentRepos: RecentRepo[];
  /** Whether the breadcrumb's undo target is still around. */
  canUndo: boolean;
  /** Themes are a supporter perk; unregistered copies get no theme rows. */
  registered: boolean;
  themes: PaletteThemeDef[];
}

const SECTIONS: { section: Section; label: string; icon: IconName }[] = [
  { section: "workbench", label: "Workbench", icon: "workbench" },
  { section: "conflicts", label: "Conflicts", icon: "conflicts" },
  { section: "publish", label: "Publish", icon: "publish" },
  { section: "operations", label: "Operations", icon: "operations" },
  { section: "workspaces", label: "Workspaces", icon: "workspaces" },
];

const NODE_GLYPH = { workingCopy: "@", mutable: "○", immutable: "◆" } as const;

function title(node: GraphNode): string {
  return node.description.split("\n")[0] || "no description";
}

// Every command this context offers, in group order. Availability mirrors
// the surfaces the commands route to: selection actions follow the actions
// row's rules, compare presets appear only when they resolve, repository
// commands only when they have something to act on.
function buildItems(ctx: PaletteContext): PaletteItem[] {
  const items: PaletteItem[] = [];
  const { snapshot, selected } = ctx;

  if (snapshot && selected) {
    const avail = actionAvailability(snapshot, selected);
    const at = `${selected.id.slice(0, 4)} “${title(selected)}”`;
    const change = (item: Omit<PaletteItem, "group">) =>
      items.push({ group: "Change", ...item });
    if (avail.describe) {
      change({
        id: "change.describe",
        title: selected.description.trim() ? "Edit description" : "Describe change",
        hint: at,
        keywords: "describe message commit text",
        icon: "edit",
        action: { type: "intent", intent: { kind: "describe" } },
      });
    }
    change({
      id: "change.new",
      title: "New child change",
      hint: `on ${at}`,
      keywords: "start work jj new empty",
      icon: "plus",
      action: { type: "newChild", id: selected.id },
    });
    if (avail.edit) {
      change({
        id: "change.edit",
        title: "Edit change",
        hint: `make ${selected.id.slice(0, 4)} the working copy`,
        keywords: "checkout working copy jj edit",
        icon: "atSign",
        action: { type: "edit", id: selected.id },
      });
    }
    change({
      id: "change.bookmark",
      title: "Bookmark…",
      hint: at,
      keywords: "branch create move bookmark",
      icon: "bookmark",
      action: { type: "intent", intent: { kind: "bookmark" } },
    });
    if (avail.rebase) {
      change({
        id: "change.rebase",
        title: "Rebase…",
        hint: at,
        keywords: "move onto parent destination",
        icon: "rebase",
        action: { type: "intent", intent: { kind: "rebase" } },
      });
    }
    if (avail.split) {
      change({
        id: "change.split",
        title: "Split change…",
        hint: at,
        keywords: "carve separate divide files jj split",
        icon: "split",
        action: { type: "intent", intent: { kind: "split" } },
      });
      change({
        id: "change.squashInto",
        title: "Move files into another change…",
        hint: at,
        keywords: "squash into amend hunks jj squash --into",
        icon: "split",
        action: { type: "intent", intent: { kind: "split", into: true } },
      });
    }
    if (avail.squash) {
      change({
        id: "change.squash",
        title: "Squash into parent",
        hint: at,
        keywords: "fold combine amend meld",
        icon: "squash",
        action: { type: "intent", intent: { kind: "squash" } },
      });
    }
    if (avail.abandon) {
      change({
        id: "change.abandon",
        title: "Abandon change",
        hint: at,
        keywords: "delete drop remove discard",
        icon: "trash",
        danger: true,
        action: { type: "intent", intent: { kind: "abandon" } },
      });
    }

    const compare = (item: Omit<PaletteItem, "group" | "icon">) =>
      items.push({ group: "Compare", icon: "compare", ...item });
    compare({
      id: "compare.parent",
      title: "Compare against parent",
      keywords: "diff vs reset",
      action: { type: "intent", intent: { kind: "compare", mode: { kind: "parent" } } },
    });
    if (resolveCompareFrom(snapshot, selected.id, { kind: "trunk" })) {
      compare({
        id: "compare.trunk",
        title: `Compare against ${snapshot.trunkBookmark || "trunk"}`,
        keywords: "diff vs trunk main",
        action: { type: "intent", intent: { kind: "compare", mode: { kind: "trunk" } } },
      });
    }
    if (resolveCompareFrom(snapshot, selected.id, { kind: "base" })) {
      compare({
        id: "compare.base",
        title: "Compare against stack base",
        keywords: "diff vs cumulative",
        action: { type: "intent", intent: { kind: "compare", mode: { kind: "base" } } },
      });
    }
    compare({
      id: "compare.pick",
      title: "Compare against any change…",
      keywords: "diff vs commit pick",
      action: { type: "intent", intent: { kind: "compare" } },
    });
  }

  if (snapshot) {
    items.push(
      {
        id: "view.graph",
        group: "View",
        title: "Graph view",
        hint: "every workstream in one tree",
        keywords: "workbench log tree",
        icon: "branch",
        shortcut: "G",
        action: { type: "intent", intent: { kind: "view", view: "graph" } },
      },
      {
        id: "view.focus",
        group: "View",
        title: "Focus view",
        hint: "one workstream at a time",
        keywords: "workbench lane stack",
        icon: "stack",
        shortcut: "W",
        action: { type: "intent", intent: { kind: "view", view: "focus" } },
      },
    );
    if (selected) {
      items.push(
        {
          id: "layout.unified",
          group: "View",
          title: "Unified diff",
          keywords: "layout single column",
          icon: "diffUnified",
          action: { type: "intent", intent: { kind: "layout", layout: "unified" } },
        },
        {
          id: "layout.split",
          group: "View",
          title: "Side-by-side diff",
          keywords: "layout split two column",
          icon: "diffSplit",
          action: { type: "intent", intent: { kind: "layout", layout: "split" } },
        },
      );
    }
    for (const [index, entry] of SECTIONS.entries()) {
      items.push({
        id: `section.${entry.section}`,
        group: "Navigate",
        title: `Go to ${entry.label}`,
        icon: entry.icon,
        shortcut: `⌘${index + 1}`,
        action: { type: "section", section: entry.section },
      });
    }
  }

  items.push({
    id: "repo.open",
    group: "Repository",
    title: "Open repository…",
    keywords: "folder project jj repo",
    icon: "folder",
    shortcut: "⌘O",
    action: { type: "chooseRepo" },
  });
  for (const recent of ctx.recentRepos.slice(0, 5)) {
    if (recent.path === snapshot?.repoPath) continue;
    items.push({
      id: `repo.recent.${recent.path}`,
      group: "Repository",
      title: `Open ${recent.name}`,
      hint: recent.path,
      keywords: "recent repository switch",
      icon: "folder",
      // Front and center on the welcome screen; out of the way once a
      // repo is open.
      secondary: snapshot !== null,
      action: { type: "openRecent", path: recent.path },
    });
  }
  if (snapshot) {
    items.push({
      id: "repo.refresh",
      group: "Repository",
      title: "Refresh snapshot",
      keywords: "reload sync update",
      icon: "refresh",
      shortcut: "⌘R",
      action: { type: "refresh" },
    });
  }
  if (snapshot && snapshot.gitRemotes.length > 0) {
    items.push({
      id: "repo.fetch",
      group: "Repository",
      title: "Fetch from remotes",
      keywords: "git fetch upstream pull remote sync check",
      icon: "refresh",
      action: { type: "fetchUpstream" },
    });
  }
  if (ctx.canUndo) {
    items.push({
      id: "repo.undo",
      group: "Repository",
      title: "Undo last operation",
      keywords: "revert rollback mistake",
      icon: "undo",
      action: { type: "undo" },
    });
  }

  // Theme rows are supporter-gated (like the picker) and query-only: ten
  // palettes would swamp the default list.
  if (ctx.registered) {
    const modes = [
      { mode: "system", label: "follow system", icon: "sunMoon" },
      { mode: "light", label: "light", icon: "sun" },
      { mode: "dark", label: "dark", icon: "moon" },
    ] as const;
    for (const entry of modes) {
      items.push({
        id: `appearance.${entry.mode}`,
        group: "Appearance",
        title: `Appearance: ${entry.label}`,
        keywords: "theme mode color scheme",
        icon: entry.icon,
        secondary: true,
        action: { type: "mode", mode: entry.mode },
      });
    }
    for (const def of ctx.themes) {
      items.push({
        id: `theme.${def.id}`,
        group: "Appearance",
        title: `Theme: ${def.label}`,
        hint: def.scheme,
        keywords: "theme color palette appearance",
        icon: "sparkles",
        secondary: true,
        action: { type: "theme", id: def.id },
      });
    }
  }

  return items;
}

// How well one item matches the query: every token must land somewhere;
// title hits outrank keyword/hint hits. 0 = filtered out.
export function scoreItem(item: PaletteItem, tokens: string[]): number {
  const titleText = item.title.toLowerCase();
  const rest = `${item.keywords ?? ""} ${item.hint ?? ""}`.toLowerCase();
  let score = 0;
  for (const token of tokens) {
    if (titleText.startsWith(token)) score += 4;
    else if (titleText.split(/[^a-z0-9]+/).some((w) => w.startsWith(token)))
      score += 3;
    else if (titleText.includes(token)) score += 2;
    else if (rest.includes(token)) score += 1;
    else return 0;
  }
  return score;
}

const GOTO_LIMIT = 8;

// Changes the query names — by id/change-id/commit-id prefix, title text,
// or bookmark — as jump rows. Query-only, capped, in graph (snapshot)
// order. Revset input is a later milestone; this is plain lookup.
export function gotoMatches(
  snapshot: RepoSnapshot,
  selectedId: string | null,
  tokens: string[],
): PaletteItem[] {
  const items: PaletteItem[] = [];
  for (const node of snapshot.nodes) {
    if (node.id === selectedId) continue;
    const ids = [node.id, node.changeId, node.commitId].map((s) =>
      s.toLowerCase(),
    );
    const text = title(node).toLowerCase();
    const marks = node.bookmarks.map((b) => b.toLowerCase());
    let score = 0;
    for (const token of tokens) {
      if (ids.some((id) => id.startsWith(token))) score += 4;
      else if (marks.some((m) => m.startsWith(token))) score += 3;
      else if (text.split(/\s+/).some((w) => w.startsWith(token))) score += 2;
      else if (text.includes(token) || marks.some((m) => m.includes(token)))
        score += 1;
      else {
        score = 0;
        break;
      }
    }
    if (score === 0) continue;
    items.push({
      id: `goto.${node.id}`,
      group: "Go to",
      title: `${node.id.slice(0, 8)}`,
      hint: [title(node), ...node.bookmarks].join(" · "),
      glyph: node.hasConflict ? "×" : NODE_GLYPH[node.kind],
      glyphTone: node.kind,
      action: { type: "goto", id: node.id },
    });
    if (items.length === GOTO_LIMIT) break;
  }
  return items;
}

// The palette's contents for one context and query. Empty query: the
// primary commands in group order. With a query: every match (commands and
// go-to-change rows) ranked best-first as one flat list.
export function paletteResults(
  ctx: PaletteContext,
  query: string,
): PaletteItem[] {
  const items = buildItems(ctx);
  const tokens = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return items.filter((item) => !item.secondary);
  const scored = items
    .map((item, index) => ({ item, index, score: scoreItem(item, tokens) }))
    .filter((entry) => entry.score > 0)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .map((entry) => entry.item);
  const jumps = ctx.snapshot
    ? gotoMatches(ctx.snapshot, ctx.selected?.id ?? null, tokens)
    : [];
  return [...scored, ...jumps];
}
