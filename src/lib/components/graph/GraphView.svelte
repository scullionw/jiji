<script lang="ts">
  import { flip } from "svelte/animate";
  import { cubicOut } from "svelte/easing";
  import { Tween } from "svelte/motion";
  import { growIn, motionMs, shrinkOut, GRAPH_MOTION_MS } from "$lib/motion";
  import GraphRow from "./GraphRow.svelte";
  import ElisionRow from "./ElisionRow.svelte";
  import type { GraphModel } from "./graph";

  let {
    model,
    emphasized,
    lanes,
    selectedId,
    onselect,
  }: {
    model: GraphModel;
    /** Workstream id rendered hot; other streams stay calm. */
    emphasized: string | null;
    /** Stream id → lane slot, shared by every stream-colored surface. */
    lanes: Map<string, number>;
    selectedId: string | null;
    onselect: (id: string) => void;
  } = $props();

  // The gutter widens or narrows smoothly when a rewrite changes how many
  // columns the tree needs; rows receive the fractional in-between widths.
  // svelte-ignore state_referenced_locally — initial width is meant to be
  // captured once; the effect tweens every later change.
  const cols = new Tween(model.columnCount, { easing: cubicOut });
  $effect(() => {
    cols.set(model.columnCount, { duration: motionMs(GRAPH_MOTION_MS) });
  });
</script>

<!-- Change ids survive rewrites, so keyed rows carry over a rebase and the
     flip shows work sliding to its new place instead of teleporting; new
     rows grow in, removed rows fold closed, and inside each row the rails
     tween their columns. Together an applied rewrite reads as one motion. -->
<div class="graph" role="listbox" aria-label="Change graph">
  {#each model.rows as row (row.type === "node" ? row.node.id : row.id)}
    <div
      animate:flip={{ duration: motionMs(GRAPH_MOTION_MS), easing: cubicOut }}
      in:growIn
      out:shrinkOut
    >
      {#if row.type === "node"}
        <GraphRow
          {row}
          columnCount={cols.current}
          {emphasized}
          {lanes}
          selected={selectedId === row.node.id}
          onselect={() => onselect(row.node.id)}
        />
      {:else}
        <ElisionRow {row} columnCount={cols.current} {emphasized} {lanes} />
      {/if}
    </div>
  {/each}
</div>

<style>
  .graph {
    display: flex;
    flex-direction: column;
  }
</style>
