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
