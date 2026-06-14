<script lang="ts">
  import GraphRow from "./GraphRow.svelte";
  import ElisionRow from "./ElisionRow.svelte";
  import type { GraphModel } from "./graph";

  let {
    model,
    emphasized,
    selectedId,
    onselect,
  }: {
    model: GraphModel;
    /** Workstream id rendered hot; other streams stay calm. */
    emphasized: string | null;
    selectedId: string | null;
    onselect: (id: string) => void;
  } = $props();
</script>

<div class="graph" role="listbox" aria-label="Change graph">
  {#each model.rows as row (row.type === "node" ? row.node.id : row.id)}
    {#if row.type === "node"}
      <GraphRow
        {row}
        columnCount={model.columnCount}
        {emphasized}
        selected={selectedId === row.node.id}
        onselect={() => onselect(row.node.id)}
      />
    {:else}
      <ElisionRow {row} columnCount={model.columnCount} {emphasized} />
    {/if}
  {/each}
</div>

<style>
  .graph {
    display: flex;
    flex-direction: column;
  }
</style>
