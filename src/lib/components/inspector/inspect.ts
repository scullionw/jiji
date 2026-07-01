// Pure lookups the inspector needs to place one node in context: its
// children, the workstream that owns it, and display helpers. No Svelte.

import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SyncState } from "$lib/bindings/SyncState";
import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";

export function findNode(
  snapshot: RepoSnapshot,
  id: string,
): GraphNode | undefined {
  return snapshot.nodes.find((node) => node.id === id);
}

// Children are derived rather than stored: the DTO only carries parent links.
export function childrenOf(snapshot: RepoSnapshot, id: string): GraphNode[] {
  return snapshot.nodes.filter((node) => node.parents.includes(id));
}

// Every transitive descendant within the snapshot, nearest first. Powers the
// "N descendants will be rebased" line of mutation confirm panels.
export function descendantsOf(snapshot: RepoSnapshot, id: string): GraphNode[] {
  const out: GraphNode[] = [];
  const seen = new Set([id]);
  const queue = [id];
  while (queue.length > 0) {
    for (const child of childrenOf(snapshot, queue.shift()!)) {
      if (seen.has(child.id)) continue;
      seen.add(child.id);
      out.push(child);
      queue.push(child.id);
    }
  }
  return out;
}

// Preview of the description a squash produces, mirroring the backend's
// combining: an empty side yields the other, two real descriptions
// concatenate destination (parent) first.
export function combinedDescription(
  destination: string,
  source: string,
): string {
  const dest = destination.trim();
  const src = source.trim();
  if (dest === "") return src;
  if (src === "") return dest;
  return `${dest}\n\n${src}`;
}

export interface StackPosition {
  workstream: WorkstreamSummary;
  /// 0-based position from the top of the stack.
  index: number;
}

// The workstream that claims this node, if any. Immutable bases belong to
// no workstream.
export function stackPosition(
  snapshot: RepoSnapshot,
  id: string,
): StackPosition | null {
  for (const workstream of snapshot.workstreams) {
    const index = workstream.nodeIds.indexOf(id);
    if (index !== -1) return { workstream, index };
  }
  return null;
}

// Ancestry within the drawn snapshot, following parent and elided-parent
// links up from the descendant.
export function isAncestor(
  snapshot: RepoSnapshot,
  ancestorId: string,
  descendantId: string,
): boolean {
  const queue = [descendantId];
  const seen = new Set<string>();
  while (queue.length > 0) {
    const id = queue.pop()!;
    if (id === ancestorId) return true;
    if (seen.has(id)) continue;
    seen.add(id);
    const node = findNode(snapshot, id);
    if (node) queue.push(...node.parents, ...node.elidedParents);
  }
  return false;
}

// The direction a bookmark move would take, mirroring how the backend
// summarizes it. Null when either end is outside the drawn snapshot — the
// move is still fine, there is just no direction to state.
export function moveDirection(
  snapshot: RepoSnapshot,
  fromId: string,
  toId: string,
): "forward" | "backwards" | "sideways" | null {
  if (!findNode(snapshot, fromId) || !findNode(snapshot, toId)) return null;
  if (isAncestor(snapshot, fromId, toId)) return "forward";
  if (isAncestor(snapshot, toId, fromId)) return "backwards";
  return "sideways";
}

// Candidate destinations for rebasing one change, in the snapshot's graph
// order. Excluded: the change itself; its descendants when they come along
// (a cycle — but a lone move onto a descendant is how adjacent changes swap
// order, so they stay); and the sole current parent when landing there
// would change nothing (it still extracts descendants in a lone move).
export function rebaseDestinations(
  snapshot: RepoSnapshot,
  id: string,
  withDescendants: boolean,
): GraphNode[] {
  const node = findNode(snapshot, id);
  if (!node) return [];
  const excluded = new Set([id]);
  const descendants = descendantsOf(snapshot, id);
  if (withDescendants) {
    for (const descendant of descendants) excluded.add(descendant.id);
  }
  if (
    node.parents.length === 1 &&
    (withDescendants || descendants.length === 0)
  ) {
    excluded.add(node.parents[0]);
  }
  return snapshot.nodes.filter((n) => !excluded.has(n.id));
}

// What the diff surface measures a selection against. The presets are
// relative, not pinned change ids, so walking the stack with "vs trunk"
// keeps comparing each selection against trunk.
export type CompareMode =
  | { kind: "parent" }
  | { kind: "trunk" }
  | { kind: "base" }
  | { kind: "change"; id: string };

// The change id a node's workstream grew from: the first (possibly elided)
// parent under the stack's bottom node. Null for nodes no workstream
// claims (immutable bases) and for stacks whose base is not drawn.
export function stackBaseOf(
  snapshot: RepoSnapshot,
  id: string,
): string | null {
  const workstream = stackPosition(snapshot, id)?.workstream;
  const bottom = workstream?.nodeIds[workstream.nodeIds.length - 1];
  const bottomNode = bottom ? findNode(snapshot, bottom) : undefined;
  return bottomNode?.parents[0] ?? bottomNode?.elidedParents[0] ?? null;
}

// The from-change a compare mode resolves to for one selection, or null
// when the comparison cannot apply (no trunk, no owning workstream, the
// picked change left the snapshot, or it would compare the change against
// itself) — the surface then falls back to the plain parent diff.
export function resolveCompareFrom(
  snapshot: RepoSnapshot,
  nodeId: string,
  mode: CompareMode,
): string | null {
  const usable = (id: string | null | undefined): string | null =>
    id && id !== nodeId && findNode(snapshot, id) ? id : null;
  switch (mode.kind) {
    case "parent":
      return null;
    case "trunk":
      return usable(snapshot.bookmarks.find((b) => b.isTrunk)?.target);
    case "base":
      return usable(stackBaseOf(snapshot, nodeId));
    case "change":
      return usable(mode.id);
  }
}

// The other visible commit(s) of a divergent change, drawn-snapshot only.
// Divergent nodes share `changeId` but key by commit id, so equality on the
// change id finds the sibling copies.
export function divergentSiblings(
  snapshot: RepoSnapshot,
  node: GraphNode,
): GraphNode[] {
  if (!node.isDivergent) return [];
  return snapshot.nodes.filter(
    (n) => n.changeId === node.changeId && n.id !== node.id,
  );
}

// Bookmark states pointing at this node, trunk first. Richer than
// `node.bookmarks`, which is names only.
export function bookmarksAt(
  snapshot: RepoSnapshot,
  id: string,
): BookmarkState[] {
  return snapshot.bookmarks
    .filter((bookmark) => bookmark.target === id)
    .sort((a, b) => Number(b.isTrunk) - Number(a.isTrunk));
}

export interface SplitPath {
  /// Leading directories with a trailing slash, or "" for root-level files.
  dir: string;
  name: string;
}

export function splitPath(path: string): SplitPath {
  const cut = path.lastIndexOf("/");
  if (cut === -1) return { dir: "", name: path };
  return { dir: path.slice(0, cut + 1), name: path.slice(cut + 1) };
}

export const SYNC_LABEL: Record<SyncState, { text: string; tone: string }> = {
  synced: { text: "synced", tone: "ok" },
  ahead: { text: "ahead", tone: "warn" },
  behind: { text: "behind", tone: "warn" },
  diverged: { text: "diverged", tone: "danger" },
  localOnly: { text: "local", tone: "muted" },
};
