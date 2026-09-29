// User-facing actions. Each one talks to the backend through `$lib/api`
// and writes results into the shared app state.

import { ask, open as openDialog } from "@tauri-apps/plugin-dialog";
import * as api from "$lib/api";
import type { ConflictSide } from "$lib/bindings/ConflictSide";
import type { MutationOutcome } from "$lib/bindings/MutationOutcome";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SnapshotView } from "$lib/bindings/SnapshotView";
import type { SplitHunk } from "$lib/bindings/SplitHunk";
import type { SplitSelection } from "$lib/bindings/SplitSelection";
import { persistView, persistedView } from "$lib/components/graph/views";
import { stackPosition } from "$lib/components/inspector/inspect";
import { app, GRAPH_KEY, SIDEBAR_KEY, type Section, type UiIntent } from "./app.svelte";
import { attachAutoLand } from "./autoland.svelte";
import { syncCliStatus } from "./cli.svelte";
import { syncEditors } from "./editor.svelte";
import { syncTerminals } from "./terminal.svelte";
import { refreshForgePrs } from "./forge.svelte";
import { forgetRepo, loadRecentRepos, rememberRepo } from "./recent";
import {
  activeTab,
  activeTabPath,
  createTab,
  loadPersistedTabs,
  persistTabs,
  rememberClosedTab,
  tabForPath,
  tabs,
  takeClosedTab,
  type RepoTab,
} from "./tabs.svelte";

export async function bootstrap(): Promise<void> {
  try {
    app.recentRepos = await loadRecentRepos();
  } catch (error) {
    console.warn("failed to load recent repos", error);
  }

  // App-lifetime subscription: every snapshot the backend publishes lands
  // on the tab of the repo it belongs to. Background repos (their
  // watchers, a pinned auto-land job's mutations) stay fresh without
  // owning the screen; only the active tab's snapshot reaches `app`.
  await api.onSnapshotUpdated(applySnapshot);

  // Same shape for the auto-land job: subscribe, then reattach to
  // whatever the backend is already doing.
  await attachAutoLand();

  // Restore the previous session's tabs. The active one recovers its
  // snapshot right away — from the surviving backend session if the
  // webview merely reloaded, else a full open; background tabs load
  // lazily on first activation.
  const persisted = loadPersistedTabs();
  for (const repo of persisted.repos) createTab(repo.path, repo.name);
  if (tabs.all.length === 0) createTab();
  const active =
    tabs.all[Math.min(persisted.active, tabs.all.length - 1)] ?? tabs.all[0];
  tabs.activeId = active.id;
  if (active.path) {
    try {
      const current = await api.currentSnapshot(active.path);
      if (current) applySnapshot(current);
      else await openRepoInto(active, active.path);
    } catch (error) {
      console.warn("failed to restore the active tab", error);
    }
  }

  // A repo handed over by the `jiji` CLI launcher (or a folder dropped on
  // the Dock icon): the backend parks the path and pings. Draining on the
  // ping covers the running app; the sweep below covers a cold start whose
  // path arrived before this code ran. After the tab restore above, so a
  // launch-delivered repo wins the screen over a restored one.
  const drainCliOpen = async () => {
    try {
      const path = await api.takePendingOpen();
      if (path) await openRepoAt(path);
    } catch (error) {
      console.warn("failed to drain pending cli open", error);
    }
  };
  await api.onCliOpenRequest(() => void drainCliOpen());
  await drainCliOpen();

  // Whether to offer installing the launcher (welcome card, palette rows).
  void syncCliStatus();

  // Which editor CLIs and terminal apps exist, for the context menu's
  // Open in… items and the Settings pickers.
  void syncEditors();
  void syncTerminals();
}

