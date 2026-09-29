<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";

  let {
    workstream,
    lane,
    onfocus,
  }: {
    workstream: WorkstreamSummary;
    /** Lane slot shared with the graph view, so hue stays identity. */
    lane: number;
    onfocus: () => void;
  } = $props();

  const count = $derived(workstream.nodeIds.length);
  const dots = $derived(Math.min(count, 4));
</script>

<button class="sibling" style:--lane={`var(--lane-${lane})`} onclick={onfocus}>
  <span class="mini" aria-hidden="true">
    {#each Array.from({ length: dots }) as _, i (i)}
      <i class="dot" class:wc={workstream.isActive && i === 0}></i>
    {/each}
  </span>
  <span class="text">
    <span class="line1">
      <span class="name truncate">{workstream.title}</span>
      {#if workstream.bookmark}
        <span class="ref sib-bm"><span class="truncate">{workstream.bookmark}</span></span>
      {/if}
      {#if workstream.isActive}
        <span class="wc-note">working copy</span>
      {/if}
    </span>
    <span class="meta">
      {count} change{count === 1 ? "" : "s"}{workstream.behindTrunk > 0
        ? ` · ${workstream.behindTrunk} behind trunk`
        : ""}
    </span>
  </span>
  <span class="open"><Icon name="chevronRight" size={14} /></span>
</button>

<style>
  .sibling {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    text-align: left;
    padding: 6px var(--sp-2);
    border-radius: var(--radius-m);
    transition: background var(--t-fast) var(--ease-out);
  }

  .sibling:hover {
    background: var(--clr-bg-hover);
  }

  .mini {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
    width: 14px;
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--lane, var(--clr-accent)) 65%, var(--clr-bg-1));
  }

  .dot.wc {
    background: var(--clr-working-copy);
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .line1 {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .name {
    font-size: var(--text-m);
    font-weight: 500;
    color: var(--clr-text-1);
    min-width: 0;
  }

  .meta {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .sib-bm {
    flex-shrink: 1;
    min-width: 0;
    max-width: 14em;
  }

  .wc-note {
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-working-copy);
  }

  .open {
    flex-shrink: 0;
    color: var(--clr-text-3);
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease-out);
  }

  .sibling:hover .open {
    opacity: 1;
  }
</style>
