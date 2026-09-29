<script lang="ts">
  import { tick, untrack } from "svelte";
  import { flip } from "svelte/animate";
  import { cubicOut } from "svelte/easing";
  import { Tween } from "svelte/motion";
  import { growIn, motionMs, shrinkOut, GRAPH_MOTION_MS } from "$lib/motion";
  import GraphRow from "./GraphRow.svelte";
  import ElisionRow from "./ElisionRow.svelte";
  import { laneBudget } from "./lanes.svelte";
  import { gutterWidth, nodeRowHeight, ELISION_ROW_HEIGHT, type GraphModel } from "./graph";
  import { density } from "./density.svelte";
  import { observeViewport, rowOffsets, visibleRange } from "$lib/viewport";

  let {
    model,
    emphasized,
    lanes,
    selectedIds,
    onselect,
    scroller,
    activeId = null,
  }: {
    model: GraphModel;
    /** Workstream id rendered hot; other streams stay calm. */
    emphasized: string | null;
    /** Stream id → lane slot, shared by every stream-colored surface. */
    lanes: Map<string, number>;
    /** Every selected change: one id, or a ⇧-click range's whole run. */
    selectedIds: ReadonlySet<string>;
    onselect: (id: string, event: MouseEvent) => void;
    scroller?: HTMLDivElement;
    activeId?: string | null;
  } = $props();

  // The gutter widens or narrows smoothly when a rewrite changes how many
  // columns the tree needs, or the pane's lane budget moves; rows receive
  // the fractional in-between widths.
  // svelte-ignore state_referenced_locally — initial width is meant to be
  // captured once; the effect tweens every later change.
  const gutter = new Tween(gutterWidth(model.columnCount, laneBudget.full), {
    easing: cubicOut,
  });
  $effect(() => {
    gutter.set(gutterWidth(model.columnCount, laneBudget.full), {
      duration: motionMs(GRAPH_MOTION_MS),
    });
  });

  let element = $state<HTMLDivElement>();
  let top = $state(0);
  let height = $state(800);
  const windowed = $derived(model.rows.length > 80 && scroller !== undefined);
  const offsets = $derived(rowOffsets(model.rows, (row) =>
    row.type === "node" ? nodeRowHeight(density.mode) : ELISION_ROW_HEIGHT,
  ));
  const totalHeight = $derived(offsets[offsets.length - 1]);
  const range = $derived(visibleRange(offsets, top, height, 300));

  $effect(() => {
    if (!scroller || !element || !windowed) return;
    return observeViewport(scroller, (view) => {
      top = view.top - element!.getBoundingClientRect().top;
      height = view.height;
    });
  });

  // Keyboard navigation can select an unmounted row. Reveal its known
  // offset first, so the next frame mounts it without a DOM lookup.
  $effect(() => {
    const id = activeId;
    const rows = model.rows;
    const positions = offsets;
    const root = element;
    const viewport = scroller;
    if (!id || !root || !viewport || !windowed) return;
    let active = true;
    void tick().then(() => {
      if (!active) return;
      const index = rows.findIndex((row) => row.type === "node" && row.node.id === id);
      if (index < 0) return;
      const start = viewport.getBoundingClientRect().top - root.getBoundingClientRect().top;
      let next = start;
      if (positions[index] < start) next = positions[index];
      else if (positions[index + 1] > start + viewport.clientHeight) {
        next = positions[index + 1] - viewport.clientHeight;
      }
      viewport.scrollTop += next - start;
      untrack(() => { top = next; height = viewport.clientHeight; });
    });
    return () => { active = false; };
  });
</script>

<!-- Change ids survive rewrites, so keyed rows carry over a rebase and the
     flip shows work sliding to its new place instead of teleporting; new
     rows grow in, removed rows fold closed, and inside each row the rails
     tween their columns. Together an applied rewrite reads as one motion. -->
<div
  class="graph"
  class:windowed
  bind:this={element}
  style:height={windowed ? `${totalHeight}px` : undefined}
  role="group"
  aria-label="Change graph"
>
  {#if windowed}
    {#each model.rows.slice(range.start, range.end) as row, index (row.type === "node" ? row.node.id : row.id)}
      <div class="virtual-row" style:top="{offsets[range.start + index]}px">
        {#if row.type === "node"}
          <GraphRow {row} gutter={gutter.current} {emphasized} {lanes}
            selected={selectedIds.has(row.node.id)}
            onselect={(event) => onselect(row.node.id, event)} />
        {:else}
          <ElisionRow {row} gutter={gutter.current} {emphasized} {lanes} />
        {/if}
      </div>
    {/each}
  {:else}
    {#each model.rows as row (row.type === "node" ? row.node.id : row.id)}
      <div
        animate:flip={{ duration: motionMs(GRAPH_MOTION_MS), easing: cubicOut }}
        in:growIn
        out:shrinkOut
      >
        {#if row.type === "node"}
          <GraphRow
            {row}
            gutter={gutter.current}
            {emphasized}
            {lanes}
            selected={selectedIds.has(row.node.id)}
            onselect={(event) => onselect(row.node.id, event)}
          />
        {:else}
          <ElisionRow {row} gutter={gutter.current} {emphasized} {lanes} />
        {/if}
      </div>
    {/each}
  {/if}
</div>

<style>
  .graph {
    display: flex;
    flex-direction: column;
  }

  .graph.windowed { position: relative; display: block; }
  .virtual-row { position: absolute; left: 0; right: 0; }
</style>
