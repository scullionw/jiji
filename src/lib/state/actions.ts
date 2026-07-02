// User-facing actions. Each one talks to the backend through `$lib/api`
// and writes results into the shared app state.

import { open as openDialog } from "@tauri-apps/plugin-dialog";
import * as api from "$lib/api";
import type { MutationOutcome } from "$lib/bindings/MutationOutcome";
import { stackPosition } from "$lib/components/inspector/inspect";
import { app, type Section, type UiIntent } from "./app.svelte";
import { loadRecentRepos, rememberRepo } from "./recent";

export async function bootstrap(): Promise<void> {
  try {
    app.recentRepos = await loadRecentRepos();
  } catch (error) {
    console.warn("failed to load recent repos", error);
  }

  // App-lifetime subscription: every snapshot the backend publishes becomes
  // the new source of truth for all surfaces.
  await api.onSnapshotUpdated((snapshot) => {
    app.snapshot = snapshot;
    app.error = null;
  });

  // Recover state if the webview reloaded while a repo was open.
  try {
    const current = await api.currentSnapshot();
    if (current) app.snapshot = current;
  } catch (error) {
    console.warn("failed to fetch current snapshot", error);
  }
}

export async function openRepoAt(path: string): Promise<void> {
  app.opening = true;
  app.error = null;
  try {
    const snapshot = await api.openRepo(path);
    app.snapshot = snapshot;
    app.section = "workbench";
    app.recentRepos = await rememberRepo(path, snapshot.repoName);
  } catch (error) {
    app.error = api.errorMessage(error);
  } finally {
    app.opening = false;
  }
}

export async function chooseRepo(): Promise<void> {
  const selected = await openDialog({
    directory: true,
    multiple: false,
    title: "Open a JJ repository",
  });
  if (typeof selected === "string") {
    await openRepoAt(selected);
  }
}

export async function refreshSnapshot(): Promise<void> {
  if (!app.snapshot) return;
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
  const outcome = await call();
  // The refreshed snapshot also arrives via the snapshot event; pulling it
  // here makes the mutation flow self-contained and deterministic.
  const snapshot = await api.currentSnapshot();
  if (snapshot) app.snapshot = snapshot;
  if (outcome.operationId) {
    app.lastMutation = { outcome, at: Date.now() };
  }
  const target = outcome.targetChange;
  if (target && app.snapshot?.nodes.some((n) => n.id === target)) {
    app.selectedNodeId = target;
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

// Hands one conflicted file to the external merge tool and waits for the
// tool's window to close. `app.resolvingConflict` stays set the whole time
// so every Resolve affordance shows the waiting state; errors propagate to
// the initiating surface like every other mutation.
export async function resolveConflict(
  changeId: string,
  path: string,
): Promise<MutationOutcome> {
  app.resolvingConflict = { changeId, path };
  try {
    return await runMutation(() => api.resolveConflict(changeId, path));
  } finally {
    app.resolvingConflict = null;
  }
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

export function showOperations(): void {
  app.section = "operations";
}

export function goToSection(section: Section): void {
  if (app.snapshot) app.section = section;
}

// ── Command palette ──

export function togglePalette(): void {
  app.paletteOpen = !app.paletteOpen;
}

export function closePalette(): void {
  app.paletteOpen = false;
}

// Every intent targets a workbench surface, so the section switches along;
// the owning surface mounts (if it wasn't) and consumes the intent.
export function sendIntent(intent: UiIntent): void {
  app.section = "workbench";
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
  const owner = stackPosition(app.snapshot, id)?.workstream;
  if (owner) app.focusedWorkstreamId = owner.id;
}

// Palette-launched mutations have no panel to render errors inline, so
// failures land in the status bar's error slot (like the breadcrumb Undo).
export async function runQuiet(
  call: () => Promise<MutationOutcome>,
): Promise<void> {
  try {
    await call();
  } catch (error) {
    app.error = api.errorMessage(error);
  }
}
