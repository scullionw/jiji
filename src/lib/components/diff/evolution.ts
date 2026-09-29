// The evolution timeline's pure logic: versions in reading order, and the
// selected span of rewrite steps — how clicks and arrow keys move it.
// A span names two versions by index (oldest-first); the surface shows
// the diff versions[from] → versions[to].

import type { ChangeEvolog } from "$lib/bindings/ChangeEvolog";
import type { EvologEntry } from "$lib/bindings/EvologEntry";

export interface Span {
  from: number;
  to: number;
}

/** Versions oldest → newest — the timeline's reading order. The backend
 * answers newest first. */
export function timelineVersions(evolog: ChangeEvolog): EvologEntry[] {
  return [...evolog.entries].reverse();
}

/** Where the pane opens: the newest rewrite step — "what did I just
 * change". Null when the change has a single version (no step exists). */
export function defaultSpan(count: number): Span | null {
  return count >= 2 ? { from: count - 2, to: count - 1 } : null;
}

/** The single step that produced version `i` (clicking its dot). The
 * oldest version has no producing step, so it selects the first step —
 * the click still lands somewhere sensible. */
export function stepTo(i: number, count: number): Span | null {
  if (count < 2) return null;
  const to = Math.min(Math.max(i, 1), count - 1);
  return { from: to - 1, to };
}

/** Widen `span` to include version `i` (⇧-click). Landing inside the
 * span narrows toward the clicked version from the far end, so ⇧-click
 * always answers "measure to here". */
export function extendTo(span: Span, i: number, count: number): Span {
  const at = Math.min(Math.max(i, 0), count - 1);
  if (at < span.from) return { from: at, to: span.to };
  if (at > span.to) return { from: span.from, to: at };
  // Inside: keep the anchor further from the click.
  return at - span.from >= span.to - at
    ? { from: span.from, to: Math.max(at, span.from + 1) }
    : { from: Math.min(at, span.to - 1), to: span.to };
}

/** Scrub by one step (←/→): the selection collapses to the single step
 * ending at the moved edge, sliding along the chain. */
export function scrub(span: Span, delta: -1 | 1, count: number): Span | null {
  if (count < 2) return null;
  const width = span.to - span.from;
  const to = Math.min(Math.max(width === 1 ? span.to + delta : span.to, 1), count - 1);
  return { from: to - 1, to };
}

/** Grow one edge by a step (⇧←/⇧→); already at the end, nothing moves. */
export function grow(span: Span, edge: "older" | "newer", count: number): Span {
  return edge === "older"
    ? { from: Math.max(0, span.from - 1), to: span.to }
    : { from: span.from, to: Math.min(count - 1, span.to + 1) };
}
