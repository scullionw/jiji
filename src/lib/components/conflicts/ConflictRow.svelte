<script lang="ts">
  import type { ConflictItem } from "$lib/bindings/ConflictItem";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { findNode, splitPath } from "$lib/components/inspector/inspect";
  import { jumpToChange } from "$lib/state/actions";
  import { app } from "$lib/state/app.svelte";

  let { item }: { item: ConflictItem } = $props();

  const snapshot = $derived(app.snapshot!);
  const node = $derived(
    item.nodeId ? findNode(snapshot, item.nodeId) : undefined,
  );

  // Bookmark candidates render with their change's title so picking the
  // right target is a read, not a lookup. Undrawn candidates stay inert.
  const targets = $derived(
    item.targets.map((id) => ({ id, node: findNode(snapshot, id) })),
  );

  const glyph = $derived(
    item.kind === "file" ? "×" : item.kind === "bookmark" ? "??" : "!",
  );
</script>

{#snippet body()}
  <span class="head">
    <span class="glyph mono" class:warn={item.kind === "staleWorkspace"}>
      {glyph}
    </span>
    <span class="summary">{item.summary}</span>
    {#if node}
      <span class="go mono">
        {node.id}
        <Icon name="chevronRight" size={11} />
      </span>
    {/if}
  </span>
  {#if item.paths.length > 0}
    <ul class="paths mono">
      {#each item.paths as path (path)}
        {@const parts = splitPath(path)}
        <li>
          <span class="mark">×</span>
          <span class="dir">{parts.dir}</span><span class="name">{parts.name}</span>
        </li>
      {/each}
      {#if item.morePaths > 0}
        <li class="more">+ {item.morePaths} more conflicted files</li>
      {/if}
    </ul>
  {/if}
{/snippet}

{#if item.kind === "bookmark"}
  <div class="row" data-conflict-id={item.id}>
    {@render body()}
    <span class="targets">
      <span class="targets-label">Could point to</span>
      {#each targets as target (target.id)}
        {#if target.node}
          <button
            class="target"
            title="Go to {target.id}"
            data-conflict-target={target.id}
            onclick={() => jumpToChange(target.id)}
          >
            <span class="mono">{target.id}</span>
            {#if target.node.description}
              <span class="target-title truncate">
                {target.node.description.split("\n")[0]}
              </span>
            {/if}
          </button>
        {:else}
          <span class="target inert">
            <span class="mono">{target.id}</span>
          </span>
        {/if}
      {/each}
    </span>
  </div>
{:else if node}
  <button
    class="row jump"
    data-conflict-id={item.id}
    title="Open {node.id} in the workbench"
    onclick={() => jumpToChange(node.id)}
  >
    {@render body()}
  </button>
{:else}
  <div class="row" data-conflict-id={item.id}>
    {@render body()}
  </div>
{/if}

<style>
  .row {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    width: 100%;
    text-align: left;
    padding: var(--sp-3) var(--sp-3);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-m);
    background: var(--clr-bg-2);
    margin-bottom: var(--sp-2);
    transition:
      background var(--t-fast) var(--ease-out),
      border-color var(--t-fast) var(--ease-out);
  }

  button.row.jump:hover {
    background: var(--clr-bg-hover);
    border-color: var(--clr-border-1);
  }

  .head {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }

  .glyph {
    flex-shrink: 0;
    width: 18px;
    text-align: center;
    font-size: var(--text-m);
    font-weight: 600;
    color: var(--clr-danger);
  }

  .glyph.warn {
    color: var(--clr-warn);
  }

  .summary {
    flex: 1;
    min-width: 0;
    font-size: var(--text-m);
    font-weight: 500;
    color: var(--clr-text-1);
  }

  .go {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    transition: color var(--t-fast) var(--ease-out);
  }

  button.row.jump:hover .go {
    color: var(--clr-accent-strong);
  }

  .paths {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-left: calc(18px + var(--sp-2));
    font-size: var(--text-s);
  }

  .paths li {
    display: flex;
    align-items: baseline;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .mark {
    flex-shrink: 0;
    margin-right: var(--sp-2);
    color: color-mix(in srgb, var(--clr-danger) 70%, transparent);
  }

  .dir {
    color: var(--clr-text-3);
  }

  .name {
    color: var(--clr-text-1);
  }

  .more {
    color: var(--clr-text-3);
    font-style: italic;
  }

  .targets {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
    padding-left: calc(18px + var(--sp-2));
  }

  .targets-label {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .target {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    max-width: 24em;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 2px 10px;
    background: var(--clr-bg-3);
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-2);
    transition:
      background var(--t-fast) var(--ease-out),
      border-color var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  button.target:hover {
    background: var(--clr-accent-dim);
    border-color: color-mix(in srgb, var(--clr-accent) 40%, transparent);
    color: var(--clr-accent-strong);
  }

  .target.inert {
    opacity: 0.7;
  }

  .target-title {
    min-width: 0;
  }
</style>
