// The auto-land job's chip and gating models: how a job status reads in
// the shell's activity chip, and when a derived land plan may be queued.
// Pure data — no Svelte. The job itself is Rust-owned (`jiji-forge`'s
// `run_autoland`); this only phrases it — including the record that
// survived a restart, which arrives `live: false` and reads as
// interrupted until resumed or dismissed.

import type { AutoLandPhase } from "$lib/bindings/AutoLandPhase";
import type { AutoLandStatus } from "$lib/bindings/AutoLandStatus";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { LandOptions } from "$lib/bindings/LandOptions";
import type { LandPlan } from "$lib/bindings/LandPlan";

/** The persisted rules a job will use on every re-plan. This is rendered
 * beside resume controls so a surviving admin bypass is never implicit. */
export function autolandOptionsStory(options: LandOptions): string {
  const method =
    options.method === null
      ? "automatic merge method"
      : `${options.method} merge method`;
  return options.bypass
    ? `Saved rules: ${method}. Admin bypass is on; this job attempts direct merges past checks, reviews, and merge queues.`
    : `Saved rules: ${method}. Checks, reviews, and merge queues remain enforced.`;
}

/** Done, failed, or stopped — the job is over and its state is a record,
 * not an activity. */
export function isTerminalPhase(phase: AutoLandPhase): boolean {
  return (
    phase.kind === "done" || phase.kind === "failed" || phase.kind === "stopped"
  );
}

/** A non-terminal record no thread is driving: the job was watching or
 * mid-round when the app closed (or its thread died). Resumable — the
 * record carries everything the story needs. */
export function isInterrupted(status: AutoLandStatus): boolean {
  return !status.live && !isTerminalPhase(status.record.state.phase);
}

/** A job record belongs to the repo it was queued in — it renders only
 * while that repo is open, so a restart into another project does not
 * wear a chip about somewhere else. */
export function autolandVisible(
  status: AutoLandStatus,
  repoPath: string | null | undefined,
): boolean {
  return repoPath != null && status.record.repoPath === repoPath;
}

/** The open repo's job records in render order: the live job first, then
 * interrupted records (they want a decision), then terminal ones. Ties
 * keep the host's stable record order. */
export function visibleAutolandJobs(
  jobs: AutoLandStatus[],
  repoPath: string | null | undefined,
): AutoLandStatus[] {
  const rank = (status: AutoLandStatus) =>
    status.live ? 0 : isInterrupted(status) ? 1 : 2;
  return jobs
    .filter((status) => autolandVisible(status, repoPath))
    .map((status, index) => ({ status, index }))
    .sort((a, b) => rank(a.status) - rank(b.status) || a.index - b.index)
    .map(({ status }) => status);
}

/** The one record a thread is driving right now, whichever repo it
 * belongs to — queueing is gated on it repo-wide, since the host runs one
 * live job at a time. */
export function liveAutolandJob(jobs: AutoLandStatus[]): AutoLandStatus | null {
  return jobs.find((status) => status.live) ?? null;
}

/** Why an interrupted job cannot resume — the head bookmark is gone from
 * the snapshot — or null when it can. The engine would park with the same
 * story on its first poll; saying it up front beats offering a doomed
 * button. */
export function resumeBlocker(
  status: AutoLandStatus,
  bookmarks: BookmarkState[],
): string | null {
  const name = status.record.state.headBookmark;
  return bookmarks.some((b) => b.name === name && b.isLocal)
    ? null
    : `“${name}” is no longer a bookmark here — the stack moved or was cleaned up while the job was away`;
}

/** Whether a derived plan may be queued for auto-land: there is either
 * work to run right now, or only conditions waiting clears by itself
 * (transient blockers — checks running, a review pending). Needs-user
 * blockers refuse, the same way Land does: fix what the plan names, then
 * queue. A plan that is all hand-off supervision (no actions, no
 * blockers) queues fine — watching GitHub finish is the job's purpose. */
export function canQueueAutoLand(plan: LandPlan): boolean {
  return plan.actions.length > 0 || plan.blockers.every((b) => b.wait);
}

export interface AutoLandChip {
  label: string;
  tone: "accent" | "ok" | "warn" | "danger" | "muted";
  /** The chip pulses while the job is actively working or watching. */
  pulse: boolean;
  /** Terminal and interrupted states grow a dismiss affordance. */
  dismissable: boolean;
}

/** The status-bar activity chip for a job status. Labels stay short — the
 * tooltip (`autolandTooltip`) carries the full story. */
export function autolandChip(status: AutoLandStatus): AutoLandChip {
  const state = status.record.state;
  const name = state.headBookmark;
  if (isInterrupted(status)) {
    // Warn-toned: the user may believe the job is still watching, and it
    // is not — resuming (or dismissing) is a decision only they can make.
    return {
      label: `Auto-land ${name}: interrupted`,
      tone: "warn",
      pulse: false,
      dismissable: true,
    };
  }
  switch (state.phase.kind) {
    case "waiting":
      return state.phase.attention
        ? {
            label: `Auto-land ${name}: needs you`,
            tone: "warn",
            pulse: false,
            dismissable: false,
          }
        : {
            label: `Auto-land ${name}: watching`,
            tone: "accent",
            pulse: true,
            dismissable: false,
          };
    case "round":
      return {
        label: `Auto-land ${name}: landing…`,
        tone: "accent",
        pulse: true,
        dismissable: false,
      };
    case "done":
      return {
        label: `Auto-land ${name}: landed`,
        tone: "ok",
        pulse: false,
        dismissable: true,
      };
    case "failed":
      return {
        label: `Auto-land ${name}: gave up`,
        tone: "danger",
        pulse: false,
        dismissable: true,
      };
    case "stopped":
      return {
        label: `Auto-land ${name}: stopped`,
        tone: "muted",
        pulse: false,
        dismissable: true,
      };
  }
}

/** The full story behind the chip, for its tooltip and the job card's
 * phase line. */
export function autolandStory(status: AutoLandStatus): string {
  const state = status.record.state;
  const landed =
    state.merged.length === 0
      ? ""
      : ` Landed so far: ${state.merged.map((m) => `#${m.number}`).join(", ")}.`;
  if (isInterrupted(status)) {
    const doing = state.phase.kind === "round" ? "mid-round" : "watching";
    return `This job was interrupted while ${doing} — the app closed or the repo was switched away. Resume it from Publish; a fresh check picks up where it left off.${landed}`;
  }
  switch (state.phase.kind) {
    case "waiting":
      return `${state.phase.reasons.join("; ")}.${landed}`;
    case "round":
      return `Running a landing round now.${landed}`;
    case "done": {
      const prs = state.merged.map((m) => `#${m.number}`).join(", ");
      const rounds = `${state.rounds} round${state.rounds === 1 ? "" : "s"}`;
      return state.merged.length > 0
        ? `The whole stack landed: ${prs} in ${rounds}.`
        : `The whole stack landed in ${rounds}.`;
    }
    case "failed":
      return `${state.phase.message}.${landed}`;
    case "stopped":
      return `Stopped on request.${landed}`;
  }
}
