<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { WorkstreamSummary } from "$lib/bindings/WorkstreamSummary";

  let {
    workstream,
    onfocus,
  }: { workstream: WorkstreamSummary; onfocus: () => void } = $props();

  const count = $derived(workstream.nodeIds.length);
  const dots = $derived(Math.min(count, 4));
</script>

<button class="sibling" onclick={onfocus}>
  <span class="mini" aria-hidden="true">
    {#each Array.from({ length: dots }) as _, i (i)}
      <i class="dot" class:wc={workstream.isActive && i === 0}></i>
    {/each}
  </span>
  <span class="text">
    <span class="line1">
      <span class="name truncate">{workstream.title}</span>
      {#if workstream.bookmark}
        <span class="chip">
          <Icon name="bookmark" size={10} />
          {workstream.bookmark}
        </span>
      {/if}
      {#if workstream.isActive}
        <span class="chip wc-chip">working copy</span>
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
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
    transition:
      background var(--t-fast) var(--ease-out),
      border-color var(--t-fast) var(--ease-out);
  }

  .sibling:hover {
    background: var(--clr-bg-hover);
    border-color: var(--clr-border-1);
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
    background: color-mix(in srgb, var(--clr-accent) 55%, var(--clr-bg-3));
  }

  .dot.wc {
    background: var(--clr-working-copy);
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
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
    color: var(--clr-text-2);
    min-width: 0;
  }

  .meta {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 8px;
    background: var(--clr-bg-3);
    color: var(--clr-text-2);
    border: 1px solid var(--clr-border-2);
  }

  .wc-chip {
    background: var(--clr-working-copy-dim);
    color: var(--clr-working-copy);
    border: none;
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
