// App-level UI state. Mutated only by `actions.ts`; components read from it.

import type { MutationOutcome } from "$lib/bindings/MutationOutcome";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";

// The breadcrumb after a mutation: what happened and which operation
// recorded it, so the status bar can deep-link into the timeline.
export interface MutationBreadcrumb {
  outcome: MutationOutcome;
  at: number;
}

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
});
