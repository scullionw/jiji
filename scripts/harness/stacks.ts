// Browser-only IPC twin for visual scenarios; production discovery is Rust-owned.
import type { GraphNode } from "$lib/bindings/GraphNode";
import type { PrSummary } from "$lib/bindings/PrSummary";
import type { RepoPrState } from "$lib/bindings/RepoPrState";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SubmitHead } from "$lib/bindings/SubmitHead";
import {
  publishableStacks,
  stackHead,
  stackKey,
} from "../../src/lib/components/publish/submit";

export interface StackSegment {
  bookmark: string | null;
  title: string;
  base: string;
  changes: GraphNode[];
  pr: PrSummary | null;
}

export interface PublishStack {
  key: string;
  title: string;
  headBookmark: string | null;
  headChange: string | null;
  submitHead: SubmitHead | null;
  isActive: boolean;
  /** Bottom first, in landing order. PR identity comes from branch names,
   * so stacks created by jjpr need no Jiji-specific metadata. */
  segments: StackSegment[];
  base: string;
  changeCount: number;
  prCount: number;
  searchText: string;
}

/** Walk actual ancestry, including shared workstream ancestors. A graph
 * lane is a layout choice; it is not a PR stack boundary. */
function chainOf(snapshot: RepoSnapshot, head: string): GraphNode[] {
  const nodes = new Map(snapshot.nodes.map((node) => [node.id, node]));
  const seen = new Set<string>();
  const chain: GraphNode[] = [];
  let node = nodes.get(head);
  while (node && node.kind !== "immutable" && !seen.has(node.id)) {
    seen.add(node.id);
    chain.push(node);
    node = nodes.get(node.parents[0]);
  }
  return chain.reverse();
}

function finish(
  stack: Omit<PublishStack, "prCount" | "searchText">,
): PublishStack {
  return {
    ...stack,
    prCount: stack.segments.filter((segment) => segment.pr != null).length,
    searchText: [
      stack.title,
      stack.headBookmark,
      ...stack.segments.flatMap((segment) => [
        segment.bookmark,
        segment.title,
        segment.pr ? `#${segment.pr.number}` : "",
      ]),
    ]
      .join(" ")
      .toLowerCase(),
  };
}

