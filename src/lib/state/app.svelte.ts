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
// mode, PublishView the land plan card. The owner calls `consumeIntent`
// after acting; intents it does not own it leaves alone.
export type UiIntent =
  | { kind: "describe" }
  | { kind: "bookmark" }
  | { kind: "rebase" }
  // `into` opens the split panel in its move-into-an-existing-change mode;
  // `path` preselects one file (the diff pane's "move this file" route).
  | { kind: "split"; into?: boolean; path?: string }
  | { kind: "squash" }
  | { kind: "abandon" }
  // With a mode the comparison applies directly; without one the compare
  // panel opens for picking.
  | { kind: "compare"; mode?: CompareMode }
  // Opens the change's evolution pane (the version timeline + restore).
  | { kind: "evolog" }
  | { kind: "layout"; layout: DiffLayout }
  // Toggles the diff pane's file tree.
  | { kind: "filetree" }
  // Opens the diff pane's find bar (⌘F).
  | { kind: "find" }
  | { kind: "view"; view: "graph" | "focus" }
  // Opens the stack's land plan card in Publish — the consequence-stating
  // panel that owns both Land and Auto-land-when-ready.
  | { kind: "land"; bookmark: string }
  // Opens the stack's ship-to-trunk plan card in Publish (`head` is the
  // stack's head change id — shipping needs no bookmark).
  | { kind: "ship"; head: string };

export type Section =
  | "workbench"
  | "conflicts"
  | "publish"
  | "bookmarks"
  | "operations"
  | "workspaces";

export interface RecentRepo {
  path: string;
  name: string;
  lastOpenedAt: number;
}

// The section rail can step aside entirely (⌘B); the choice sticks across
// sessions. actions.ts writes the key on toggle. (A new key: the old
// "collapsed" preference meant icons-only, which the rail always is now.)
export const SIDEBAR_KEY = "jiji.rail.hidden";

// The workbench's graph pane collapses the same way (⌘⇧B pairs with ⌘B):
// both left bars out of the way leaves the diff the whole window.
export const GRAPH_KEY = "jiji.graph.collapsed";

// A class rather than one `$state({...})` object so each field picks its
// own rune: snapshots are replaced wholesale and never edited in place, so
// `snapshot` stays raw. A deep proxy would wrap every node, bookmark and
// operation on first read, and every derived view that walks the graph
// would pay that again on each refresh, an order of magnitude slower.
class AppState {
  snapshot = $state.raw<RepoSnapshot | null>(null);
  section = $state<Section>("workbench");
  sidebarCollapsed = $state(localStorage.getItem(SIDEBAR_KEY) === "1");
  graphCollapsed = $state(localStorage.getItem(GRAPH_KEY) === "1");
  recentRepos = $state<RecentRepo[]>([]);
  opening = $state(false);
  error = $state<string | null>(null);
  // Change id selected in the workbench graph.
  selectedNodeId = $state<string | null>(null);
  // ⇧-click multi-selection: a contiguous run of one workstream, head-first,
  // containing selectedNodeId (the anchor). The diff pane renders it as one
  // combined diff. Transient reading state — anything that moves the
  // selection or reshapes the lane clears it (see selection.ts).
  selectedRange = $state<string[] | null>(null);
  // Workstream expanded in the workbench. null falls back to the active
  // (working-copy) workstream.
  focusedWorkstreamId = $state<string | null>(null);
  // Breadcrumb for the most recent mutation; the status bar surfaces it.
  lastMutation = $state<MutationBreadcrumb | null>(null);
  // The conflict currently handed to the external merge tool. Set for the
  // whole time the tool's window is open, so every Resolve affordance (the
  // inbox card and the diff header alike) shows the same waiting state and
  // no second tool launches meanwhile.
  resolvingConflict = $state<{ changeId: string; path: string } | null>(null);
  // Command palette (⌘K), the keyboard route to every action.
  paletteOpen = $state(false);
  // History search overlay (⌘⇧F): find a string or filename across
  // changes. `searchSeed` prefills its input when opened with a query
  // already in hand (the palette's dynamic row).
  searchOpen = $state(false);
  searchSeed = $state("");
  // Pending palette request for another surface; see UiIntent.
  intent = $state<UiIntent | null>(null);
  // The annotate pane (`jj file annotate`): which change's tree and file
  // are being read line-by-line. Transient reading state like a ⇧-click
  // range — moving the selection (including a line-click jump) closes it.
  annotate = $state<{ changeId: string; path: string } | null>(null);
  // The evolution pane (`jj evolog`): which change's rewrite history is
  // on the diff surface as a scrubbable timeline. Same transient reading
  // state as annotate — the two share the surface, so opening either
  // closes the other, and any selection move closes both.
  evolution = $state<{ changeId: string } | null>(null);
}

export const app = new AppState();
