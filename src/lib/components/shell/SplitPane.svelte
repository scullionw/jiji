<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    id,
    min = 240,
    max = 460,
    initial = 320,
    side = "end",
    children,
    panel,
  }: {
    id: string;
    min?: number;
    max?: number;
    initial?: number;
    /** Which edge the fixed-width panel docks to; `children` takes the rest. */
    side?: "start" | "end";
    children: Snippet;
    panel: Snippet;
  } = $props();

  function storageKey(): string {
    return `jiji.pane.${id}`;
  }

  function readSaved(): number {
    const saved = Number(localStorage.getItem(storageKey()));
    return Number.isFinite(saved) && saved >= min && saved <= max
      ? saved
      : initial;
  }

  let width = $state(readSaved());
  let dragging = $state(false);
  let container: HTMLDivElement;

  function startDrag(event: PointerEvent) {
    dragging = true;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function onDrag(event: PointerEvent) {
    if (!dragging) return;
    const rect = container.getBoundingClientRect();
    const raw =
      side === "end" ? rect.right - event.clientX : event.clientX - rect.left;
    width = Math.min(max, Math.max(min, raw));
  }

  function endDrag() {
    if (!dragging) return;
    dragging = false;
    localStorage.setItem(storageKey(), String(Math.round(width)));
  }
</script>

<div class="split" bind:this={container}>
  {#if side === "start"}
    <aside class="panel" style:width="{width}px">{@render panel()}</aside>
  {:else}
    <div class="primary">{@render children()}</div>
  {/if}
  <div
    class="divider"
    class:dragging
    role="separator"
    aria-orientation="vertical"
    onpointerdown={startDrag}
    onpointermove={onDrag}
    onpointerup={endDrag}
    onpointercancel={endDrag}
  ></div>
  {#if side === "start"}
    <div class="primary">{@render children()}</div>
  {:else}
    <aside class="panel" style:width="{width}px">{@render panel()}</aside>
  {/if}
</div>

<style>
  .split {
    display: flex;
    height: 100%;
    min-height: 0;
  }

  .primary {
    flex: 1;
    min-width: 0;
    min-height: 0;
  }

  .divider {
    position: relative;
    width: 1px;
    flex-shrink: 0;
    background: var(--clr-border-2);
    transition: background var(--t-fast) var(--ease-out);
  }

  /* Wider invisible hit area for the 1px divider. */
  .divider::after {
    content: "";
    position: absolute;
    inset: 0 -4px;
    cursor: col-resize;
  }

  .divider:hover,
  .divider.dragging {
    background: var(--clr-accent);
  }

  .panel {
    flex-shrink: 0;
    min-height: 0;
    overflow-y: auto;
  }
</style>