export function discoverStacks(
  snapshot: RepoSnapshot,
  prs: RepoPrState | null,
): PublishStack[] {
  const byBranch = prs?.byBranch ?? {};
  const bookmarks = snapshot.bookmarks.filter(
    (bookmark) => bookmark.isLocal && !bookmark.isTrunk,
  );
  const names = new Set(bookmarks.map((bookmark) => bookmark.name));
  const result: PublishStack[] = [];
  const seen = new Set<string>();
  for (const candidate of publishableStacks(
    snapshot,
    new Set(Object.keys(byBranch)),
  )) {
    const key = stackKey(candidate);
    if (seen.has(key)) continue;
    seen.add(key);
    const headChange =
      candidate.headBookmark != null
        ? bookmarks.find((bookmark) => bookmark.name === candidate.headBookmark)
            ?.target
        : candidate.headChange;
    if (!headChange) continue;
    const chain = chainOf(snapshot, headChange);
    if (!chain.length) continue;
    const segments: StackSegment[] = [];
    let pending: GraphNode[] = [];
    let base = snapshot.trunkBookmark;
    for (const node of chain) {
      pending.push(node);
      const nodeNames = node.bookmarks
        .filter((name) => names.has(name))
        .sort(
          (a, b) =>
            Number(b === candidate.headBookmark) -
              Number(a === candidate.headBookmark) ||
            Number(Boolean(byBranch[b])) - Number(Boolean(byBranch[a])) ||
            a.localeCompare(b),
        );
      const bookmark = nodeNames[0];
      if (!bookmark && node !== chain.at(-1)) continue;
      const pr = bookmark ? (byBranch[bookmark] ?? null) : null;
      segments.push({
        bookmark: bookmark ?? null,
        title:
          pr?.title ??
          (pending[0].description.split("\n")[0] || "Untitled change"),
        base,
        changes: pending,
        pr,
      });
      pending = [];
      if (bookmark) base = bookmark;
    }
    // Undescribed empty scratch changes have no work to publish.
    if (
      !candidate.headBookmark &&
      chain.every((node) => node.isEmpty && !node.description.trim())
    )
      continue;
    const top = segments.at(-1)!;
    result.push(
      finish({
        key,
        title: top.title,
        headBookmark: candidate.headBookmark ?? null,
        headChange,
        submitHead: stackHead(candidate),
        isActive: candidate.isActive,
        segments,
        base: segments[0].pr?.baseBranch ?? snapshot.trunkBookmark,
        changeCount: chain.length,
      }),
    );
  }

  // Also reveal published stacks absent locally. Follow GitHub's base
  // links, never branch prefixes or commit SHAs (rewrites change those).
  // byBranch is fork-filtered by Rust; a fork's same-named branch cannot
  // become part of a local stack.
  const represented = new Set(
    result.flatMap((stack) =>
      stack.segments.map((segment) => segment.bookmark),
    ),
  );
  const parents = new Set(Object.values(byBranch).map((pr) => pr?.baseBranch));
  for (const pr of Object.values(byBranch)) {
    if (!pr || parents.has(pr.headBranch) || represented.has(pr.headBranch))
      continue;
    const segments: StackSegment[] = [];
    const visited = new Set<string>();
    let current: PrSummary | undefined = pr;
    while (current && !visited.has(current.headBranch)) {
      visited.add(current.headBranch);
      segments.unshift({
        bookmark: current.headBranch,
        title: current.title,
        base: current.baseBranch,
        changes: [],
        pr: current,
      });
      current = byBranch[current.baseBranch];
    }
    result.push(
      finish({
        key: `remote:${pr.headBranch}`,
        title: pr.title,
        headBookmark: pr.headBranch,
        headChange: null,
        submitHead: null,
        isActive: false,
        segments,
        base: segments[0].base,
        changeCount: 0,
      }),
    );
  }
  return result.sort(
    (a, b) =>
      Number(b.isActive) - Number(a.isActive) ||
      Number(b.prCount > 0) - Number(a.prCount > 0),
  );
}

export function stackSummary(stack: PublishStack): {
  label: string;
  tone: string;
} {
  if (
    stack.segments.some((segment) =>
      segment.changes.some((change) => change.hasConflict),
    )
  )
    return { label: "Conflicts", tone: "danger" };
  const prs = stack.segments.flatMap((segment) =>
    segment.pr ? [segment.pr] : [],
  );
  if (!prs.length) return { label: "Local", tone: "muted" };
  if (
    prs.some(
      (pr) => pr.checks === "failing" || pr.review === "changesRequested",
    )
  )
    return { label: "Needs attention", tone: "danger" };
  if (prs.some((pr) => pr.isDraft)) return { label: "Draft", tone: "muted" };
  if (prs.some((pr) => pr.checks === "pending"))
    return { label: "Checks running", tone: "warn" };
  if (prs.some((pr) => pr.review === "reviewRequired"))
    return { label: "In review", tone: "warn" };
  return { label: "Published", tone: "ok" };
}

Object.assign(window, {
  __HARNESS_DISCOVER_STACKS__: (
    snapshot: RepoSnapshot,
    prs: RepoPrState | null,
  ) =>
    discoverStacks(snapshot, prs).map((stack) => ({
      ...stack,
      status: (
        {
          Local: "local",
          Conflicts: "conflicts",
          "Needs attention": "needsAttention",
          Draft: "draft",
          "Checks running": "checksRunning",
          "In review": "inReview",
          Published: "published",
        } as Record<string, string>
      )[stackSummary(stack).label],
    })),
});