// Route a published snapshot to its repo's tab; the active tab also puts
// it on screen. A tab's first snapshot is its open moment — land on the
// working copy's diff, not an overview. Snapshots for repos no tab holds
// (a just-closed tab's late publish) are dropped. Exported for the flows
// that pull a snapshot explicitly (the upstream check) instead of waiting
// on the event.
export function applySnapshot(snapshot: RepoSnapshot): void {
  const tab = tabForPath(snapshot.repoPath);
  if (!tab) return;
  const firstSnapshot = tab.snapshot === null;
  snapshot = settledSnapshot(tab, snapshot);
  tab.snapshot = snapshot;
  tab.name = snapshot.repoName;
  if (tab.id !== tabs.activeId) return;
  app.snapshot = snapshot;
  app.error = null;
  if (firstSnapshot) selectWorkingCopy();
}

// Most snapshots arrive twice or change nothing: a command's return value
// and its broadcast event carry the same one, the mutation flow pulls the
// snapshot its event already delivered, and a tab's re-sync on activation
// (or ⌘R) republishes whatever is already on screen. A new object would
// rebuild every derived view for an identical graph, so a republish equal
// to the tab's current snapshot keeps the object it already has.
function settledSnapshot(tab: RepoTab, next: RepoSnapshot): RepoSnapshot {
  return tab.snapshot && jsonEqual(tab.snapshot, next) ? tab.snapshot : next;
}

// Structural equality for deserialized JSON (plain objects, arrays,
// primitives). Bails at the first difference, so a changed snapshot costs
// little more than the walk to wherever it changed.
function jsonEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) {
    return false;
  }
  if (Array.isArray(a)) {
    if (!Array.isArray(b) || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (!jsonEqual(a[i], b[i])) return false;
    return true;
  }
  if (Array.isArray(b)) return false;
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  const keys = Object.keys(left);
  if (keys.length !== Object.keys(right).length) return false;
  for (const key of keys) {
    if (!(key in right) || !jsonEqual(left[key], right[key])) return false;
  }
  return true;
}

// Opening a repo lands on the diff of the current work — the thing a VCS
// GUI gets opened to see. Selecting the working-copy node puts its diff on
// screen instead of the repo overview. On a divergent working copy (nodes
// keyed by commit id) or a view lens that hides it, nothing resolves and
// the overview stays — the workbench's own guards drop stale selections.
function selectWorkingCopy(): void {
  const snapshot = app.snapshot;
  if (!snapshot) return;
  const id = snapshot.workingCopy;
  if (!snapshot.nodes.some((n) => n.id === id)) return;
  app.selectedNodeId = id;
  app.selectedRange = null;
  const owner = stackPosition(snapshot, id)?.workstream;
  if (owner) app.focusedWorkstreamId = owner.id;
}

// ── Tabs ──
// The `app` singleton always holds the active tab's state; switching
// stashes the repo-scoped slice on the outgoing tab and restores the
// incoming one's, so components keep reading plain `app`.

function stashTabUi(tab: RepoTab): void {
  tab.snapshot = app.snapshot;
  tab.ui = {
    section: app.section,
    selectedNodeId: app.selectedNodeId,
    focusedWorkstreamId: app.focusedWorkstreamId,
    lastMutation: app.lastMutation,
    resolvingConflict: app.resolvingConflict,
    error: app.error,
  };
}

function restoreTabUi(tab: RepoTab): void {
  app.snapshot = tab.snapshot;
  app.section = tab.ui?.section ?? "workbench";
  app.selectedNodeId = tab.ui?.selectedNodeId ?? null;
  app.selectedRange = null;
  app.focusedWorkstreamId = tab.ui?.focusedWorkstreamId ?? null;
  app.lastMutation = tab.ui?.lastMutation ?? null;
  app.resolvingConflict = tab.ui?.resolvingConflict ?? null;
  app.error = tab.ui?.error ?? null;
  app.intent = null;
  app.opening = false;
  // A tab that gained its snapshot while backgrounded (watcher events)
  // never had a selection; land on the working copy like an open would.
  if (tab.snapshot && !tab.ui) selectWorkingCopy();
}

// The synchronous switch: instant restore of whatever the tab last showed.
function switchToTab(target: RepoTab): void {
  const current = activeTab();
  if (current?.id === target.id) return;
  if (current) stashTabUi(current);
  tabs.activeId = target.id;
  restoreTabUi(target);
  persistTabs();
}

