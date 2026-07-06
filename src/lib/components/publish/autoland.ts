// The auto-land job's chip and gating models: how a job state reads in
// the shell's activity chip, and when a derived land plan may be queued.
// Pure data — no Svelte. The job itself is Rust-owned (`jiji-forge`'s
// `run_autoland`); this only phrases it.

import type { AutoLandPhase } from "$lib/bindings/AutoLandPhase";
import type { AutoLandState } from "$lib/bindings/AutoLandState";
import type { LandPlan } from "$lib/bindings/LandPlan";

/** Done, failed, or stopped — the job is over and its state is a record,
 * not an activity. */
export function isTerminalPhase(phase: AutoLandPhase): boolean {
  return (
    phase.kind === "done" || phase.kind === "failed" || phase.kind === "stopped"
  );
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
  /** Terminal states grow a dismiss affordance. */
  dismissable: boolean;
}

/** The status-bar activity chip for a job state. Labels stay short — the
 * tooltip (`autolandTooltip`) carries the full story. */
export function autolandChip(state: AutoLandState): AutoLandChip {
  const name = state.headBookmark;
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
export function autolandStory(state: AutoLandState): string {
  const landed =
    state.merged.length === 0
      ? ""
      : ` Landed so far: ${state.merged.map((m) => `#${m.number}`).join(", ")}.`;
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
