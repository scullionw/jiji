<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    id,
    min = 240,
    max = 460,
    initial = 320,
    side = "end",
    collapsed = false,
    children,
    panel,
  }: {
    id: string;
    min?: number;
    max?: number;
    initial?: number;
    /** Which edge the fixed-width panel docks to; `children` takes the rest. */
    side?: "start" | "end";
    /** Slides the panel off its edge; `children` takes the whole width. */
    collapsed?: boolean;
    children: Snippet;
    panel: Snippet;
  } = $props();

  function storageKey(): string {
    return `jiji.pane.${id}`;
  }

  function readSaved(): number {
    try {
      const saved = Number(localStorage.getItem(storageKey()));
      if (Number.isFinite(saved) && saved >= min && saved <= max) return saved;
    } catch { /* Storage is optional. */ }
    return initial;
  }

  let width = $state(readSaved());
  let dragging = $state(false);
  let container = $state<HTMLDivElement>();
  let available = $state(Infinity);
  const limit = $derived(Math.min(max, Math.max(0, available - 320)));
  const minimum = $derived(Math.min(min, limit));
  const shown = $derived(Math.max(minimum, Math.min(width, limit)));

  $effect(() => {
    if (!container) return;
    const observer = new ResizeObserver(([entry]) => { available = entry.contentRect.width; });
    observer.observe(container);
    return () => observer.disconnect();
  });

  function save() {
    try { localStorage.setItem(storageKey(), String(Math.round(width))); }
    catch { /* Storage is optional. */ }
  }

  function startDrag(event: PointerEvent) {
    if (event.button !== 0) return;
    event.preventDefault();
    dragging = true;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function onDrag(event: PointerEvent) {
    if (!dragging || !container) return;
    const rect = container.getBoundingClientRect();
    const raw =
      side === "end" ? rect.right - event.clientX : event.clientX - rect.left;
    width = Math.min(limit, Math.max(minimum, raw));
  }

  function endDrag() {
    if (!dragging) return;
    dragging = false;
    save();
  }

  function onKeydown(event: KeyboardEvent) {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    event.stopPropagation();
    const delta = (event.key === "ArrowRight" ? 16 : -16) * (side === "start" ? 1 : -1);
    width = event.key === "Home" ? minimum : event.key === "End" ? limit
      : Math.min(limit, Math.max(minimum, shown + delta));
    save();
  }

  function reset() {
    width = initial;
    save();
  }
</script>

<!-- Collapse slides the panel off its own edge (a margin, so its content
     never reflows) and retires the divider; the clip keeps the slide
     inside the split's frame. -->
<div class="split" class:collapsed bind:this={container}>
  {#if side === "start"}
    <aside
      class="panel"
      inert={collapsed}
      aria-hidden={collapsed}
      style:width="{shown}px"
      style:margin-inline-start={collapsed ? `-${shown}px` : null}
    >
      {@render panel()}
    </aside>
  {:else}
    <div class="primary">{@render children()}</div>
  {/if}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions (A window splitter is a focusable separator with Arrow/Home/End controls.) -->
  <div
    class="divider"
    class:dragging
    role="separator"
    tabindex={collapsed ? -1 : 0}
    aria-label="Resize {id} panel"
    aria-valuemin={minimum}
    aria-valuemax={limit}
    aria-valuenow={shown}
    aria-orientation="vertical"
    onkeydown={onKeydown}
    ondblclick={reset}
    onpointerdown={startDrag}
    onpointermove={onDrag}
    onpointerup={endDrag}
    onpointercancel={endDrag}
    onlostpointercapture={endDrag}
  ></div>
  {#if side === "start"}
    <div class="primary">{@render children()}</div>
  {:else}
    <aside
      class="panel"
      inert={collapsed}
      aria-hidden={collapsed}
      style:width="{shown}px"
      style:margin-inline-end={collapsed ? `-${shown}px` : null}
    >
      {@render panel()}
    </aside>
  {/if}
</div>

<style>
  .split {
    display: flex;
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }

  .primary {
    flex: 1;
    min-width: 0;
    min-height: 0;
  }

  .divider {
    touch-action: none;
    position: relative;
    width: 1px;
    flex-shrink: 0;
    background: var(--clr-border-2);
    transition: background var(--t-fast) var(--ease-out);
  }

  .collapsed .divider {
    display: none;
  }

  /* Wider invisible hit area for the 1px divider. */
  .divider::after {
    content: "";
    position: absolute;
    inset: 0 -4px;
    z-index: 4;
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
    transition: margin var(--t-med) var(--ease-out);
  }
</style>
