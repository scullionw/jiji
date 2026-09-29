// Typed wrappers around the Tauri command surface and snapshot events.
// This is the only file that should touch `invoke`/`listen` directly.
//
// Repo-scoped commands name their repo explicitly (several can be open at
// once — the shell's tabs); the wrappers stamp the active tab's path on
// each call, so every surface keeps calling them without threading paths
// around. A call made for one tab keeps targeting that repo even if
// another tab takes the screen mid-flight — the path is captured when the
// call starts.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { AutoLandStatus } from "$lib/bindings/AutoLandStatus";
import type { ChangeDiff } from "$lib/bindings/ChangeDiff";
import type { ChangeEvolog } from "$lib/bindings/ChangeEvolog";
import type { FileAnnotation } from "$lib/bindings/FileAnnotation";
import type { CiRerunReport } from "$lib/bindings/CiRerunReport";
import type { ConflictSide } from "$lib/bindings/ConflictSide";
import type { ForgeStatus } from "$lib/bindings/ForgeStatus";
import type { HistorySearch } from "$lib/bindings/HistorySearch";
import type { LandOptions } from "$lib/bindings/LandOptions";
import type { LandOutcome } from "$lib/bindings/LandOutcome";
import type { LandPlan } from "$lib/bindings/LandPlan";
import type { ShipOutcome } from "$lib/bindings/ShipOutcome";
import type { ShipPlan } from "$lib/bindings/ShipPlan";
import type { MutationOutcome } from "$lib/bindings/MutationOutcome";
import type { PrSummary } from "$lib/bindings/PrSummary";
import type { RepoPrState } from "$lib/bindings/RepoPrState";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SnapshotView } from "$lib/bindings/SnapshotView";
import type { SplitHunk } from "$lib/bindings/SplitHunk";
import type { SplitSelection } from "$lib/bindings/SplitSelection";
import type { SubmitHead } from "$lib/bindings/SubmitHead";
import type { SubmitOutcome } from "$lib/bindings/SubmitOutcome";
import type { SubmitPlan } from "$lib/bindings/SubmitPlan";
import { activeTabPath } from "$lib/state/tabs.svelte";

export interface CommandError {
  code: string;
  message: string;
  /** The folder the error is about, when the UI can act on it: the Git
   * root a `git_repo_without_jj` refusal offers to set jj up in. */
  path?: string;
}

const SNAPSHOT_UPDATED_EVENT = "snapshot://updated";
const AUTOLAND_STATE_EVENT = "autoland://state";
const CLI_OPEN_REQUEST_EVENT = "cli://open-request";
const MENU_ACTION_EVENT = "menu://action";

/** The repo a repo-scoped command targets: the active tab's. Refusing
 * without one (instead of sending null) keeps a routing bug loud; the
 * thrown shape matches CommandError so error rendering stays uniform. */
function repoPath(): string {
  const path = activeTabPath();
  if (!path) {
    throw {
      code: "no_repo_open",
      message: "No repository is currently open",
    } satisfies CommandError;
  }
  return path;
}

// The `jiji` terminal launcher. Not a ts-rs binding: the type lives in
// `src-tauri` (a desktop-shell concern, not repo domain), outside the
// crates the bindings script exports.
export interface CliStatus {
  installed: boolean;
  upgradeAvailable?: boolean;
  /** Where our launcher lives, when installed. */
  path: string | null;
  /** A foreign `jiji` occupying a launcher location; installing would
   * clobber it, so the UI explains instead of offering. */
  conflict: string | null;
}

export function openRepo(path: string): Promise<RepoSnapshot> {
  return invoke<RepoSnapshot>("open_repo", { path });
}

// Set jj up in the plain Git repo enclosing `path` (the CLI's `jj git init
// --colocate` at its root). Resolves to the root, ready for `openRepo`.
export function initJjRepo(path: string): Promise<string> {
  return invoke<string>("init_jj_repo", { path });
}