// Bring a repo tab's content up: recover the backend session (a webview
// reload keeps it), open fresh when there is none, or quietly re-sync a
// tab that already has a snapshot so a missed watcher beat cannot leave
// stale state on screen.
async function ensureTabLoaded(tab: RepoTab): Promise<void> {
  if (!tab.path) return;
  if (tab.snapshot !== null) {
    void api.refreshSnapshot().catch(() => {});
    return;
  }
  try {
    const current = await api.currentSnapshot(tab.path);
    if (current) {
      applySnapshot(current);
      return;
    }
  } catch {
    // Fall through to a full open.
  }
  await openRepoInto(tab, tab.path);
}

export async function activateTab(id: number): Promise<void> {
  const target = tabs.all.find((t) => t.id === id);
  if (!target || tabs.activeId === id) return;
  switchToTab(target);
  await ensureTabLoaded(target);
}

export function newTab(): void {
  switchToTab(createTab());
}

/** Close a tab; its backend session drops (the watcher stops, a live
 * auto-land job for the repo detaches into its resumable record). The
 * last tab never closes into nothing — it gives way to an empty tab,
 * except a lone empty tab, which just stays. */
export function closeTab(id: number): void {
  const idx = tabs.all.findIndex((t) => t.id === id);
  if (idx === -1) return;
  const tab = tabs.all[idx];
  if (tabs.all.length === 1 && tab.path === null) return;
  const wasActive = tabs.activeId === id;
  tabs.all.splice(idx, 1);
  if (tab.path) {
    rememberClosedTab(tab);
    void api
      .closeRepo(tab.path)
      .catch((error) => console.warn("failed to close repo session", error));
  }
  if (tabs.all.length === 0) createTab();
  if (wasActive) {
    const neighbor = tabs.all[Math.min(idx, tabs.all.length - 1)];
    tabs.activeId = neighbor.id;
    restoreTabUi(neighbor);
    void ensureTabLoaded(neighbor);
  }
  persistTabs();
}

export function closeActiveTab(): void {
  closeTab(tabs.activeId);
}

/** Bring back the most recently closed repo tab (⌘⇧T), newest first on
 * repeat. It opens the way any repo does — into the active tab while that
 * one is empty, else a new tab. */
export async function reopenClosedTab(): Promise<void> {
  const closed = takeClosedTab();
  if (closed) await openRepoAt(closed.path);
}

/** Cycle the active tab (⌃Tab / ⌘⇧] with +1, reversed with -1). */
export function cycleTab(delta: number): void {
  const idx = tabs.all.findIndex((t) => t.id === tabs.activeId);
  if (idx === -1 || tabs.all.length < 2) return;
  const next = tabs.all[(idx + delta + tabs.all.length) % tabs.all.length];
  void activateTab(next.id);
}

/** Open a repo somewhere sensible: the tab that already shows it (the
 * CLI's open and a re-pick from recents land on the existing view, never
 * a duplicate), the active tab while it is empty, or a new tab. */
export async function openRepoAt(path: string): Promise<void> {
  const existing = tabForPath(path);
  if (existing) {
    await activateTab(existing.id);
    return;
  }
  let tab = activeTab();
  if (!tab || tab.path !== null) {
    tab = createTab();
    switchToTab(tab);
  }
  await openRepoInto(tab, path);
}

// Whether a tab is still in the strip — an open can outlive its tab (⌘W
// while the repo loads).
function tabIsOpen(tab: RepoTab): boolean {
  return tabs.all.some((t) => t.id === tab.id);
}

