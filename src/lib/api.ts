// Typed wrappers around the Tauri command surface and snapshot events.
// This is the only file that should touch `invoke`/`listen` directly.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ChangeDiff } from "$lib/bindings/ChangeDiff";
import type { ForgeStatus } from "$lib/bindings/ForgeStatus";
import type { LandOutcome } from "$lib/bindings/LandOutcome";
import type { LandPlan } from "$lib/bindings/LandPlan";
import type { MutationOutcome } from "$lib/bindings/MutationOutcome";
import type { RepoPrState } from "$lib/bindings/RepoPrState";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SplitSelection } from "$lib/bindings/SplitSelection";
import type { SubmitOutcome } from "$lib/bindings/SubmitOutcome";
import type { SubmitPlan } from "$lib/bindings/SubmitPlan";

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

// Move the selected content into an existing change (`jj squash --from
// --into <paths>`): the destination takes it wherever it sits in the graph;
// a selection covering everything abandons the emptied source.
export function squashInto(
  changeId: string,
  selection: SplitSelection[],
  destinationId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("squash_into", {
    changeId,
    selection,
    destinationId,
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

// Fetch from the repo's git remotes (`jj git fetch`) — the upstream check.
// Stays pending for as long as the network takes; the backend publishes
// the refreshed snapshot when remote state moved.
export function gitFetch(): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("git_fetch");
}

// The forge connection (GitHub). `forgeStatus` answers without touching
// the network; `forgeVerify` checks the resolved token against the API and
// remembers the login for the session. Login validates before storing the
// token in the system keychain; logout only removes Jiji's stored token —
// tokens managed outside Jiji (environment, gh CLI) stay.
export function forgeStatus(): Promise<ForgeStatus> {
  return invoke<ForgeStatus>("forge_status");
}

export function forgeVerify(): Promise<ForgeStatus> {
  return invoke<ForgeStatus>("forge_verify");
}

export function forgeLogin(token: string): Promise<ForgeStatus> {
  return invoke<ForgeStatus>("forge_login", { token });
}

export function forgeLogout(): Promise<ForgeStatus> {
  return invoke<ForgeStatus>("forge_logout");
}

// Open-PR state of the detected repo — what PR badges and publish flows
// render (one batched query on the backend), with the fork-filtered
// head-branch → PR attachment map already built.
export function forgePrs(): Promise<RepoPrState> {
  return invoke<RepoPrState>("forge_prs");
}

// Plan submitting the stack under a bookmark: which bookmarks push, which
// PRs open against which bases, which existing PRs retarget. Read-only —
// the plan is the confirm step, nothing runs yet.
export function submitPlan(headBookmark: string): Promise<SubmitPlan> {
  return invoke<SubmitPlan>("submit_plan", { headBookmark });
}

// Execute a confirmed plan. The backend re-derives it first and refuses
// with code `plan_stale` when the stack or GitHub moved since the panel
// rendered it.
export function submitStack(
  headBookmark: string,
  plan: SubmitPlan,
): Promise<SubmitOutcome> {
  return invoke<SubmitOutcome>("submit_stack", { headBookmark, plan });
}

// Plan landing the stack under a bookmark: what already merged, whether
// the bottom PR merges now (or hands to GitHub's automation), and the
// reconcile that follows. Read-only — the plan is the confirm step.
export function landPlan(headBookmark: string): Promise<LandPlan> {
  return invoke<LandPlan>("land_plan", { headBookmark });
}

// Execute a confirmed land plan. The backend re-derives it first and
// refuses with code `plan_stale` when the stack or GitHub moved since the
// panel rendered it, and the merge re-checks GitHub once more before it
// runs.
export function landStack(
  headBookmark: string,
  plan: LandPlan,
): Promise<LandOutcome> {
  return invoke<LandOutcome>("land_stack", { headBookmark, plan });
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
