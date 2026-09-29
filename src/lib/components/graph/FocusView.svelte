<script lang="ts">
  import { cubicOut } from "svelte/easing";
  import { Tween } from "svelte/motion";
  import { motionMs, GRAPH_MOTION_MS } from "$lib/motion";
  import GraphView from "./GraphView.svelte";
  import SiblingLane from "./SiblingLane.svelte";
  import { laneBudget } from "./lanes.svelte";
  import { gutterWidth, railX, type FocusModel } from "./graph";
  import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";

  let {
    model,
    workstream,
    siblings,
    lanes,
    selectedIds,
    onselect,
    onfocus,
    scroller,
    activeId = null,
  }: {
    model: FocusModel;
    workstream: WorkstreamSummary;
    siblings: WorkstreamSummary[];
    /** Stream id → lane slot, from the full snapshot so hues match the
     *  graph view. */
    lanes: Map<string, number>;
    /** Every selected change: one id, or a ⇧-click range's whole run. */
    selectedIds: ReadonlySet<string>;
    onselect: (id: string, event: MouseEvent) => void;
    onfocus: (id: string) => void;
    scroller?: HTMLDivElement;
    activeId?: string | null;
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

  const chain = $derived({ ...model.graph, rows: chainRows });
  const base = $derived({ ...model.graph, rows: baseRows });

  // Same width morph as the graph view when a rewrite changes the lane's
  // column needs.
  // svelte-ignore state_referenced_locally — initial width is meant to be
  // captured once; the effect tweens every later change.
  const gutter = new Tween(
    gutterWidth(model.graph.columnCount, laneBudget.full),
    { easing: cubicOut },
  );
  $effect(() => {
    gutter.set(gutterWidth(model.graph.columnCount, laneBudget.full), {
      duration: motionMs(GRAPH_MOTION_MS),
    });
  });
  const gw = $derived(gutter.current);

  function plural(n: number, word: string): string {
    return `${n} ${word}${n === 1 ? "" : "s"}`;
  }
</script>

{#key workstream.id}
  <section class="lane">
    <header class="lane-head">
      <i
        class="lane-dot"
        style:--lane={`var(--lane-${lanes.get(workstream.id) ?? 0})`}
      ></i>
      <h2 class="truncate">{workstream.title}</h2>
      {#if workstream.bookmark}
        <span class="ref lane-bm"><span class="truncate">{workstream.bookmark}</span></span>
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
      <GraphView model={chain} emphasized={workstream.id} {lanes}
        {selectedIds} {onselect} {scroller} {activeId} />
    </div>

    {#if baseRows.length > 0}
      <div class="base-zone">
        <div class="zone-caption">
          <span class="zone-gutter" style:width="{gw}px">
            <i class="zone-rail" style:left="{railX(0) - 1}px"></i>
          </span>
          <span class="zone-label">
            {model.trunkOnBase ? "Trunk" : "Based on"}
          </span>
        </div>
        <GraphView model={base} emphasized={workstream.id} {lanes}
          {selectedIds} {onselect} {scroller} {activeId} />
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
    max-width: 760px;
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
    overflow: hidden;
  }

  .lane-head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 36px;
    padding: 0 var(--sp-3);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .lane-dot {
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--lane, var(--clr-accent));
  }

  h2 {
    font-size: var(--text-m);
    font-weight: 600;
    min-width: 0;
  }

  .lane-bm {
    flex-shrink: 1;
    min-width: 0;
    max-width: 16em;
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
    padding: var(--sp-1) 0;
  }

  /* Immutable context the stack sits on, below a hairline. */
  .base-zone {
    border-top: 1px solid var(--clr-border-2);
    padding-bottom: var(--sp-1);
  }

  .zone-caption {
    display: flex;
    align-items: center;
    height: 24px;
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
    border-left: 1.5px dashed var(--clr-border-1);
  }

  .zone-label {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--clr-text-3);
  }

  .behind-foot {
    padding: var(--sp-1) var(--sp-4) var(--sp-1) 46px;
    font-size: var(--text-s);
    color: var(--clr-warn);
  }

  .aside-label {
    max-width: 760px;
    margin: var(--sp-5) 0 var(--sp-1);
    padding: 0 var(--sp-2);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--clr-text-3);
  }

  .siblings {
    display: flex;
    flex-direction: column;
    max-width: 760px;
  }
</style>
