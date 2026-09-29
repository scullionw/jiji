// The auto-land jobs' shared state: one *live* job at a time, but every
// job story the host knows keeps its own record — published whole by the
// backend on every change and rendered by the shell chips and the Publish
// job cards alike. The jobs run host-side — this mirrors them, so a
// webview reload reattaches to whatever the backend is doing. The backend
// also persists the records across app restarts: a surviving non-terminal
// record arrives with `live: false` — the interrupted state, offering
// resume — and dismissing clears one record at a time, host-side too.

import * as api from "$lib/api";
import type { AutoLandStatus } from "$lib/bindings/AutoLandStatus";
import type { LandOptions } from "$lib/bindings/LandOptions";
import {
  isInterrupted,
  isTerminalPhase,
} from "$lib/components/publish/autoland";

export const autoland = $state({
  /** Every job status as last published; terminal and interrupted records
   * stay until dismissed. At most one is live. */
  jobs: [] as AutoLandStatus[],
  /** A stop was requested and the definitive stopped phase is pending. */
  stopping: false,
});

function setJobs(statuses: AutoLandStatus[]) {
  autoland.jobs = statuses;
  // The stop the flag was waiting on has landed once nothing is live.
  if (!statuses.some((status) => status.live)) autoland.stopping = false;
}

/** App-lifetime subscription plus reattach — called once at bootstrap. */
export async function attachAutoLand(): Promise<void> {
  await api.onAutoLandState(setJobs);
  try {
    setJobs(await api.autolandState());
  } catch (error) {
    console.warn("failed to fetch auto-land state", error);
  }
}

/** Queue a stack — or resume an interrupted job for the same stack, which
 * is deliberately the same call (the backend seeds from the surviving
 * record). Options set the job's landing rules; omitted (a plain resume)
 * keeps the ones the record was queued with. Refusals (a job already
 * running, no token) throw so the initiating surface renders them
 * inline. */
export async function startAutoLand(
  headBookmark: string,
  options?: LandOptions,
): Promise<void> {
  setJobs(await api.autolandStart(headBookmark, options));
  autoland.stopping = false;
}

/** Ask the live job to stop. It winds down at the next safe point —
 * instantly from a wait, after the round from a round — and the stopped
 * phase arrives on the event. */
export async function stopAutoLand(): Promise<void> {
  autoland.stopping = true;
  setJobs(await api.autolandStop());
}

/** Clear one finished or interrupted job's record — from the shell and
 * from the backend's persisted list, so it stays gone across restarts. */
export async function dismissAutoLand(status: AutoLandStatus): Promise<void> {
  if (!isTerminalPhase(status.record.state.phase) && !isInterrupted(status)) {
    return;
  }
  const key = (s: AutoLandStatus) =>
    s.record.repoPath === status.record.repoPath &&
    s.record.state.headBookmark === status.record.state.headBookmark;
  autoland.jobs = autoland.jobs.filter((s) => !key(s));
  try {
    setJobs(
      await api.autolandDismiss(
        status.record.repoPath,
        status.record.state.headBookmark,
      ),
    );
  } catch (error) {
    console.warn("failed to dismiss the auto-land record", error);
  }
}
