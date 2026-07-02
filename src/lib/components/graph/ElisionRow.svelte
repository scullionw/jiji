<script lang="ts">
  import { cubicOut } from "svelte/easing";
  import { Tween } from "svelte/motion";
  import { SvelteMap } from "svelte/reactivity";
  import { GRAPH_MOTION_MS, motionMs } from "$lib/motion";
  import {
    gutterWidth,
    keyedElisionRails,
    railX,
    ELISION_ROW_HEIGHT,
    type ElisionRow,
    type Rail,
  } from "./graph";

  let {
    row,
    columnCount,
    emphasized,
  }: {
    row: ElisionRow;
    /** May be fractional mid-morph — the view tweens it. */
    columnCount: number;
    emphasized: string | null;
  } = $props();

  const H = ELISION_ROW_HEIGHT;
  const CY = H / 2;
  const gw = $derived(gutterWidth(columnCount));

  // Same morph as GraphRow: rails and `~` marks keyed by role + stream
  // tween sideways when a rewrite shifts their columns.
  const rails = $derived(keyedElisionRails(row));
  const railXs = new SvelteMap<string, Tween<number>>();
  $effect(() => {
    for (const { key, rail } of rails) {
      const target = railX(rail.col);
      const existing = railXs.get(key);
      if (!existing) {
        railXs.set(key, new Tween(target, { easing: cubicOut }));
      } else if (existing.target !== target) {
        existing.set(target, { duration: motionMs(GRAPH_MOTION_MS) });
      }
    }
    for (const key of railXs.keys()) {
      if (!rails.some((kr) => kr.key === key)) railXs.delete(key);
    }
  });

  function xOf(key: string, rail: Rail): number {
    return railXs.get(key)?.current ?? railX(rail.col);
  }

  function railTone(rail: Rail): string {
    if (rail.stream === null) return "base";
    return rail.stream === emphasized ? "hot" : "calm";
  }
</script>

<!-- jj's `~`: history exists here but is not shown. -->
<div class="elision" title="History between these commits is not shown">
  <span class="gutter" style:width="{gw}px">
    <svg width={gw} height={H} viewBox="0 0 {gw} {H}" aria-hidden="true">
      {#each rails as kr (kr.key)}
        {#if kr.role === "pass"}
          <path class="rail {railTone(kr.rail)}" d="M {xOf(kr.key, kr.rail)} 0 V {H}" />
        {:else}
          {#if row.continues}
            <path
              class="rail {railTone(kr.rail)}"
              d="M {xOf(kr.key, kr.rail)} {CY + 6} V {H}"
            />
          {/if}
          <text class="tilde" x={xOf(kr.key, kr.rail)} y={CY + 1}>~</text>
        {/if}
      {/each}
    </svg>
  </span>
  <span class="label">elided revisions</span>
</div>

<style>
  .elision {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 18px;
    background: color-mix(in srgb, var(--clr-bg-0) 40%, transparent);
  }

  svg {
    display: block;
  }

  .rail {
    fill: none;
    stroke-width: 1.5;
  }

  .rail.hot {
    stroke: color-mix(in srgb, var(--clr-accent) 72%, var(--clr-bg-3));
  }

  .rail.calm {
    stroke: color-mix(in srgb, var(--clr-accent) 30%, var(--clr-bg-3));
  }

  .rail.base {
    stroke: color-mix(in srgb, var(--clr-text-3) 55%, var(--clr-bg-3));
  }

  .tilde {
    font-family: var(--font-mono);
    font-size: 11px;
    fill: var(--clr-text-3);
    text-anchor: middle;
    dominant-baseline: central;
  }

  .label {
    font-size: var(--text-xs);
    font-style: italic;
    color: color-mix(in srgb, var(--clr-text-3) 75%, transparent);
  }
</style>