// Drop a repo's backend session (its tab closed): the watcher stops and a
// live auto-land job for it detaches into its resumable record.
export function closeRepo(path: string): Promise<void> {
  return invoke("close_repo", { repoPath: path });
}

export function refreshSnapshot(): Promise<RepoSnapshot> {
  return invoke<RepoSnapshot>("refresh_snapshot", { repoPath: repoPath() });
}

// The latest published snapshot of one open repo — explicit path because
// tab recovery asks for repos that are not on screen yet.
export function currentSnapshot(path: string): Promise<RepoSnapshot | null> {
  return invoke<RepoSnapshot | null>("current_snapshot", { repoPath: path });
}

// Switch the view lens snapshots render; the refreshed snapshot publishes
// under the new lens. A custom revset that fails to parse or resolve
// rejects with code `revset_invalid` and changes nothing.
export function setSnapshotView(
  view: SnapshotView,
  path = repoPath(),
): Promise<RepoSnapshot> {
  return invoke<RepoSnapshot>("set_snapshot_view", {
    repoPath: path,
    view,
  });
}

export function changeDiff(changeId: string, path = repoPath()): Promise<ChangeDiff> {
  return invoke<ChangeDiff>("change_diff", { repoPath: path, changeId });
}

// Commit-to-commit comparison (`jj diff --from --to`); the result echoes
// both ends so the surface can match it to the comparison it shows.
export function compareDiff(
  fromChangeId: string,
  toChangeId: string,
  path = repoPath(),
): Promise<ChangeDiff> {
  return invoke<ChangeDiff>("compare_diff", {
    repoPath: path,
    fromChangeId,
    toChangeId,
  });
}

// One change's evolution (`jj evolog`): its predecessor versions, newest
// first, with the operations that recorded them. Read-only; per-version
// diffs go through compareDiff, which resolves hidden commit ids.
export function changeEvolog(changeId: string): Promise<ChangeEvolog> {
  return invoke<ChangeEvolog>("change_evolog", {
    repoPath: repoPath(),
    changeId,
  });
}

// One file annotated line by line (`jj file annotate`): which change
// introduced each line as the file stands in this change's tree.
// Read-only; line origins are addressed like graph node ids for jumping.
export function fileAnnotate(
  changeId: string,
  filePath: string,
): Promise<FileAnnotation> {
  return invoke<FileAnnotation>("file_annotate", {
    repoPath: repoPath(),
    changeId,
    filePath,
  });
}

// History content/filename search (the pickaxe): which changes introduced
// or removed the string, or touched a path containing it. Read-only and
// bounded — the result reports how far the walk got.
export function searchHistory(query: string): Promise<HistorySearch> {
  return invoke<HistorySearch>("search_history", {
    repoPath: repoPath(),
    query,
  });
}

// Mutations: the backend mutates, refreshes the snapshot (published to all
// surfaces), and answers with the operation breadcrumb.
export function describeChange(
  changeId: string,
  description: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("describe_change", {
    repoPath: repoPath(),
    changeId,
    description,
  });
}

export function newChange(parentChangeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("new_change", {
    repoPath: repoPath(),
    parentChangeId,
  });
}

export function editChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("edit_change", {
    repoPath: repoPath(),
    changeId,
  });
}

export function abandonChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("abandon_change", {
    repoPath: repoPath(),
    changeId,
  });
}

// Discard a file's diff from a change (`jj restore`, scoped). With hunks,
// only those hunks are discarded; the backend verifies the coordinates
// against a fresh diff and refuses when the change moved underneath.
export function restoreFile(
  changeId: string,
  filePath: string,
  hunks: SplitHunk[] | null = null,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("restore_file", {
    repoPath: repoPath(),
    changeId,
    filePath,
    hunks,
  });
}

// Restore a change to one of its earlier versions (tree and description;
// the change does not move). The commit id must name a row of the change's
// own evolution — the backend refuses anything else.
export function restoreChangeVersion(
  changeId: string,
  commitId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("restore_change_version", {
    repoPath: repoPath(),
    changeId,
    commitId,
  });
}

