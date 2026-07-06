// The auto-land job's shared state: one job at a time, published by the
// backend on every phase change and rendered by the shell chip and the
// Publish job card alike. The job runs host-side — this mirrors it, so a
// webview reload reattaches to whatever the backend is doing.

import * as api from "$lib/api";
import type { AutoLandState } from "$lib/bindings/AutoLandState";
import { isTerminalPhase } from "$lib/components/publish/autoland";

export const autoland = $state({
  /** The job as last published; terminal states stay until dismissed. */
  job: null as AutoLandState | null,
  /** A stop was requested and the definitive stopped phase is pending. */
  stopping: false,
});

/** App-lifetime subscription plus reattach — called once at bootstrap. */
export async function attachAutoLand(): Promise<void> {
  await api.onAutoLandState((state) => {
    autoland.job = state;
    if (isTerminalPhase(state.phase)) autoland.stopping = false;
  });
  try {
    const current = await api.autolandState();
    if (current) autoland.job = current;
  } catch (error) {
    console.warn("failed to fetch auto-land state", error);
  }
}

/** Queue a stack. Refusals (a job already running, no token) throw so the
 * initiating surface renders them inline. */
export async function startAutoLand(headBookmark: string): Promise<void> {
  const state = await api.autolandStart(headBookmark);
  autoland.job = state;
  autoland.stopping = false;
}

/** Ask the job to stop. It winds down at the next safe point — instantly
 * from a wait, after the round from a round — and the stopped phase
 * arrives on the event. */
export async function stopAutoLand(): Promise<void> {
  autoland.stopping = true;
  const state = await api.autolandStop();
  if (state) autoland.job = state;
}

/** Clear a finished job's record from the shell. */
export function dismissAutoLand(): void {
  if (autoland.job && isTerminalPhase(autoland.job.phase)) {
    autoland.job = null;
  }
}
