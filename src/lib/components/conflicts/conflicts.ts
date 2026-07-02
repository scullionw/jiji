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