export function duplicateChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("duplicate_change", {
    repoPath: repoPath(),
    changeId,
  });
}

// Back out a change (`jj revert`): a new change on top of the working copy
// whose tree applies the reverse of the target's diff.
export function revertChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("revert_change", {
    repoPath: repoPath(),
    changeId,
  });
}

export function absorbChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("absorb_change", {
    repoPath: repoPath(),
    changeId,
  });
}

export function squashChange(changeId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("squash_change", {
    repoPath: repoPath(),
    changeId,
  });
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
    repoPath: repoPath(),
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
    repoPath: repoPath(),
    changeId,
    selection,
    destinationId,
  });
}

export function rebaseChange(
  changeId: string,
  destinationId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("rebase_change", {
    repoPath: repoPath(),
    changeId,
    destinationId,
  });
}

export function moveChange(
  changeId: string,
  destinationId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("move_change", {
    repoPath: repoPath(),
    changeId,
    destinationId,
  });
}

export function createBookmark(
  name: string,
  changeId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("create_bookmark", {
    repoPath: repoPath(),
    name,
    changeId,
  });
}

export function moveBookmark(
  name: string,
  changeId: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("move_bookmark", {
    repoPath: repoPath(),
    name,
    changeId,
  });
}

export function renameBookmark(
  oldName: string,
  newName: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("rename_bookmark", {
    repoPath: repoPath(),
    oldName,
    newName,
  });
}

export function deleteBookmark(name: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("delete_bookmark", {
    repoPath: repoPath(),
    name,
  });
}

// Start tracking a remote bookmark (`jj bookmark track name@remote`): the
// local bookmark takes the remote position and follows future fetches.
export function trackBookmark(
  name: string,
  remote: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("track_bookmark", {
    repoPath: repoPath(),
    name,
    remote,
  });
}

export function untrackBookmark(
  name: string,
  remote: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("untrack_bookmark", {
    repoPath: repoPath(),
    name,
    remote,
  });
}

// Push bookmarks to a remote; omitting `remote` resolves like the CLI
// (the `git.push` setting, else the sole remote, else "origin").
export function pushBookmarks(
  names: string[],
  remote?: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("push_bookmarks", {
    repoPath: repoPath(),
    names,
    remote: remote ?? null,
  });
}

// Delete a branch on the remote (and its local bookmark when one exists);
// the push carries force-with-lease safety, so a remote that moved since
// the last fetch refuses instead of deleting someone's new work.
export function deleteRemoteBookmark(
  name: string,
  remote: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("delete_remote_bookmark", {
    repoPath: repoPath(),
    name,
    remote,
  });
}

// Operation time travel (`jj op revert` / `jj op restore`); opId is the hex
// prefix operation rows and breadcrumbs carry.
export function revertOperation(opId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("revert_operation", {
    repoPath: repoPath(),
    opId,
  });
}

export function restoreOperation(opId: string): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("restore_operation", {
    repoPath: repoPath(),
    opId,
  });
}

// Launches the external merge tool for one conflicted file and resolves
// when the tool's window closes — this call can stay pending for minutes.
export function resolveConflict(
  changeId: string,
  filePath: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("resolve_conflict", {
    repoPath: repoPath(),
    changeId,
    filePath,
  });
}

// Resolves one conflicted file by keeping one side wholesale (the CLI's
// built-in `:ours`/`:theirs` merge tools: ours is side #1 of the conflict,
// theirs side #2). No external tool launches.
export function resolveConflictTake(
  changeId: string,
  filePath: string,
  side: ConflictSide,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("resolve_conflict_take", {
    repoPath: repoPath(),
    changeId,
    filePath,
    side,
  });
}

// Recovers a stale working copy (`jj workspace update-stale`) — the inbox's
// guided recovery for the current workspace.
export function updateStaleWorkspace(): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("update_stale_workspace", {
    repoPath: repoPath(),
  });
}

