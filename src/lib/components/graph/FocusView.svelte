<script lang="ts">
  import { flip } from "svelte/animate";
  import { cubicOut } from "svelte/easing";
  import { Tween } from "svelte/motion";
  import Icon from "$lib/components/ui/Icon.svelte";
  import {
    growIn,
    motionMs,
    shrinkOut,
    viewIn,
    GRAPH_MOTION_MS,
  } from "$lib/motion";
  import GraphRow from "./GraphRow.svelte";
  import ElisionRow from "./ElisionRow.svelte";
  import SiblingLane from "./SiblingLane.svelte";
  import { gutterWidth, railX, type FocusModel } from "./graph";
  import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";

  let {
    model,
    workstream,
    siblings,
    lanes,
    selectedId,
    onselect,
    onfocus,
  }: {
    model: FocusModel;
    workstream: WorkstreamSummary;
    siblings: WorkstreamSummary[];
    /** Stream id → lane slot, from the full snapshot so hues match the
     *  graph view. */
    lanes: Map<string, number>;
    selectedId: string | null;
    onselect: (id: string) => void;
    onfocus: (id: string) => void;
  } = $props();

  // The chain renders inside the card body; the immutable base(s) and their
  // terminal `~` rows sit in a recessed zone below a "trunk"/"based on"
  // caption.
  const splitAt = $derived(
    model.graph.rows.findIndex(
      (row) => row.type === "node" && row.node.kind === "immutable",
    ),
  );
  const chainRows = $derived(
    splitAt === -1 ? model.graph.rows : model.graph.rows.slice(0, splitAt),
  );
  const baseRows = $derived(
    splitAt === -1 ? [] : model.graph.rows.slice(splitAt),
  );

  // Same width morph as the graph view when a rewrite changes the lane's
  // column needs.
  // svelte-ignore state_referenced_locally — initial width is meant to be
  // captured once; the effect tweens every later change.
  const cols = new Tween(model.graph.columnCount, { easing: cubicOut });
  $effect(() => {
    cols.set(model.graph.columnCount, { duration: motionMs(GRAPH_MOTION_MS) });
  });
  const gw = $derived(gutterWidth(cols.current));

  function plural(n: number, word: string): string {
    return `${n} ${word}${n === 1 ? "" : "s"}`;
  }
</script>

{#key workstream.id}
  <section class="lane" in:viewIn>
    <header class="lane-head">
      <i
        class="lane-dot"
        style:--lane={`var(--lane-${lanes.get(workstream.id) ?? 0})`}
      ></i>
      <h2 class="truncate">{workstream.title}</h2>
      {#if workstream.bookmark}
        <span class="chip">
          <Icon name="bookmark" size={11} />
          {workstream.bookmark}
        </span>
      {/if}
      <span class="lane-meta">
        {plural(workstream.nodeIds.length, "change")}{#if model.behindTrunk > 0}<span
            class="behind-note"
          >
            · {model.behindTrunk} behind trunk</span
          >{/if}
      </span>
    </header>

    <div class="rows">
      <!-- Keyed by change id like the graph view, so an applied reorder
           slides rows to their new place, new rows grow in, and removed
           rows fold closed. -->
      {#each chainRows as row (row.type === "node" ? row.node.id : row.id)}
        <div
          animate:flip={{ duration: motionMs(GRAPH_MOTION_MS), easing: cubicOut }}
          in:growIn
          out:shrinkOut
        >
          {#if row.type === "node"}
            <GraphRow
              {row}
              columnCount={cols.current}
              emphasized={workstream.id}
              {lanes}
              selected={selectedId === row.node.id}
              onselect={() => onselect(row.node.id)}
            />
          {:else}
            <ElisionRow
              {row}
              columnCount={cols.current}
              emphasized={workstream.id}
              {lanes}
            />
          {/if}
        </div>
      {/each}
    </div>

    {#if baseRows.length > 0}
      <div class="base-zone">
        <div class="zone-caption">
          <span class="zone-gutter" style:width="{gw}px">
            <i class="zone-rail" style:left="{railX(0) - 1}px"></i>
          </span>
          <span class="zone-label">
            {model.trunkOnBase ? "trunk" : "based on"}
          </span>
        </div>
        {#each baseRows as row (row.type === "node" ? row.node.id : row.id)}
          <div
            animate:flip={{ duration: motionMs(GRAPH_MOTION_MS), easing: cubicOut }}
            in:growIn
            out:shrinkOut
          >
            {#if row.type === "node"}
              <GraphRow
                {row}
                columnCount={cols.current}
                emphasized={workstream.id}
                {lanes}
                selected={selectedId === row.node.id}
                onselect={() => onselect(row.node.id)}
              />
            {:else}
              <ElisionRow
                {row}
                columnCount={cols.current}
                emphasized={workstream.id}
                {lanes}
              />
            {/if}
          </div>
        {/each}
        {#if model.behindTrunk > 0 && model.trunkName}
          <p class="behind-foot">
            {model.trunkName} is {plural(model.behindTrunk, "change")} ahead
            of this base
          </p>
        {/if}
      </div>
    {/if}
  </section>
{/key}

{#if siblings.length > 0}
  <h3 class="aside-label">Other workstreams</h3>
  <div class="siblings">
    {#each siblings as sibling (sibling.id)}
      <SiblingLane
        workstream={sibling}
        lane={lanes.get(sibling.id) ?? 0}
        onfocus={() => onfocus(sibling.id)}
      />
    {/each}
  </div>
{/if}

<style>
  .lane {
    max-width: 720px;
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
    overflow: hidden;
  }

  .lane-head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .lane-dot {
    flex-shrink: 0;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--lane, var(--clr-accent));
  }

  h2 {
    font-size: var(--text-l);
    font-weight: 600;
    letter-spacing: -0.2px;
    min-width: 0;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 2px 8px;
    background: var(--clr-bg-3);
    color: var(--clr-text-2);
    border: 1px solid var(--clr-border-2);
  }

  .lane-meta {
    margin-left: auto;
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .behind-note {
    color: var(--clr-warn);
  }

  .rows {
    padding: var(--sp-2) 0;
  }

  /* Immutable context the stack sits on, visually recessed below the
     mutable rows. */
  .base-zone {
    background: color-mix(in srgb, var(--clr-bg-1) 55%, var(--clr-bg-2));
    border-top: 1px solid var(--clr-border-2);
    padding-bottom: var(--sp-2);
  }

  .zone-caption {
    display: flex;
    align-items: center;
    height: 26px;
  }

  .zone-gutter {
    position: relative;
    align-self: stretch;
    flex-shrink: 0;
  }

  .zone-rail {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    background: repeating-linear-gradient(
      to bottom,
      var(--clr-border-1) 0 4px,
      transparent 4px 8px
    );
  }

  .zone-label {
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--clr-text-3);
  }

  .behind-foot {
    padding: var(--sp-1) var(--sp-4) var(--sp-1) 46px;
    font-size: var(--text-s);
    color: var(--clr-warn);
  }

  .aside-label {
    margin: var(--sp-6) 0 var(--sp-2);
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--clr-text-3);
  }

  .siblings {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    max-width: 720px;
  }
</style>
