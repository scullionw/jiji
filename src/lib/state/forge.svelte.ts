// The forge connection and open-PR state, shared by every surface that
// renders it: the Publish section's connection cards and the workbench's
// PR badges. One source of truth, so connecting in Publish lights the
// badges up immediately and both read the same fetch.
//
// The fetch cadence is deliberately manual for now — repo open (or the
// repo's remotes changing out from under us), ⌘R, and connection changes.
// The background cadence belongs to M4's upstream-checks item.

import * as api from "$lib/api";
import type { ForgeStatus } from "$lib/bindings/ForgeStatus";
import type { GitRemote } from "$lib/bindings/GitRemote";
import type { RepoPrState } from "$lib/bindings/RepoPrState";

export type ForgePhase = "idle" | "loading" | "verifying" | "ready";

export const forge = $state({
  status: null as ForgeStatus | null,
  phase: "idle" as ForgePhase,
  /** Connection-level failure (status/verify); Publish renders it. */
  error: null as string | null,
  /** Open-PR state of the detected repo; null until fetched. */
  prs: null as RepoPrState | null,
  /** PR-fetch failure: badges quietly stay absent, Publish names it. */
  prsError: null as string | null,
  prsLoading: false,
});

// Stale-response guards: only the newest sync/fetch may write back.
let syncSeq = 0;
let prsSeq = 0;
let lastRemotesKey: string | null = null;

/** PRs are worth fetching once a repo is detected and the token verified
 * this session — "Connected as …" is earned, and badges ride on it. */
function connectionReady(): boolean {
  return forge.status?.repo != null && forge.status?.auth.login != null;
}

/** Re-derive the connection when the open repo's remotes change (open,
 * repo switch, remote edits). Keyed on the remotes' JSON, so the snapshot
 * refreshes that leave them alone cost nothing. */
export async function syncForgeConnection(
  remotes: GitRemote[] | null,
): Promise<void> {
  const remotesKey = JSON.stringify(remotes);
  if (remotesKey === lastRemotesKey) return;
  lastRemotesKey = remotesKey;
  const seq = ++syncSeq;
  prsSeq += 1; // orphan any in-flight PR fetch for the previous repo
  forge.prs = null;
  forge.prsError = null;
  forge.prsLoading = false;
  forge.error = null;
  if (remotes === null) {
    // No open repo: back to the blank slate.
    forge.status = null;
    forge.phase = "idle";
    return;
  }
  forge.phase = "loading";
  try {
    let status = await api.forgeStatus();
    if (seq !== syncSeq) return;
    forge.status = status;
    // A token without a session-verified login: check it against the API
    // once so "Connected as …" is earned, not assumed.
    if (status.auth.source && !status.auth.login) {
      forge.phase = "verifying";
      status = await api.forgeVerify();
      if (seq !== syncSeq) return;
      forge.status = status;
    }
  } catch (error) {
    if (seq !== syncSeq) return;
    forge.error = api.errorMessage(error);
    forge.phase = "ready";
    return;
  }
  forge.phase = "ready";
  await refreshForgePrs();
}

/** Fetch the open-PR state badges and publish surfaces render. Manual
 * cadence (open, ⌘R, connect); a no-op without a ready connection. */
export async function refreshForgePrs(): Promise<void> {
  if (!connectionReady()) return;
  const seq = ++prsSeq;
  forge.prsLoading = true;
  try {
    const prs = await api.forgePrs();
    if (seq !== prsSeq) return;
    forge.prs = prs;
    forge.prsError = null;
  } catch (error) {
    if (seq !== prsSeq) return;
    forge.prsError = api.errorMessage(error);
  } finally {
    if (seq === prsSeq) forge.prsLoading = false;
  }
}

/** Validate and store a pasted token (Publish's connect flow), then light
 * the PR state up. Refusals throw so the form renders them inline. */
export async function connectForge(token: string): Promise<void> {
  const status = await api.forgeLogin(token);
  forge.status = status;
  forge.error = null;
  await refreshForgePrs();
}

/** Remove Jiji's stored token. A fallback token (env/gh) may take over —
 * it gets verified so the connection stays earned — and the PR state
 * follows the answer: refreshed under a fallback, cleared without one. */
export async function disconnectForge(): Promise<void> {
  let status = await api.forgeLogout();
  forge.status = status;
  if (status.auth.source && !status.auth.login) {
    forge.phase = "verifying";
    try {
      status = await api.forgeVerify();
      forge.status = status;
    } finally {
      forge.phase = "ready";
    }
  }
  if (connectionReady()) {
    await refreshForgePrs();
  } else {
    forge.prs = null;
    forge.prsError = null;
  }
}