// Fetch from the repo's git remotes (`jj git fetch`) — the upstream check.
// Stays pending for as long as the network takes; the backend publishes
// the refreshed snapshot when remote state moved.
export function gitFetch(): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("git_fetch", { repoPath: repoPath() });
}

// Fetch a pull request's head (`refs/pull/N/head`, cross-fork included)
// into a new local bookmark for local review. Network-blocking like any
// fetch. (PR numbers are `bigint` in the ts-rs DTOs; the wire wants a
// plain number.)
export function fetchPr(
  remote: string,
  number: number | bigint,
  bookmark: string,
): Promise<MutationOutcome> {
  return invoke<MutationOutcome>("fetch_pr", {
    repoPath: repoPath(),
    remote,
    number: Number(number),
    bookmark,
  });
}

// The forge connection (GitHub). `forgeStatus` answers without touching
// the network; `forgeVerify` checks the resolved token against the API and
// remembers the login for the session. Login validates before storing the
// token in the system keychain; logout only removes Jiji's stored token —
// tokens managed outside Jiji (environment, gh CLI) stay.
export function forgeStatus(): Promise<ForgeStatus> {
  return invoke<ForgeStatus>("forge_status", { repoPath: repoPath() });
}

export function forgeVerify(): Promise<ForgeStatus> {
  return invoke<ForgeStatus>("forge_verify", { repoPath: repoPath() });
}

export function forgeLogin(token: string): Promise<ForgeStatus> {
  return invoke<ForgeStatus>("forge_login", { repoPath: repoPath(), token });
}

export function forgeLogout(): Promise<ForgeStatus> {
  return invoke<ForgeStatus>("forge_logout", { repoPath: repoPath() });
}

// Open-PR state of the detected repo — what PR badges and publish flows
// render (one batched query on the backend), with the fork-filtered
// head-branch → PR attachment map already built.
export function forgePrs(): Promise<RepoPrState> {
  return invoke<RepoPrState>("forge_prs", { repoPath: repoPath() });
}

// One PR by number — the review flow's lookup for PRs the batched open-PR
// state cannot see (past the 100 cap, or closed).
export function forgePr(number: number | bigint): Promise<PrSummary> {
  return invoke<PrSummary>("forge_pr", {
    repoPath: repoPath(),
    number: Number(number),
  });
}

// Re-run the failed GitHub Actions runs on a PR's current head. The
// answer names which workflow runs went back to work; empty means the
// failing check lives outside Actions' reach.
export function rerunFailedCi(
  number: number | bigint,
): Promise<CiRerunReport> {
  return invoke<CiRerunReport>("rerun_failed_ci", {
    repoPath: repoPath(),
    number: Number(number),
  });
}

// Plan submitting the stack under a head — a bookmark, or an unbookmarked
// head change whose branch name the plan derives: which bookmarks push,
// which PRs open against which bases, which existing PRs retarget.
// Read-only — the plan is the confirm step, nothing runs yet.
export function submitPlan(head: SubmitHead): Promise<SubmitPlan> {
  return invoke<SubmitPlan>("submit_plan", { repoPath: repoPath(), head });
}

// Execute a confirmed plan. The backend re-derives it first and refuses
// with code `plan_stale` when the stack or GitHub moved since the panel
// rendered it.
export function submitStack(
  head: SubmitHead,
  plan: SubmitPlan,
): Promise<SubmitOutcome> {
  return invoke<SubmitOutcome>("submit_stack", {
    repoPath: repoPath(),
    head,
    plan,
  });
}

// Plan landing the stack under a bookmark: what already merged, whether
// the bottom PR merges now (or hands to GitHub's automation), and the
// reconcile that follows. Read-only — the plan is the confirm step.
// Options pick the merge method and the bypass posture; the plan carries
// them, so execution re-derives under the same rules.
export function landPlan(
  headBookmark: string,
  options?: LandOptions,
): Promise<LandPlan> {
  return invoke<LandPlan>("land_plan", {
    repoPath: repoPath(),
    headBookmark,
    options: options ?? null,
  });
}