// Open `path` into a tab (which must be the active one — every route here
// puts it on screen first, so open errors and progress have somewhere to
// render). The tab can lose the screen, or close, while the open is in
// flight: every call after the first names the opened repo explicitly
// instead of trusting whichever tab is active by then.
async function openRepoInto(tab: RepoTab, path: string): Promise<void> {
  app.opening = true;
  app.error = null;
  try {
    let snapshot = await api.openRepo(path);
    if (!tabIsOpen(tab)) {
      // Closed mid-open: the session the backend just started has no tab
      // to feed, so drop it — unless another tab already holds the repo.
      if (!tabForPath(snapshot.repoPath)) {
        void api.closeRepo(snapshot.repoPath).catch(() => {});
      }
      return;
    }
    // The requested path may be an alias of the canonical root (a subdir,
    // a symlink) of a repo already open in another tab — fold into that
    // tab instead of showing the same repo twice.
    const dupe = tabs.all.find(
      (t) => t.id !== tab.id && t.path === snapshot.repoPath,
    );
    if (dupe) {
      tabs.all = tabs.all.filter((t) => t.id !== tab.id);
      if (tabs.activeId === tab.id) {
        tabs.activeId = dupe.id;
        restoreTabUi(dupe);
      }
      applySnapshot(snapshot);
      persistTabs();
      return;
    }
    tab.path = snapshot.repoPath;
    tab.name = snapshot.repoName;
    tab.snapshot = snapshot;
    // The lens choice sticks per repo; a fresh session opens on the
    // default, so re-apply the persisted one (a surviving session already
    // rendered it — skip the redundant refresh). A choice that stopped
    // working (a custom revset naming a deleted bookmark) falls back
    // silently.
    const lens = persistedView(snapshot.repoPath);
    if (
      lens.kind !== "workstreams" &&
      JSON.stringify(lens) !== JSON.stringify(snapshot.view)
    ) {
      try {
        snapshot = settledSnapshot(
          tab,
          await api.setSnapshotView(lens, snapshot.repoPath),
        );
        tab.snapshot = snapshot;
      } catch {
        // A tab closed meanwhile refuses because its session is gone, not
        // because the lens broke — keep the choice for the next open.
        if (tabIsOpen(tab)) persistView(snapshot.repoPath, { kind: "workstreams" });
      }
    }
    if (tabs.activeId === tab.id) {
      app.snapshot = snapshot;
      app.section = "workbench";
      selectWorkingCopy();
    }
    persistTabs();
    app.recentRepos = await rememberRepo(snapshot.repoPath, snapshot.repoName);
  } catch (error) {
    if (tabs.activeId !== tab.id) return;
    const refusal = error as api.CommandError | null;
    if (refusal?.code === "git_repo_without_jj" && refusal.path) {
      try {
        if (await offerJjSetup(tab, refusal.path)) return;
      } catch (setupError) {
        if (tabs.activeId === tab.id) app.error = api.errorMessage(setupError);
        return;
      }
    }
    app.error = api.errorMessage(error);
  } finally {
    app.opening = false;
  }
}

// A plain Git repository: offer to set jj up in it, the way `jj git init
// --colocate` does, then open it. Nothing touches the folder unless the
// user says yes; declining leaves the refusal on the status bar.
async function offerJjSetup(tab: RepoTab, gitRoot: string): Promise<boolean> {
  app.opening = false;
  const name = gitRoot.split(/[\\/]/).filter(Boolean).pop() ?? gitRoot;
  const setUp = await ask(
    `${gitRoot} is a Git repository without jj.\n\n` +
      "Jiji can set jj up here the way `jj git init --colocate` does: " +
      "a .jj folder next to .git, with your branches imported as " +
      "bookmarks. Git keeps working alongside it.",
    {
      title: `Set up jj in “${name}”?`,
      okLabel: "Set up jj",
      cancelLabel: "Cancel",
    },
  );
  if (!setUp || !tabIsOpen(tab)) return false;
  app.opening = true;
  const root = await api.initJjRepo(gitRoot);
  await openRepoInto(tab, root);
  return true;
}

// Remove a repo from the recents list (the welcome screen's ×). The row
// leaves at once; the store catches up behind it.
export async function forgetRecentRepo(path: string): Promise<void> {
  app.recentRepos = app.recentRepos.filter((repo) => repo.path !== path);
  try {
    app.recentRepos = await forgetRepo(path);
  } catch (error) {
    console.warn("failed to forget recent repo", error);
  }
}

export async function chooseRepo(): Promise<void> {
  const selected = await openDialog({
    directory: true,
    multiple: false,
    title: "Open a jj or Git repository",
  });
  if (typeof selected === "string") {
    await openRepoAt(selected);
  }
}

