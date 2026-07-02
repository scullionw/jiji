// Typed wrappers around the Tauri command surface and snapshot events.
// This is the only file that should touch `invoke`/`listen` directly.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ChangeDiff } from "$lib/bindings/ChangeDiff";
import type { MutationOutcome } from "$lib/bindings/MutationOutcome";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SplitSelection } from "$lib/bindings/SplitSelection";

export interface CommandError {
  code: string;
  message: string;
}

const SNAPSHOT_UPDATED_EVENT = "snapshot://updated";

export function openRepo(path: string): Promise<RepoSnapshot> {
  return invoke<RepoSnapshot>("open_repo", { path });
}

export function refreshSnapshot(): Promise<RepoSnapshot> {
  return invoke<RepoSnapshot>("refresh_snapshot");
}

export function currentSnapshot(): Promise<RepoSnapshot | null> {
  return invoke<RepoSnapshot | null>("current_snapshot");
}

export function changeDiff(changeId: string): Promise<ChangeDiff> {
  return invoke<ChangeDiff>("change_diff", { changeId });
}

// Commit-to-commit comparison (`jj diff --from --to`); the result echoes
// both ends so the surface can match it to the comparison it shows.
export function compareDiff(
  fromChangeId: string,
  toChangeId: string,
): Promise<ChangeDiff> {
  return invoke<ChangeDiff>("compare_diff", { fromChangeId, toChangeId });
}

// Mutations: the backend mutates, refreshes the snapshot (published to all
// surfaces), and answers with the operation breadcrumb.
export function describeChange(
  changeId: string,
  description: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("describe_change", { changeId, description });
}

export function newChange(parentChangeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("new_change", { parentChangeId });
}

export function editChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("edit_change", { changeId });
}

export function abandonChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("abandon_change", { changeId });
}

export function squashChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("squash_change", { changeId });
}

// Split a change in two (`jj split`): the selected content — whole files,
// or just the chosen hunks of a file — stays in the change itself with the
// new description; the rest moves to a new change on top, which inherits
// bookmarks, descendants, and working-copy status.
export function splitChange(
  changeId: string,
  selection: SplitSelection[],
  description: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("split_change", {
    changeId,
    selection,
    description,
  });
}

export function rebaseChange(
  changeId: string,
  destinationId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("rebase_change", { changeId, destinationId });
}

export function moveChange(
  changeId: string,
  destinationId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("move_change", { changeId, destinationId });
}

export function createBookmark(
  name: string,
  changeId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("create_bookmark", { name, changeId });
}

export function moveBookmark(
  name: string,
  changeId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("move_bookmark", { name, changeId });
}

export function renameBookmark(
  oldName: string,
  newName: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("rename_bookmark", { oldName, newName });
}

export function deleteBookmark(name: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("delete_bookmark", { name });
}

// Operation time travel (`jj op revert` / `jj op restore`); opId is the hex
// prefix operation rows and breadcrumbs carry.
export function revertOperation(opId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("revert_operation", { opId });
}

export function restoreOperation(opId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("restore_operation", { opId });
}

// Launches the external merge tool for one conflicted file and resolves
// when the tool's window closes — this call can stay pending for minutes.
export function resolveConflict(
  changeId: string,
  filePath: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("resolve_conflict", { changeId, filePath });
}

// Recovers a stale working copy (`jj workspace update-stale`) — the inbox's
// guided recovery for the current workspace.
export function updateStaleWorkspace(): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("update_stale_workspace");
}

export function onSnapshotUpdated(
  callback: (snapshot: RepoSnapshot) => void,
): Promise<UnlistenFn> {
  return listen<RepoSnapshot>(SNAPSHOT_UPDATED_EVENT, (event) => {
    callback(event.payload);
  });
}

export function errorMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    return error.message;
  }
  return String(error);
}