// Execute a confirmed land plan. The backend re-derives it first and
// refuses with code `plan_stale` when the stack or GitHub moved since the
// panel rendered it, and the merge re-checks GitHub once more before it
// runs.
export function landStack(
  headBookmark: string,
  plan: LandPlan,
): Promise<LandOutcome> {
  return invoke<LandOutcome>("land_stack", {
    repoPath: repoPath(),
    headBookmark,
    plan,
  });
}

// Plan shipping the stack under a change directly to trunk — no PR. The
// backend fetches from the remote first (the spec's fetch-then-preview),
// so this can record a fetch op and take a moment; the plan itself is the
// confirm step, nothing else runs.
export function shipPlan(headChange: string): Promise<ShipPlan> {
  return invoke<ShipPlan>("ship_plan", { repoPath: repoPath(), headChange });
}

// Execute a confirmed ship plan. The backend re-derives it first and
// refuses with code `plan_stale` when the stack moved since the panel
// rendered it; a remote trunk moved since the plan's fetch refuses at the
// push step (force-with-lease) — re-running Ship is the retry.
export function shipStack(
  headChange: string,
  plan: ShipPlan,
): Promise<ShipOutcome> {
  return invoke<ShipOutcome>("ship_stack", {
    repoPath: repoPath(),
    headChange,
    plan,
  });
}

// Queue the stack under a bookmark for auto-land: a supervised background
// job that keeps re-deriving the landing plan and running rounds as remote
// conditions allow. Refused while another job is running. When a
// persisted record matches this repo and bookmark (a restart survivor),
// the job resumes from it — Resume and re-queue are the same call.
// Answers every job status the host knows, the new job's live among them.
// The job pins the repo path at queueing, so other tabs taking the screen
// never disturb it.
export function autolandStart(
  headBookmark: string,
  options?: LandOptions,
): Promise<AutoLandStatus[]> {
  return invoke<AutoLandStatus[]>("autoland_start", {
    repoPath: repoPath(),
    headBookmark,
    options: options ?? null,
  });
}

// Ask the running job to stop. Answers the statuses as of the request; the
// definitive stopped phase arrives on the state event once the job winds
// down (immediately from a wait, after the round from a round).
export function autolandStop(): Promise<AutoLandStatus[]> {
  return invoke<AutoLandStatus[]>("autoland_stop");
}

// Every job status the host knows, terminal states and restart survivors
// included — how a reloading frontend reattaches. A record with live:
// false and a non-terminal phase is an interrupted job offering resume.
export function autolandState(): Promise<AutoLandStatus[]> {
  return invoke<AutoLandStatus[]>("autoland_state");
}

// Clear one finished or interrupted job record, memory and disk both.
// Refused for the record the running job is driving; every other record
// dismisses independently. Explicitly pathed: records outlive tabs.
export function autolandDismiss(
  repoPath: string,
  headBookmark: string,
): Promise<AutoLandStatus[]> {
  return invoke<AutoLandStatus[]>("autoland_dismiss", {
    repoPath,
    headBookmark,
  });
}

export function cliStatus(): Promise<CliStatus> {
  return invoke<CliStatus>("cli_status");
}

// Both mutate then re-read, so the result is the launcher's actual state.
// Install rejects with `install_canceled` when the admin prompt is
// declined and `cli_conflict` when a foreign `jiji` is in the way.
export function installCli(): Promise<CliStatus> {
  return invoke<CliStatus>("install_cli");
}

export function uninstallCli(): Promise<CliStatus> {
  return invoke<CliStatus>("uninstall_cli");
}

// Editor CLIs detected on this machine — a desktop-shell concern like
// CliStatus, so the type is mirrored here rather than generated.
export interface EditorInfo {
  id: string;
  label: string;
}

export function detectEditors(): Promise<EditorInfo[]> {
  return invoke<EditorInfo[]>("detect_editors");
}

