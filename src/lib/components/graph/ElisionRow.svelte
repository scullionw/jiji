<script lang="ts">
  import {
    gutterWidth,
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
    columnCount: number;
    emphasized: string | null;
  } = $props();

  const H = ELISION_ROW_HEIGHT;
  const CY = H / 2;
  const gw = $derived(gutterWidth(columnCount));

  function railTone(rail: Rail): string {
    if (rail.stream === null) return "base";
    return rail.stream === emphasized ? "hot" : "calm";
  }
</script>

<!-- jj's `~`: history exists here but is not shown. -->
<div class="elision" title="History between these commits is not shown">
  <span class="gutter" style:width="{gw}px">
    <svg width={gw} height={H} viewBox="0 0 {gw} {H}" aria-hidden="true">
      {#each row.passThrough as rail (rail.col)}
        <path class="rail {railTone(rail)}" d="M {railX(rail.col)} 0 V {H}" />
      {/each}
      {#each row.marks as rail (rail.col)}
        {#if row.continues}
          <path
            class="rail {railTone(rail)}"
            d="M {railX(rail.col)} {CY + 6} V {H}"
          />
        {/if}
        <text class="tilde" x={railX(rail.col)} y={CY + 1}>~</text>
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
