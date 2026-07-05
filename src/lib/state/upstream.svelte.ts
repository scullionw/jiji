// The background upstream check: `jj git fetch` on a quiet cadence plus a
// manual fetch-now, so "the remote moved under you" surfaces while the app
// sits open — sync glyphs, behind-trunk counts, and conflicted bookmarks
// all read the refreshed remote state, and the PR-badge refresh rides the
// same rhythm (its cadence was deliberately manual until this existed).
//
// GitButler's lesson (see the inspiration notes): upstream state lives in
// the shell, always visible, never noisy. The shell chip renders this
// state; background ticks stay silent (no breadcrumb — the op timeline
// records real imports), while an explicit Fetch now gets the standard
// mutation breadcrumb.

import * as api from "$lib/api";
import { app } from "./app.svelte";
import { refreshForgePrs } from "./forge.svelte";

/** Background checks run this far apart. Manual fetches reset the clock. */
export const UPSTREAM_CHECK_INTERVAL_MS = 15 * 60_000;
/** The heartbeat that drives due-checks and ages the chip label. */
const HEARTBEAT_MS = 60_000;

export const upstream = $state({
  checking: false,
  /** When the last *successful* check completed; null before the first. */
  lastChecked: null as number | null,
  /** The last check's failure, cleared by the next success. */
  error: null as string | null,
  /** Heartbeat-updated clock the chip ages its label against. */
  now: Date.now(),
});

let lastRepoPath: string | null = null;
/** When a check last started (success or not) — the cadence gate, kept
 * separate from `lastChecked` so an unreachable remote retries on the
 * interval instead of every heartbeat. */
let lastAttempt = 0;
let checkSeq = 0;

/** Follow the open repo: a different repo (or none) resets the check
 * state, and a repo with remotes gets its first check right away. Called
 * from the shell's snapshot effect, keyed on the repo path so ordinary
 * snapshot refreshes no-op. */
export function syncUpstreamRepo(
  path: string | null,
  hasRemotes: boolean,
): void {
  if (path === lastRepoPath) return;
  lastRepoPath = path;
  checkSeq += 1; // orphan any in-flight check for the previous repo
  upstream.checking = false;
  upstream.lastChecked = null;
  upstream.error = null;
  lastAttempt = 0;
  if (path !== null && hasRemotes) void checkUpstream();
}

/** One upstream check: fetch from the default remotes, then refresh the
 * PR state on the same rhythm. `breadcrumb` is the manual Fetch-now
 * flavor — the recorded operation lands in the status bar like any other
 * mutation (background ticks stay silent). */
export async function checkUpstream(
  options: { breadcrumb?: boolean } = {},
): Promise<void> {
  if (!app.snapshot || app.snapshot.gitRemotes.length === 0) return;
  if (upstream.checking) return;
  const seq = ++checkSeq;
  upstream.checking = true;
  lastAttempt = Date.now();
  try {
    const outcome = await api.gitFetch();
    if (seq !== checkSeq) return;
    // The refreshed snapshot also arrives via the snapshot event; pulling
    // it here keeps the check self-contained (same shape as runMutation).
    const snapshot = await api.currentSnapshot();
    if (seq !== checkSeq) return;
    if (snapshot) app.snapshot = snapshot;
    upstream.error = null;
    upstream.lastChecked = Date.now();
    upstream.now = upstream.lastChecked;
    if (options.breadcrumb && outcome.operationId) {
      app.lastMutation = { outcome, at: Date.now() };
    }
  } catch (error) {
    if (seq !== checkSeq) return;
    upstream.error = api.errorMessage(error);
  } finally {
    if (seq === checkSeq) upstream.checking = false;
  }
  void refreshForgePrs();
}

/** The shell chip's click and the palette command. */
export function fetchUpstreamNow(): Promise<void> {
  return checkUpstream({ breadcrumb: true });
}

/** Start the background cadence; returns the stopper for unmount. A
 * one-minute heartbeat ages the chip label and starts a check whenever
 * the last attempt is a full interval old — so a repo opened five minutes
 * ago is not re-fetched by the first tick, and an offline stretch retries
 * on the interval, not the heartbeat. */
export function startUpstreamChecks(): () => void {
  const timer = setInterval(() => {
    upstream.now = Date.now();
    if (!app.snapshot || app.snapshot.gitRemotes.length === 0) return;
    if (upstream.checking) return;
    if (Date.now() - lastAttempt < UPSTREAM_CHECK_INTERVAL_MS) return;
    void checkUpstream();
  }, HEARTBEAT_MS);
  return () => clearInterval(timer);
}