export function openInEditor(
  editor: string,
  path: string,
  line?: number,
): Promise<void> {
  return invoke("open_in_editor", { editor, path, line: line ?? null });
}

// Terminal apps answer with the same id+label shape as editors.
export type TerminalInfo = EditorInfo;

export function detectTerminals(): Promise<TerminalInfo[]> {
  return invoke<TerminalInfo[]>("detect_terminals");
}

export function openInTerminal(path: string, terminal?: string): Promise<void> {
  return invoke("open_in_terminal", { path, terminal: terminal ?? null });
}

// One-shot drain of a repo path handed over by the CLI launcher (or a
// folder dropped on the Dock icon). The backend parks the path and pings
// (`onCliOpenRequest`); draining on the ping and once at bootstrap covers
// a launch that delivered the path before any listener existed.
export function takePendingOpen(): Promise<string | null> {
  return invoke<string | null>("take_pending_open");
}

export function onCliOpenRequest(callback: () => void): Promise<UnlistenFn> {
  return listen(CLI_OPEN_REQUEST_EVENT, () => {
    callback();
  });
}

export function onSnapshotUpdated(
  callback: (snapshot: RepoSnapshot) => void,
): Promise<UnlistenFn> {
  return listen<RepoSnapshot>(SNAPSHOT_UPDATED_EVENT, (event) => {
    callback(event.payload);
  });
}

export function onAutoLandState(
  callback: (statuses: AutoLandStatus[]) => void,
): Promise<UnlistenFn> {
  return listen<AutoLandStatus[]>(AUTOLAND_STATE_EVENT, (event) => {
    callback(event.payload);
  });
}

// A native menu bar choice; the payload is the item id (menu.rs). Only
// mouse-driven picks normally arrive — key equivalents are handled by the
// in-webview shortcuts first, which preventDefault and stop the re-dispatch.
export function onMenuAction(
  callback: (id: string) => void,
): Promise<UnlistenFn> {
  return listen<string>(MENU_ACTION_EVENT, (event) => {
    callback(event.payload);
  });
}

// ── Native window shell ──

/** Names the window for Mission Control, the Dock, and ⌘-Tab — the
 * overlay title bar never renders it. */
export function setWindowTitle(title: string): void {
  void getCurrentWindow()
    .setTitle(title)
    .catch((error) => console.warn("failed to set window title", error));
}

/** Files and folders dragged onto the window from Finder — the window's
 * twin of dropping a folder on the Dock icon. `onHover` gets the dragged
 * paths when a drag enters and an empty list when it leaves or drops;
 * `onDrop` gets the dropped paths. */
export function onPathDrop(
  onHover: (paths: string[]) => void,
  onDrop: (paths: string[]) => void,
): Promise<UnlistenFn> {
  return getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload;
    if (payload.type === "enter") {
      onHover(payload.paths);
    } else if (payload.type === "leave") {
      onHover([]);
    } else if (payload.type === "drop") {
      onHover([]);
      if (payload.paths.length > 0) onDrop(payload.paths);
    }
  });
}

/** Reports fullscreen transitions (macOS traffic lights auto-hide there,
 * so the shell can drop its clearance). Fires once with the current state. */
export function onFullscreenChange(
  callback: (fullscreen: boolean) => void,
): Promise<UnlistenFn> {
  const win = getCurrentWindow();
  const sync = () =>
    win
      .isFullscreen()
      .then(callback)
      .catch((error) => console.warn("failed to read fullscreen state", error));
  void sync();
  return win.onResized(() => void sync());
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

// Complete local stacks, independent of the workbench graph filter.
export function publishSnapshot(): Promise<RepoSnapshot> {
  return invoke<RepoSnapshot>("publish_snapshot", { repoPath: repoPath() });
}

/** The desktop and CLI use the same Rust stack discovery. */
export function publishStacks(snapshot: RepoSnapshot, prs: RepoPrState | null): Promise<import("$lib/bindings/PublishStack").PublishStack[]> {
  return invoke("publish_stacks", { snapshot, prs });
}
