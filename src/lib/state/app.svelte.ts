// App-level UI state. Mutated only by `actions.ts`; components read from it.

import type { MutationOutcome } from "$lib/bindings/MutationOutcome";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { DiffLayout } from "$lib/components/diff/diff";
import type { CompareMode } from "$lib/components/inspector/inspect";

// The breadcrumb after a mutation: what happened and which operation
// recorded it, so the status bar can deep-link into the timeline.
export interface MutationBreadcrumb {
  outcome: MutationOutcome;
  at: number;
}

// A one-shot request from the command palette to the surface that owns the
// matching UI: ChangeHeader opens its plan/confirm panels (so the palette
// never duplicates them), DiffView owns the layout, WorkbenchView the view
// mode. The owner calls `consumeIntent` after acting; intents it does not
// own it leaves alone.
export type UiIntent =
  | { kind: "describe" }
  | { kind: "bookmark" }
  | { kind: "rebase" }
  | { kind: "split" }
  | { kind: "squash" }
  | { kind: "abandon" }
  // With a mode the comparison applies directly; without one the compare
  // panel opens for picking.
  | { kind: "compare"; mode?: CompareMode }
  | { kind: "layout"; layout: DiffLayout }
  | { kind: "view"; view: "graph" | "focus" };

export type Section =
  | "workbench"
  | "conflicts"
  | "publish"
  | "operations"
  | "workspaces";

export interface RecentRepo {
  path: string;
  name: string;
  lastOpenedAt: number;
}

export const app = $state({
  snapshot: null as RepoSnapshot | null,
  section: "workbench" as Section,
  recentRepos: [] as RecentRepo[],
  opening: false,
  error: null as string | null,
  // Change id selected in the workbench graph.
  selectedNodeId: null as string | null,
  // Workstream expanded in the workbench. null falls back to the active
  // (working-copy) workstream.
  focusedWorkstreamId: null as string | null,
  // Breadcrumb for the most recent mutation; the status bar surfaces it.
  lastMutation: null as MutationBreadcrumb | null,
  // The conflict currently handed to the external merge tool. Set for the
  // whole time the tool's window is open, so every Resolve affordance (the
  // inbox card and the diff header alike) shows the same waiting state and
  // no second tool launches meanwhile.
  resolvingConflict: null as { changeId: string; path: string } | null,
  // Command palette (⌘K), the keyboard route to every action.
  paletteOpen: false,
  // Pending palette request for another surface; see UiIntent.
  intent: null as UiIntent | null,
});
