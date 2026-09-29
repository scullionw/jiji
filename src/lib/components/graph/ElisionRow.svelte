<script lang="ts">
  import { cubicOut } from "svelte/easing";
  import { Tween } from "svelte/motion";
  import { SvelteMap } from "svelte/reactivity";
  import { GRAPH_MOTION_MS, motionMs } from "$lib/motion";
  import { laneBudget } from "./lanes.svelte";
  import {
    keyedElisionRails,
    laneTier,
    laneVar,
    railX,
    ELISION_ROW_HEIGHT,
    type ElisionRow,
    type Rail,
  } from "./graph";

  let {
    row,
    gutter,
    emphasized,
    lanes,
  }: {
    row: ElisionRow;
    /** Gutter width in px; may be fractional mid-morph — the view tweens it. */
    gutter: number;
    emphasized: string | null;
    /** Stream id → lane slot; rails wear their stream's hue. */
    lanes: Map<string, number>;
  } = $props();

  const H = ELISION_ROW_HEIGHT;
  const CY = H / 2;
  const gw = $derived(gutter);
  const full = $derived(laneBudget.full);

  // Same morph as GraphRow: rails and `~` marks keyed by role + stream
  // tween sideways when a rewrite shifts their columns.
  const rails = $derived(keyedElisionRails(row));
  const railXs = new SvelteMap<string, Tween<number>>();
  $effect(() => {
    for (const { key, rail } of rails) {
      const target = railX(rail.col, full);
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
    return railXs.get(key)?.current ?? railX(rail.col, full);
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
        {@const railTier = laneTier(kr.rail.col, full)}
        {#if kr.role === "pass"}
          <path
            class="rail {railTone(kr.rail)}"
            class:narrow={railTier !== "full"}
            class:overflow={railTier === "overflow"}
            style:--lane={laneVar(lanes, kr.rail.stream)}
            d="M {xOf(kr.key, kr.rail)} 0 V {H}"
          />
        {:else}
          {#if row.continues}
            <path
              class="rail {railTone(kr.rail)}"
              class:narrow={railTier !== "full"}
              class:overflow={railTier === "overflow"}
              style:--lane={laneVar(lanes, kr.rail.stream)}
              d="M {xOf(kr.key, kr.rail)} {CY + 6} V {H}"
            />
          {/if}
          <text
            class="tilde"
            class:narrow={railTier !== "full"}
            x={xOf(kr.key, kr.rail)}
            y={CY + 1}>~</text
          >
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
  }

  svg {
    display: block;
  }

  .rail {
    fill: none;
    stroke-width: 1.5;
    stroke-linecap: round;
  }

  .rail.hot {
    stroke: color-mix(in srgb, var(--lane, var(--clr-accent)) 80%, var(--clr-bg-1));
  }

  .rail.calm {
    stroke: color-mix(in srgb, var(--lane, var(--clr-accent)) 38%, var(--clr-bg-1));
  }

  .rail.base {
    stroke: color-mix(in srgb, var(--clr-text-3) 50%, var(--clr-bg-1));
  }

  .rail.narrow {
    stroke-width: 1;
  }

  .rail.overflow {
    opacity: 0.55;
  }

  .tilde {
    font-family: var(--font-mono);
    font-size: 11px;
    fill: var(--clr-text-3);
    text-anchor: middle;
    dominant-baseline: central;
  }

  .tilde.narrow {
    font-size: 8px;
  }

  .label {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
