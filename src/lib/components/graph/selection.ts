// The graph's contiguous multi-selection (⇧-click along a lane): a run of
// adjacent changes in one workstream, rendered by the diff pane as a single
// combined diff from the run's root parent to its head. Pure data — no
// Svelte. Ranges are id lists in workstream order (head-first), anchored on
// the change that was selected when the extension started.

import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import { findNode, stackPosition } from "$lib/components/inspector/inspect";

/** A contiguous selection the diff pane can render as one comparison. */
export interface RangeSelection {
  /** Every change in the run, head-first (workstream order). */
  ids: string[];
  /** What the combined diff is measured from: the root's first parent. */
  from: string;
}

/**
 * The run a ⇧-click extends to: anchor and target must sit in the same
 * workstream — the lane whose nodeIds spell out contiguity — and the range
 * is the slice between them, inclusive. Null when the two share no lane or
 * the run has no diff base; the caller falls back to a plain select. A
 * target equal to the anchor yields the single-id run (a collapse).
 */
export function extendRange(
  snapshot: RepoSnapshot,
  anchorId: string,
  targetId: string,
): string[] | null {
  const anchor = stackPosition(snapshot, anchorId);
  const target = stackPosition(snapshot, targetId);
  if (!anchor || !target || anchor.workstream.id !== target.workstream.id) {
    return null;
  }
  const lo = Math.min(anchor.index, target.index);
  const hi = Math.max(anchor.index, target.index);
  const ids = anchor.workstream.nodeIds.slice(lo, hi + 1);
  return rangeCompareFrom(snapshot, ids) === null ? null : ids;
}

/**
 * What a range's combined diff is measured from: the first parent of the
 * run's root (its oldest change) — so the diff shows everything the run
 * adds together, exactly `compareDiff(rootParent, head)`. Null when either
 * endpoint is missing from the drawn snapshot or the root is parentless;
 * such a range cannot render and must not exist.
 */
export function rangeCompareFrom(
  snapshot: RepoSnapshot,
  ids: string[],
): string | null {
  if (ids.length === 0 || !findNode(snapshot, ids[0])) return null;
  const root = findNode(snapshot, ids[ids.length - 1]);
  return root?.parents[0] ?? null;
}

/**
 * Whether a stored range still holds against a (possibly refreshed)
 * snapshot: still one contiguous run of a workstream, unchanged, with a
 * diff base. Rewrites that reshape the lane invalidate the range rather
 * than silently meaning something else.
 */
export function validRange(snapshot: RepoSnapshot, ids: string[]): boolean {
  if (ids.length < 2) return false;
  const head = stackPosition(snapshot, ids[0]);
  if (!head) return false;
  const current = head.workstream.nodeIds.slice(
    head.index,
    head.index + ids.length,
  );
  return (
    current.length === ids.length &&
    current.every((id, i) => id === ids[i]) &&
    rangeCompareFrom(snapshot, ids) !== null
  );
}
