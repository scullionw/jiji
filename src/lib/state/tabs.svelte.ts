// The open-repo tabs. Each tab is one repo's session in the shell; the
// `app` singleton always holds the *active* tab's state, and switching
// tabs stashes/restores the repo-scoped slice (actions.ts owns those
// moves, so every component keeps reading plain `app`). This module is
// pure state — no `$lib/api` import — so api.ts can read the active repo
// path without a cycle.

import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { MutationBreadcrumb, Section } from "./app.svelte";

/** The repo-scoped slice of `app` state a tab keeps while another tab
 * owns the screen. */
export interface TabUiState {
  section: Section;
  selectedNodeId: string | null;
  focusedWorkstreamId: string | null;
  lastMutation: MutationBreadcrumb | null;
  resolvingConflict: { changeId: string; path: string } | null;
  error: string | null;
}

export interface RepoTab {
  /** Stable identity for keyed rendering and activation. */
  id: number;
  /** Canonical repo path (`snapshot.repoPath`, the workspace root); null
   * while the tab is empty (the welcome screen). */
  path: string | null;
  /** Label for the tab strip; null until the repo names itself. */
  name: string | null;
  /** Latest snapshot the backend published for this repo — restored
   * instantly on activation, kept fresh in the background by the repo's
   * own watcher. null until the tab first opens this run. */
  snapshot: RepoSnapshot | null;
  /** UI state stashed when the tab deactivates; null until then. */
  ui: TabUiState | null;
}

/** A closed repo tab, remembered so ⌘⇧T can bring it back. */
export interface ClosedTab {
  path: string;
  name: string | null;
}

const TABS_KEY = "jiji.tabs";

/** How many closed tabs ⌘⇧T can walk back through. */
const MAX_CLOSED = 10;

let nextId = 1;

// Tabs are class instances, which `$state` stores as-is instead of
// proxying: each field is its own signal, so `snapshot` can stay raw like
// `app.snapshot` (see app.svelte.ts). As a plain literal in `tabs.all` it
// would be deep-proxied, and restoring a tab would hand that proxy back
// to `app`.
class Tab implements RepoTab {
  readonly id: number;
  path = $state<string | null>(null);
  name = $state<string | null>(null);
  snapshot = $state.raw<RepoSnapshot | null>(null);
  ui = $state<TabUiState | null>(null);

  constructor(id: number, path: string | null, name: string | null) {
    this.id = id;
    this.path = path;
    this.name = name;
  }
}

export const tabs = $state({
  all: [] as RepoTab[],
  activeId: 0,
  /** Recently closed repo tabs, oldest first. Session-only, like a
   * browser's: a restart already restores the tabs that were open. */
  closed: [] as ClosedTab[],
});

export function activeTab(): RepoTab | undefined {
  return tabs.all.find((t) => t.id === tabs.activeId);
}

/** The active tab's repo path — what `$lib/api` stamps on repo-scoped
 * commands. */
export function activeTabPath(): string | null {
  return activeTab()?.path ?? null;
}

export function tabForPath(path: string): RepoTab | undefined {
  return tabs.all.find((t) => t.path === path);
}

/** Append a tab (empty unless a path is given). The caller decides when
 * it takes the screen. */
export function createTab(
  path: string | null = null,
  name: string | null = null,
): RepoTab {
  const tab = new Tab(nextId++, path, name);
  tabs.all.push(tab);
  return tab;
}

/** Remember a repo tab as it closes; closing the same repo again moves
 * it to the top instead of stacking a duplicate. */
export function rememberClosedTab(tab: RepoTab): void {
  if (!tab.path) return;
  const path = tab.path;
  tabs.closed = [
    ...tabs.closed.filter((c) => c.path !== path),
    { path, name: tab.name },
  ].slice(-MAX_CLOSED);
}

/** The closed tab ⌘⇧T would reopen: the most recent one whose repo is
 * not open again already (a re-pick from recents, the CLI). */
export function lastClosedTab(): ClosedTab | undefined {
  for (let i = tabs.closed.length - 1; i >= 0; i--) {
    if (!tabForPath(tabs.closed[i].path)) return tabs.closed[i];
  }
  return undefined;
}

/** Take the closed tab ⌘⇧T reopens off the list, along with any newer
 * entries that were reopened another way. */
export function takeClosedTab(): ClosedTab | undefined {
  const entry = lastClosedTab();
  if (!entry) return undefined;
  const at = tabs.closed.findIndex((c) => c.path === entry.path);
  const taken = { path: entry.path, name: entry.name };
  tabs.closed = tabs.closed.slice(0, at);
  return taken;
}

/** The tab strip's label: the repo's name, its path's last segment while
 * the name has not arrived, or the empty-tab placeholder. */
export function tabLabel(tab: Pick<RepoTab, "name" | "path">): string {
  if (tab.name) return tab.name;
  const base = tab.path?.replace(/\/+$/, "").split("/").pop();
  return base || "New Tab";
}

export interface TabTitle {
  /** The repo's name (see tabLabel). */
  label: string;
  /** The enclosing directories that tell this tab apart from others
   * sharing its label; null while the label is unique. */
  hint: string | null;
}

/** Labels for a strip of tabs, disambiguated the way editors do for
 * same-named files: repos sharing a name (~/work/api and ~/oss/api) each
 * gain the shortest run of parent directories that no other shares. */
export function tabTitles(
  all: Pick<RepoTab, "id" | "name" | "path">[],
): Map<number, TabTitle> {
  const titles = new Map<number, TabTitle>();
  const byLabel = new Map<string, { id: number; parents: string[] }[]>();
  for (const tab of all) {
    const label = tabLabel(tab);
    titles.set(tab.id, { label, hint: null });
    if (!tab.path) continue;
    const parents = tab.path.replace(/\/+$/, "").split("/").slice(0, -1);
    byLabel.set(label, [...(byLabel.get(label) ?? []), { id: tab.id, parents }]);
  }
  const tail = (segments: string[], depth: number) =>
    segments.slice(-depth).join("/");
  for (const [label, group] of byLabel) {
    if (group.length < 2) continue;
    for (const { id, parents } of group) {
      let depth = 1;
      while (
        depth < parents.length &&
        group.some(
          (other) => other.id !== id && tail(other.parents, depth) === tail(parents, depth),
        )
      ) {
        depth++;
      }
      titles.set(id, { label, hint: tail(parents, depth) || "/" });
    }
  }
  return titles;
}

interface PersistedTabs {
  repos: { path: string; name: string }[];
  active: number;
}

/** Only repo tabs survive a restart (empty tabs are ephemeral); `active`
 * indexes into `repos`. Snapshots and UI state are not persisted — a
 * restored tab re-opens its repo on first activation. */
export function persistTabs(): void {
  const repos = tabs.all.filter((t) => t.path !== null);
  const payload: PersistedTabs = {
    repos: repos.map((t) => ({ path: t.path as string, name: t.name ?? "" })),
    active: Math.max(
      0,
      repos.findIndex((t) => t.id === tabs.activeId),
    ),
  };
  localStorage.setItem(TABS_KEY, JSON.stringify(payload));
}

export function loadPersistedTabs(): PersistedTabs {
  try {
    const raw = localStorage.getItem(TABS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as PersistedTabs;
      if (Array.isArray(parsed.repos)) {
        return {
          repos: parsed.repos.filter((r) => typeof r?.path === "string"),
          active: typeof parsed.active === "number" ? parsed.active : 0,
        };
      }
    }
  } catch {
    // Corrupt persistence only loses the restored tabs.
  }
  return { repos: [], active: 0 };
}