// Folders dropped on the window open like picks from the dialog, one tab
// each (a subfolder or file opens its enclosing repo). Sequential, so
// each open settles into its own tab before the next claims the screen.
export async function openDroppedPaths(paths: string[]): Promise<void> {
  for (const path of paths) await openRepoAt(path);
}

// Switch the workbench's view lens. The choice sticks per repo (re-applied
// on open). Errors propagate so the picker can render a refused custom
// revset inline; the current snapshot and lens stay as they were.
export async function setSnapshotView(view: SnapshotView): Promise<void> {
  const snapshot = await api.setSnapshotView(view);
  applySnapshot(snapshot);
  persistView(snapshot.repoPath, view);
}

export async function refreshSnapshot(): Promise<void> {
  if (!app.snapshot) return;
  // An explicit refresh also re-asks GitHub for PR state. Deliberately
  // not awaited — the local snapshot must not wait on the network. The
  // git fetch itself stays with the upstream check (its chip and the
  // background cadence); ⌘R is the fast local refresh.
  void refreshForgePrs();
  try {
    await api.refreshSnapshot();
  } catch (error) {
    app.error = api.errorMessage(error);
  }
}

// Every mutation flows through this shape: backend action → refreshed
// snapshot → breadcrumb in app state → selection follows the outcome's
// target (the new working copy after `new`, the parent after abandon or
// squash — without this the selection would dangle on a removed node).
// Errors propagate to the caller so the initiating surface can show them
// inline (its editor or confirm panel stays open).
async function runMutation(
  call: () => Promise<MutationOutcome>,
): Promise<MutationOutcome> {
  // The mutation targets the repo on screen when it starts; if another
  // tab takes over mid-flight, results land on that repo's tab (via
  // applySnapshot) instead of the one now showing.
  const path = activeTabPath();
  const outcome = await call();
  // The refreshed snapshot also arrives via the snapshot event; pulling it
  // here makes the mutation flow self-contained and deterministic.
  const snapshot = path ? await api.currentSnapshot(path) : null;
  if (snapshot) applySnapshot(snapshot);
  if (activeTabPath() !== path) return outcome;
  if (outcome.operationId) {
    app.lastMutation = { outcome, at: Date.now() };
  }
  const target = outcome.targetChange;
  if (target && app.snapshot?.nodes.some((n) => n.id === target)) {
    app.selectedNodeId = target;
    app.selectedRange = null;
    const owner = stackPosition(app.snapshot, target)?.workstream;
    if (owner) app.focusedWorkstreamId = owner.id;
  }
  return outcome;
}

export function describeChange(
  changeId: string,
  description: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.describeChange(changeId, description));
}

export function newChange(parentChangeId: string): Promise<MutationOutcome> {
  return runMutation(() => api.newChange(parentChangeId));
}

export function editChange(changeId: string): Promise<MutationOutcome> {
  return runMutation(() => api.editChange(changeId));
}

export function abandonChange(changeId: string): Promise<MutationOutcome> {
  return runMutation(() => api.abandonChange(changeId));
}

export function squashChange(changeId: string): Promise<MutationOutcome> {
  return runMutation(() => api.squashChange(changeId));
}

// The diff pane's "discard changes to this file": the file returns to its
// parent-tree content within that change (jj restore, scoped). With hunks,
// only those hunks of the file's diff are discarded.
export function restoreFileChanges(
  changeId: string,
  path: string,
  hunks: SplitHunk[] | null = null,
): Promise<MutationOutcome> {
  return runMutation(() => api.restoreFile(changeId, path, hunks));
}

// The evolution panel's "restore this version": the change's tree and
// description return to that version, as a new undoable operation.
export function restoreChangeVersion(
  changeId: string,
  commitId: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.restoreChangeVersion(changeId, commitId));
}

export function duplicateChange(changeId: string): Promise<MutationOutcome> {
  return runMutation(() => api.duplicateChange(changeId));
}

export function revertChange(changeId: string): Promise<MutationOutcome> {
  return runMutation(() => api.revertChange(changeId));
}

