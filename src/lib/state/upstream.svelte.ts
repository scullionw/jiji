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
//
// Each repo keeps its own check memory, so tab switches restore where a
// repo's cadence left off instead of refetching on every switch.

import * as api from "$lib/api";
import { applySnapshot } from "./actions";
import { app } from "./app.svelte";
import { refreshForgePrs } from "./forge.svelte";

/** The heartbeat that drives due-checks and ages the chip label. */
const HEARTBEAT_MS = 60_000;

/** Cadence choices Settings offers; 0 turns background checks off (the
 * chip's manual Fetch now stays). */
export const UPSTREAM_INTERVALS = [
  { minutes: 5, label: "5m" },
  { minutes: 15, label: "15m" },
  { minutes: 30, label: "30m" },
  { minutes: 0, label: "Off" },
] as const;

const DEFAULT_INTERVAL_MINUTES = 15;
const INTERVAL_KEY = "jiji.upstream.interval";

function readInterval(): number {
  const raw = localStorage.getItem(INTERVAL_KEY);
  if (raw === null) return DEFAULT_INTERVAL_MINUTES;
  const minutes = Number(raw);
  return UPSTREAM_INTERVALS.some((i) => i.minutes === minutes)
    ? minutes
    : DEFAULT_INTERVAL_MINUTES;
}

export const upstream = $state({
  checking: false,
  /** When the last *successful* check completed; null before the first. */
  lastChecked: null as number | null,
  /** The last check's failure, cleared by the next success. */
  error: null as string | null,
  /** Heartbeat-updated clock the chip ages its label against. */
  now: Date.now(),
  /** Background checks run this far apart (minutes); 0 is off. Manual
   * fetches reset the clock either way. */
  intervalMinutes: readInterval(),
});

export function setUpstreamInterval(minutes: number): void {
  upstream.intervalMinutes = minutes;
  localStorage.setItem(INTERVAL_KEY, String(minutes));
}

/** One repo's check memory. `lastAttempt` is the cadence gate (when a
 * check last *started*, success or not), kept separate from `lastChecked`
 * so an unreachable remote retries on the interval instead of every
 * heartbeat. */
interface UpstreamRecord {
  lastChecked: number | null;
  error: string | null;
  lastAttempt: number;
}

const records = new Map<string, UpstreamRecord>();

function recordFor(path: string): UpstreamRecord {
  let record = records.get(path);
  if (!record) {
    record = { lastChecked: null, error: null, lastAttempt: 0 };
    records.set(path, record);
  }
  return record;
}

let lastRepoPath: string | null = null;
let checkSeq = 0;

/** Follow the repo on screen: a different repo (or none) swaps in its own
 * check memory, and a repo never checked before gets its first check
 * right away. Called from the shell's snapshot effect, keyed on the repo
 * path so ordinary snapshot refreshes no-op. */
export function syncUpstreamRepo(
  path: string | null,
  hasRemotes: boolean,
): void {
  if (path === lastRepoPath) return;
  lastRepoPath = path;
  checkSeq += 1; // orphan any in-flight check's shell-state writes
  const record = path === null ? null : recordFor(path);
  upstream.checking = false;
  upstream.lastChecked = record?.lastChecked ?? null;
  upstream.error = record?.error ?? null;
  // Off means no unrequested fetches at all — not even the open-check.
  if (
    path !== null &&
    hasRemotes &&
    upstream.intervalMinutes !== 0 &&
    record?.lastAttempt === 0
  ) {
    void checkUpstream();
  }
}

/** One upstream check: fetch from the default remotes, then refresh the
 * PR state on the same rhythm. `breadcrumb` is the manual Fetch-now
 * flavor — the recorded operation lands in the status bar like any other
 * mutation (background ticks stay silent). Results always land on the
 * repo's own record; the shell chip only follows while that repo still
 * owns the screen. */
export async function checkUpstream(
  options: { breadcrumb?: boolean } = {},
): Promise<void> {
  const path = app.snapshot?.repoPath ?? null;
  if (!path || app.snapshot?.gitRemotes.length === 0) return;
  if (upstream.checking) return;
  const seq = ++checkSeq;
  const record = recordFor(path);
  upstream.checking = true;
  record.lastAttempt = Date.now();
  try {
    const outcome = await api.gitFetch();
    // The refreshed snapshot also arrives via the snapshot event; pulling
    // it here keeps the check self-contained (same shape as runMutation).
    const snapshot = await api.currentSnapshot(path);
    if (snapshot) applySnapshot(snapshot);
    record.error = null;
    record.lastChecked = Date.now();
    if (seq === checkSeq) {
      upstream.error = null;
      upstream.lastChecked = record.lastChecked;
      upstream.now = record.lastChecked;
      if (options.breadcrumb && outcome.operationId) {
        app.lastMutation = { outcome, at: Date.now() };
      }
    }
  } catch (error) {
    record.error = api.errorMessage(error);
    if (seq === checkSeq) upstream.error = record.error;
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
 * the on-screen repo's last attempt is a full interval old — so a repo
 * opened (or switched to) five minutes ago is not re-fetched by the first
 * tick, and an offline stretch retries on the interval, not the
 * heartbeat. */
export function startUpstreamChecks(): () => void {
  const timer = setInterval(() => {
    upstream.now = Date.now();
    const path = app.snapshot?.repoPath;
    if (!path || app.snapshot?.gitRemotes.length === 0) return;
    if (upstream.checking) return;
    if (upstream.intervalMinutes === 0) return;
    const record = recordFor(path);
    if (Date.now() - record.lastAttempt < upstream.intervalMinutes * 60_000) {
      return;
    }
    void checkUpstream();
  }, HEARTBEAT_MS);
  return () => clearInterval(timer);
}
