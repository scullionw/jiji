// The auto-land job's shared state: one job at a time, published by the
// backend on every phase change and rendered by the shell chip and the
// Publish job card alike. The job runs host-side — this mirrors it, so a
// webview reload reattaches to whatever the backend is doing. The backend
// also persists the record across app restarts: a surviving non-terminal
// record arrives with `live: false` — the interrupted state, offering
// resume — and dismissing clears it host-side too.

import * as api from "$lib/api";
import type { AutoLandStatus } from "$lib/bindings/AutoLandStatus";
import {
  isInterrupted,
  isTerminalPhase,
} from "$lib/components/publish/autoland";

export const autoland = $state({
  /** The job as last published; terminal and interrupted records stay
   * until dismissed. */
  job: null as AutoLandStatus | null,
  /** A stop was requested and the definitive stopped phase is pending. */
  stopping: false,
});

/** App-lifetime subscription plus reattach — called once at bootstrap. */
export async function attachAutoLand(): Promise<void> {
  await api.onAutoLandState((status) => {
    autoland.job = status;
    if (isTerminalPhase(status.record.state.phase)) autoland.stopping = false;
  });
  try {
    const current = await api.autolandState();
    if (current) autoland.job = current;
  } catch (error) {
    console.warn("failed to fetch auto-land state", error);
  }
}

/** Queue a stack — or resume an interrupted job for the same stack, which
 * is deliberately the same call (the backend seeds from the surviving
 * record). Refusals (a job already running, no token) throw so the
 * initiating surface renders them inline. */
export async function startAutoLand(headBookmark: string): Promise<void> {
  const status = await api.autolandStart(headBookmark);
  autoland.job = status;
  autoland.stopping = false;
}

/** Ask the job to stop. It winds down at the next safe point — instantly
 * from a wait, after the round from a round — and the stopped phase
 * arrives on the event. */
export async function stopAutoLand(): Promise<void> {
  autoland.stopping = true;
  const status = await api.autolandStop();
  if (status) autoland.job = status;
}

/** Clear a finished or interrupted job's record — from the shell and from
 * the backend's persisted slot, so it stays gone across restarts. */
export async function dismissAutoLand(): Promise<void> {
  const job = autoland.job;
  if (!job) return;
  if (!isTerminalPhase(job.record.state.phase) && !isInterrupted(job)) return;
  autoland.job = null;
  try {
    await api.autolandDismiss();
  } catch (error) {
    console.warn("failed to dismiss the auto-land record", error);
  }
}
