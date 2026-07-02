// Pure shaping for the conflict inbox: which group each item belongs to,
// what order groups and items render in, and the plain-language framing
// each group gets. Rendering stays in the Svelte components.

import type { ConflictItem } from "$lib/bindings/ConflictItem";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";

export type ConflictGroupKey =
  | "working-copy"
  | "commits"
  | "bookmarks"
  | "workspaces";

export interface ConflictGroup {
  key: ConflictGroupKey;
  title: string;
  /// One-paragraph plain-language explanation of what this kind of
  /// conflict is and what resolving it looks like.
  blurb: string;
  items: ConflictItem[];
}

const GROUP_COPY: Record<ConflictGroupKey, { title: string; blurb: string }> = {
  "working-copy": {
    title: "In your working copy",
    blurb:
      "These conflicts are written into the files on disk right now as marker lines. Edit the files to resolve them — or keep working elsewhere; the conflict stays safely recorded either way.",
  },
  commits: {
    title: "Recorded in commits",
    blurb:
      "A rewrite didn't apply cleanly, so jj recorded the conflict inside the commit instead of stopping halfway. Nothing is blocked — open a change to see the conflicted content, and edit it to resolve.",
  },
  bookmarks: {
    title: "Conflicted bookmarks",
    blurb:
      "The bookmark was moved to different targets by operations that don't agree — usually concurrent commands or a fetch racing local work. Point it at the right change to resolve.",
  },
  workspaces: {
    title: "Stale workspaces",
    blurb:
      "The repository moved since this workspace's working copy was last updated. Update the workspace before working in it.",
  },
};

function groupKeyOf(item: ConflictItem, workingCopy: string): ConflictGroupKey {
  switch (item.kind) {
    case "file":
      return item.nodeId === workingCopy ? "working-copy" : "commits";
    case "bookmark":
      return "bookmarks";
    case "staleWorkspace":
      return "workspaces";
  }
}

// Fixed group order: what is on disk right now first, then recorded
// conflicts in graph order, then refs, then workspaces. Empty groups are
// omitted.
export function groupConflicts(snapshot: RepoSnapshot): ConflictGroup[] {
  const order: ConflictGroupKey[] = [
    "working-copy",
    "commits",
    "bookmarks",
    "workspaces",
  ];
  const buckets = new Map<ConflictGroupKey, ConflictItem[]>();
  for (const item of snapshot.conflicts) {
    const key = groupKeyOf(item, snapshot.workingCopy);
    const bucket = buckets.get(key);
    if (bucket) bucket.push(item);
    else buckets.set(key, [item]);
  }

  // Commit conflicts read top-down like the graph they point into; items
  // whose change is not drawn sort last, keeping their backend order.
  const commits = buckets.get("commits");
  if (commits) {
    const rowIndex = new Map(snapshot.nodes.map((n, i) => [n.id, i]));
    const indexOf = (item: ConflictItem) =>
      item.nodeId != null ? (rowIndex.get(item.nodeId) ?? Infinity) : Infinity;
    commits.sort((a, b) => indexOf(a) - indexOf(b));
  }

  return order
    .filter((key) => buckets.has(key))
    .map((key) => ({ key, ...GROUP_COPY[key], items: buckets.get(key)! }));
}

// Display names for the merge tools whose configs ship embedded (plus the
// obvious spellings users configure themselves); anything unknown shows its
// configured name verbatim, which is what the user typed into their config.
const TOOL_LABELS: Record<string, string> = {
  smerge: "Sublime Merge",
  meld: "Meld",
  kdiff3: "KDiff3",
  mergiraf: "Mergiraf",
  vimdiff: "Vim",
  vscode: "VS Code",
  vscodium: "VSCodium",
};

export function mergeToolLabel(tool: string): string {
  return TOOL_LABELS[tool] ?? tool;
}

// Whether a stale-workspace item gets the guided recovery action: only the
// current workspace's working copy is reachable from here (a sibling
// workspace keeps its state in its own root), so only its item can offer
// "Update workspace".
export function canRecoverWorkspace(
  snapshot: RepoSnapshot,
  item: ConflictItem,
): boolean {
  if (item.kind !== "staleWorkspace" || item.workspace == null) return false;
  const current = snapshot.workspaces.find((w) => w.isCurrent);
  return current !== undefined && current.name === item.workspace;
}

// Whether a Resolve affordance should render for a conflicted file in this
// change: a usable merge tool exists and the change is drawn and mutable
// (resolving rewrites the change, so immutable conflicts get no button —
// same rule the actions row applies to rewrites).
export function canResolve(
  snapshot: RepoSnapshot,
  nodeId: string | null,
): boolean {
  if (!snapshot.resolveTool || !nodeId) return false;
  const node = snapshot.nodes.find((n) => n.id === nodeId);
  return node !== undefined && node.kind !== "immutable";
}
