// Shared motion primitives for structural animation. Every duration routes
// through motionMs so the OS reduce-motion preference settles surfaces
// instantly instead of animating — which is also what keeps the visual
// harness deterministic (it forces reduced motion; Svelte outros never
// finish under headless Chrome's virtual time at real durations).

import { cubicOut } from "svelte/easing";
import type { TransitionConfig } from "svelte/transition";

const reduced =
  typeof window !== "undefined" && "matchMedia" in window
    ? window.matchMedia("(prefers-reduced-motion: reduce)")
    : null;

export function motionMs(ms: number): number {
  return reduced?.matches ? 0 : ms;
}

/** How long structural motion takes: row flips, rail morphs, column
 * shifts. One number so the whole graph moves as one gesture. */
export const GRAPH_MOTION_MS = 220;

/** A list row growing into place. The row animates its own height open so
 * neighbors part smoothly; a transform-only intro would make them jump.
 * The small delay lets the reflow of existing rows read first. */
export function growIn(node: Element): TransitionConfig {
  const height = (node as HTMLElement).offsetHeight;
  return {
    delay: motionMs(60),
    duration: motionMs(180),
    easing: cubicOut,
    css: (t) => `height: ${t * height}px; opacity: ${t}; overflow: hidden;`,
  };
}

/** The reverse: a removed row folds closed so the rows below slide up into
 * its place instead of teleporting. */
export function shrinkOut(node: Element): TransitionConfig {
  const height = (node as HTMLElement).offsetHeight;
  return {
    duration: motionMs(160),
    easing: cubicOut,
    css: (t) => `height: ${t * height}px; opacity: ${t}; overflow: hidden;`,
  };
}

/** An inline plan panel or card opening: height unfolds immediately (no
 * delay — the click that opened it is the cue) so the content below is
 * pushed rather than jumped. Intro-only by design, the palette's posture:
 * opening explains where the surface came from, closing on confirm or
 * cancel is instant, which is what makes executing feel snappy. */
export function panelIn(node: Element): TransitionConfig {
  const height = (node as HTMLElement).offsetHeight;
  return {
    duration: motionMs(170),
    easing: cubicOut,
    css: (t) =>
      `height: ${t * height}px; opacity: ${Math.min(1, t * 1.6)}; overflow: hidden;`,
  };
}

/** A whole view or section arriving (section switch, workbench view
 * toggle): a small settle-up so the swap reads as one gesture. Intro-only
 * — the outgoing view is gone the moment the choice is made. */
export function viewIn(node: Element): TransitionConfig {
  return {
    duration: motionMs(180),
    easing: cubicOut,
    css: (t) => `opacity: ${t}; transform: translateY(${(1 - t) * 8}px);`,
  };
}