export function absorbChange(changeId: string): Promise<MutationOutcome> {
  return runMutation(() => api.absorbChange(changeId));
}

export function splitChange(
  changeId: string,
  selection: SplitSelection[],
  description: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.splitChange(changeId, selection, description));
}

export function squashInto(
  changeId: string,
  selection: SplitSelection[],
  destinationId: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.squashInto(changeId, selection, destinationId));
}

export function rebaseChange(
  changeId: string,
  destinationId: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.rebaseChange(changeId, destinationId));
}

export function moveChange(
  changeId: string,
  destinationId: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.moveChange(changeId, destinationId));
}

export function createBookmark(
  name: string,
  changeId: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.createBookmark(name, changeId));
}

export function moveBookmark(
  name: string,
  changeId: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.moveBookmark(name, changeId));
}

export function renameBookmark(
  oldName: string,
  newName: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.renameBookmark(oldName, newName));
}

export function deleteBookmark(name: string): Promise<MutationOutcome> {
  return runMutation(() => api.deleteBookmark(name));
}

export function trackBookmark(
  name: string,
  remote: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.trackBookmark(name, remote));
}

export function untrackBookmark(
  name: string,
  remote: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.untrackBookmark(name, remote));
}

export function deleteRemoteBookmark(
  name: string,
  remote: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.deleteRemoteBookmark(name, remote));
}

export function pushBookmark(
  name: string,
  remote?: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.pushBookmarks([name], remote));
}

// Fetch a PR's head into a local review bookmark. The selection follows
// the fetched head (`targetChange`), so jumping to the workbench lands on
// the PR's diff.
export function fetchPr(
  remote: string,
  number: number | bigint,
  bookmark: string,
): Promise<MutationOutcome> {
  return runMutation(() => api.fetchPr(remote, number, bookmark));
}

// Hands one conflicted file to the external merge tool and waits for the
// tool's window to close. `app.resolvingConflict` stays set the whole time
// so every Resolve affordance shows the waiting state; errors propagate to
// the initiating surface like every other mutation.
export async function resolveConflict(
  changeId: string,
  path: string,
): Promise<MutationOutcome> {
  const repo = activeTabPath();
  app.resolvingConflict = { changeId, path };
  try {
    return await runMutation(() => api.resolveConflict(changeId, path));
  } finally {
    // The waiting state belongs to the repo's tab; clear it wherever the
    // tab stashed it if another one took the screen mid-resolve.
    if (activeTabPath() === repo) {
      app.resolvingConflict = null;
    } else if (repo) {
      const tab = tabForPath(repo);
      if (tab?.ui) tab.ui.resolvingConflict = null;
    }
  }
}

// Resolves one conflicted file by keeping one side wholesale (`:ours` /
// `:theirs` semantics). Instant — no tool window, so no waiting state;
// errors propagate to the initiating surface like every other mutation.
export function resolveConflictTake(
  changeId: string,
  path: string,
  side: ConflictSide,
): Promise<MutationOutcome> {
  return runMutation(() => api.resolveConflictTake(changeId, path, side));
}

// Recovers the current workspace when it went stale (`jj workspace
// update-stale`): local edits are recorded first, then the working copy
// catches up to where the repo moved. The checkout records no undoable
// operation, so success usually shows as the inbox item settling and the
// selection following the fresh working copy rather than as a breadcrumb.
export function updateStaleWorkspace(): Promise<MutationOutcome> {
  return runMutation(() => api.updateStaleWorkspace());
}

export function revertOperation(opId: string): Promise<MutationOutcome> {
  return runMutation(() => api.revertOperation(opId));
}

export function restoreOperation(opId: string): Promise<MutationOutcome> {
  return runMutation(() => api.restoreOperation(opId));
}

// The breadcrumb's Undo: revert the operation the last mutation recorded.
// The undo itself records an operation and becomes the new breadcrumb, so
// its own Undo is redo. Failures land in the status bar's error slot — the
// toast has no inline error surface of its own.
export async function undoLastMutation(): Promise<void> {
  const opId = app.lastMutation?.outcome.operationId;
  if (!opId) return;
  try {
    await revertOperation(opId);
  } catch (error) {
    app.error = api.errorMessage(error);
  }
}

