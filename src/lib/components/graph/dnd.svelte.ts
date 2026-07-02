// The live drag session behind graph drag-and-drop. One reactive state
// object (the rows, the floating plan card, and the status bar all read
// it) plus the pointer controller that drives it: delegated pointerdown on
// any `[data-node-id]` row, a small movement threshold so clicks stay
// clicks, elementFromPoint retargeting while the pointer moves, ⌥ toggling
// the move-alone scope mid-drag, Esc cancelling, and edge auto-scroll so
// long graphs can be reparented end to end. Releasing runs the same
// rebase/move mutations as the explicit rebase panel — breadcrumb, Undo,
// and selection-follow included.

import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import { findNode } from "$lib/components/inspector/inspect";
import { moveChange, rebaseChange, runQuiet } from "$lib/state/actions";
import { canDrag, planDrop, type DropPlan } from "./dnd";

export const drag = $state({
  active: false,
  sourceId: "",
  targetId: null as string | null,
  /** ⌥ held: only this change moves; descendants stay behind. */
  alone: false,
  x: 0,
  y: 0,
  plan: null as DropPlan | null,
});

/** Pixels of travel before a press becomes a drag instead of a click. */
const THRESHOLD = 5;
/** Edge band that auto-scrolls the pane, and the per-frame speed cap. */
const EDGE = 36;
const MAX_STEP = 14;

export function attachRowDnd(
  container: HTMLElement,
  getSnapshot: () => RepoSnapshot | null,
): () => void {
  let pressed: { id: string; x: number; y: number } | null = null;
  // One drop mutation at a time; new presses wait for the refresh.
  let busy = false;
  let raf = 0;

  function reset() {
    pressed = null;
    drag.active = false;
    drag.targetId = null;
    drag.plan = null;
    document.body.classList.remove("row-dragging");
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
  }

  function replan() {
    const snapshot = getSnapshot();
    drag.plan =
      snapshot && drag.targetId
        ? planDrop(snapshot, drag.sourceId, drag.targetId, drag.alone)
        : null;
  }

  function setAlone(alone: boolean) {
    if (drag.alone === alone) return;
    drag.alone = alone;
    replan();
  }

  // The row under the pointer, found by scanning row geometry rather than
  // elementFromPoint: rects come straight from layout, so targeting works
  // identically for real pointers, auto-scroll ticks, and the synthetic
  // events the visual harness dispatches (headless Chrome has no hit-test
  // data before first paint, so elementFromPoint answers nothing there).
  // Bounded by the pane, and linear over at most the 500-node snapshot cap.
  function rowAt(x: number, y: number): string | null {
    const bounds = container.getBoundingClientRect();
    if (x < bounds.left || x > bounds.right || y < bounds.top || y > bounds.bottom) {
      return null;
    }
    for (const el of container.querySelectorAll("[data-node-id]")) {
      const rect = el.getBoundingClientRect();
      if (y >= rect.top && y <= rect.bottom && x >= rect.left && x <= rect.right) {
        return el.getAttribute("data-node-id");
      }
    }
    return null;
  }

  function retarget() {
    const id = rowAt(drag.x, drag.y);
    if (id === drag.targetId) return;
    drag.targetId = id;
    replan();
  }

  // Auto-scroll while the pointer sits in the pane's edge bands; the loop
  // also re-picks the target as rows slide under a resting pointer.
  function loop() {
    if (!drag.active) {
      raf = 0;
      return;
    }
    const rect = container.getBoundingClientRect();
    if (drag.x >= rect.left && drag.x <= rect.right) {
      let dy = 0;
      if (drag.y < rect.top + EDGE) {
        dy = Math.max(-MAX_STEP, (drag.y - (rect.top + EDGE)) / 4);
      } else if (drag.y > rect.bottom - EDGE) {
        dy = Math.min(MAX_STEP, (drag.y - (rect.bottom - EDGE)) / 4);
      }
      if (dy !== 0) {
        const before = container.scrollTop;
        container.scrollTop = before + dy;
        if (container.scrollTop !== before) retarget();
      }
    }
    raf = requestAnimationFrame(loop);
  }

  function onPointerDown(event: PointerEvent) {
    if (event.button !== 0 || busy || drag.active) return;
    const row = (event.target as Element | null)?.closest("[data-node-id]");
    if (!row || !container.contains(row)) return;
    const id = row.getAttribute("data-node-id");
    const snapshot = getSnapshot();
    const node = snapshot && id ? findNode(snapshot, id) : undefined;
    if (!id || !node || !canDrag(node)) return;
    pressed = { id, x: event.clientX, y: event.clientY };
  }

  function onPointerMove(event: PointerEvent) {
    // A release outside the window never delivers pointerup (no pointer
    // capture — a captured pointer id must be real, which the harness's
    // synthetic events are not). The next movement shows the button is up:
    // cancel rather than guess at a drop that already happened elsewhere.
    if (drag.active && event.buttons === 0) {
      reset();
      return;
    }
    if (pressed && !drag.active) {
      const travel = Math.hypot(
        event.clientX - pressed.x,
        event.clientY - pressed.y,
      );
      if (travel < THRESHOLD) return;
      drag.sourceId = pressed.id;
      drag.active = true;
      drag.alone = event.altKey;
      drag.targetId = null;
      drag.plan = null;
      document.body.classList.add("row-dragging");
      raf = requestAnimationFrame(loop);
    }
    if (!drag.active) return;
    drag.x = event.clientX;
    drag.y = event.clientY;
    setAlone(event.altKey);
    retarget();
  }

  function onPointerUp() {
    if (!drag.active) {
      pressed = null;
      return;
    }
    const { sourceId, targetId, alone } = drag;
    reset();
    const snapshot = getSnapshot();
    if (!snapshot || !targetId) return;
    // Recompute at release so a snapshot that refreshed mid-drag cannot
    // execute a stale plan.
    const plan = planDrop(snapshot, sourceId, targetId, alone);
    if (!plan.allowed) return;
    busy = true;
    const call = plan.op === "move" ? moveChange : rebaseChange;
    // No panel owns a drop, so failures land in the status bar's error
    // slot, like palette-launched mutations.
    void runQuiet(() => call(sourceId, targetId)).finally(() => {
      busy = false;
    });
  }

  function onKeyDown(event: KeyboardEvent) {
    if (event.key === "Escape" && (drag.active || pressed)) {
      event.preventDefault();
      // Esc cancels the gesture only — keep other window bindings (the
      // workbench clears its selection on Esc) out of it, whichever
      // listener the browser happens to run first.
      event.stopImmediatePropagation();
      reset();
    } else if (event.key === "Alt" && drag.active) {
      setAlone(true);
    }
  }

  function onKeyUp(event: KeyboardEvent) {
    if (event.key === "Alt" && drag.active) setAlone(false);
  }

  container.addEventListener("pointerdown", onPointerDown);
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp);
  window.addEventListener("pointercancel", reset);
  window.addEventListener("blur", reset);
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  return () => {
    reset();
    container.removeEventListener("pointerdown", onPointerDown);
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", onPointerUp);
    window.removeEventListener("pointercancel", reset);
    window.removeEventListener("blur", reset);
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("keyup", onKeyUp);
  };
}