export function dismissBreadcrumb(): void {
  app.lastMutation = null;
}

export function dismissError(): void {
  app.error = null;
}

export function showOperations(): void {
  app.section = "operations";
}

export function goToSection(section: Section): void {
  if (app.snapshot) app.section = section;
}

export function toggleSidebar(): void {
  app.sidebarCollapsed = !app.sidebarCollapsed;
  localStorage.setItem(SIDEBAR_KEY, app.sidebarCollapsed ? "1" : "0");
}

export function toggleGraphPanel(): void {
  app.graphCollapsed = !app.graphCollapsed;
  localStorage.setItem(GRAPH_KEY, app.graphCollapsed ? "1" : "0");
}

// ── Command palette ──

export function togglePalette(): void {
  app.paletteOpen = !app.paletteOpen;
  if (app.paletteOpen) app.searchOpen = false;
}

export function closePalette(): void {
  app.paletteOpen = false;
}

// ── History search (⌘⇧F) ──

export function openSearch(seed = ""): void {
  if (!app.snapshot) return;
  app.searchSeed = seed;
  app.searchOpen = true;
  app.paletteOpen = false;
}

export function closeSearch(): void {
  app.searchOpen = false;
}

// Select a change a search hit names, wherever it is: a change the current
// lens draws is a plain jump; one it does not (landed history under the
// default lens) re-renders the graph under a transient custom revset
// pinning that change. Deliberately not persisted — a refresh or lens
// switch returns to the user's own choice; within the session the lens
// picker shows the revset, so the detour stays visible and reversible.
export async function revealChange(id: string): Promise<void> {
  if (app.snapshot?.nodes.some((n) => n.id === id)) {
    jumpToChange(id);
    return;
  }
  const snapshot = await api.setSnapshotView({ kind: "custom", revset: id });
  applySnapshot(snapshot);
  jumpToChange(id);
}

// ── Annotate pane ──

// Open one file's line-by-line annotation over the diff surface. The
// workbench owns the pane; it renders while the annotated change stays
// selected and closes on any selection move (see WorkbenchView).
export function openAnnotate(changeId: string, path: string): void {
  app.section = "workbench";
  app.evolution = null;
  app.annotate = { changeId, path };
}

export function closeAnnotate(): void {
  app.annotate = null;
}

// ── Evolution pane ──

// Open a change's evolution timeline over the diff surface: every version
// it was rewritten through, each rewrite's diff, and restore. Shares the
// surface with annotate, so one replaces the other; closes like annotate
// does, on any selection move (see WorkbenchView).
export function openEvolution(changeId: string): void {
  app.section = "workbench";
  app.annotate = null;
  app.evolution = { changeId };
}

export function closeEvolution(): void {
  app.evolution = null;
}

// Every intent switches to the section whose surface owns it — the
// workbench for the change/diff panels, Publish for the land and ship
// plans — so the owning surface mounts (if it wasn't) and consumes the
// intent.
export function sendIntent(intent: UiIntent): void {
  app.section =
    intent.kind === "land" || intent.kind === "ship" ? "publish" : "workbench";
  app.intent = intent;
}

export function consumeIntent(): void {
  app.intent = null;
}

// Move the workbench selection to a change (the palette's "go to"),
// focusing its workstream like a graph-row click would.
export function jumpToChange(id: string): void {
  if (!app.snapshot?.nodes.some((n) => n.id === id)) return;
  app.section = "workbench";
  app.selectedNodeId = id;
  app.selectedRange = null;
  const owner = stackPosition(app.snapshot, id)?.workstream;
  if (owner) app.focusedWorkstreamId = owner.id;
}

// Palette-launched actions have no panel to render errors inline, so
// failures land in the status bar's error slot (like the breadcrumb Undo).
export async function runQuiet(call: () => Promise<unknown>): Promise<void> {
  try {
    await call();
  } catch (error) {
    app.error = api.errorMessage(error);
  }
}
