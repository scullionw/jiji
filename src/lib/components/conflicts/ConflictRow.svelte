<script lang="ts">
  import type { ConflictItem } from "$lib/bindings/ConflictItem";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { findNode, splitPath } from "$lib/components/inspector/inspect";
  import { errorMessage } from "$lib/api";
  import {
    jumpToChange,
    resolveConflict,
    updateStaleWorkspace,
  } from "$lib/state/actions";
  import { app } from "$lib/state/app.svelte";
  import { canRecoverWorkspace, canResolve, mergeToolLabel } from "./conflicts";

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

  // Resolve hands the file to the external merge tool. One tool window at a
  // time: while any resolve is in flight (from this card or the diff
  // surface), every button waits.
  const toolLabel = $derived(
    snapshot.resolveTool ? mergeToolLabel(snapshot.resolveTool) : null,
  );
  const resolvable = $derived(
    item.kind === "file" && canResolve(snapshot, item.nodeId),
  );
  const resolving = $derived(app.resolvingConflict);

  let resolveError = $state<string | null>(null);

  async function resolve(path: string) {
    if (!node) return;
    resolveError = null;
    try {
      await resolveConflict(node.id, path);
    } catch (error) {
      resolveError = errorMessage(error);
    }
  }

  // The stale-workspace recovery (`jj workspace update-stale`) only reaches
  // the workspace Jiji has open; sibling cards explain the CLI route.
  const recoverable = $derived(canRecoverWorkspace(snapshot, item));
  let updating = $state(false);
  let updateError = $state<string | null>(null);

  async function updateWorkspace() {
    updateError = null;
    updating = true;
    try {
      await updateStaleWorkspace();
    } catch (error) {
      updateError = errorMessage(error);
    } finally {
      updating = false;
    }
  }
</script>

{#snippet head()}
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
{/snippet}

{#if item.kind === "file"}
  <div class="row" data-conflict-id={item.id}>
    {#if node}
      <button
        class="head-jump"
        title="Open {node.id} in the workbench"
        onclick={() => jumpToChange(node.id)}
      >
        {@render head()}
      </button>
    {:else}
      {@render head()}
    {/if}
    <ul class="paths mono">
      {#each item.paths as path (path)}
        {@const parts = splitPath(path)}
        {@const waiting =
          resolving?.changeId === node?.id && resolving?.path === path}
        <li>
          <span class="mark">×</span>
          <span class="pathname truncate">
            <span class="dir">{parts.dir}</span><span class="name">{parts.name}</span>
          </span>
          {#if resolvable && toolLabel}
            <button
              class="resolve"
              class:waiting
              data-resolve-path={path}
              disabled={resolving !== null}
              title="Open the 3-way merge in {toolLabel} and record the saved result"
              onclick={() => resolve(path)}
            >
              {waiting ? `Waiting for ${toolLabel}…` : `Resolve in ${toolLabel}`}
            </button>
          {/if}
        </li>
      {/each}
      {#if item.morePaths > 0}
        <li class="more">+ {item.morePaths} more conflicted files</li>
      {/if}
    </ul>
    {#if resolveError}
      <p class="resolve-error">{resolveError}</p>
    {/if}
  </div>
{:else if item.kind === "bookmark"}
  <div class="row" data-conflict-id={item.id}>
    {@render head()}
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
{:else}
  <div class="row" data-conflict-id={item.id}>
    {#if node}
      <button
        class="head-jump"
        title="Open {node.id} in the workbench"
        onclick={() => jumpToChange(node.id)}
      >
        {@render head()}
      </button>
    {:else}
      {@render head()}
    {/if}
    {#if recoverable}
      <div class="recover">
        <span class="note">
          Local edits are saved first, then the working copy catches up to
          where the repo moved.
        </span>
        <button
          class="resolve"
          data-update-workspace
          disabled={updating}
          title="Record on-disk edits, then check out the repo's current position (jj workspace update-stale)"
          onclick={updateWorkspace}
        >
          {updating ? "Updating…" : "Update workspace"}
        </button>
      </div>
    {:else}
      <p class="note aside">
        Jiji can only update the workspace it has open — run
        <code>jj workspace update-stale</code> inside this one.
      </p>
    {/if}
    {#if updateError}
      <p class="resolve-error">{updateError}</p>
    {/if}
  </div>
{/if}

<style>
  .row {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    width: 100%;
    text-align: left;
    padding: var(--sp-3) var(--sp-4);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
    background: var(--clr-bg-2);
    box-shadow: var(--shadow-1);
    margin-bottom: var(--sp-3);
    transition:
      background var(--t-fast) var(--ease-out),
      border-color var(--t-fast) var(--ease-out);
  }

  .row:hover {
    border-color: var(--clr-border-1);
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  /* The head line stays the jump into the workbench when the card also
     carries per-path actions. */
  .head-jump {
    width: 100%;
    text-align: left;
    border-radius: var(--radius-s);
  }

  .head-jump:hover .go {
    color: var(--clr-accent-strong);
  }

  .head-jump:hover .summary {
    color: var(--clr-accent-strong);
  }

  .glyph {
    flex-shrink: 0;
    align-self: center;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-s);
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-danger);
    background: color-mix(in srgb, var(--clr-danger) 12%, transparent);
  }

  .glyph.warn {
    color: var(--clr-warn);
    background: color-mix(in srgb, var(--clr-warn) 12%, transparent);
  }

  .summary {
    flex: 1;
    min-width: 0;
    font-size: var(--text-m);
    font-weight: 500;
    color: var(--clr-text-1);
    transition: color var(--t-fast) var(--ease-out);
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

  .paths {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding-left: calc(22px + var(--sp-2));
    font-size: var(--text-s);
  }

  .paths li {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    min-height: 24px;
  }

  .mark {
    flex-shrink: 0;
    color: color-mix(in srgb, var(--clr-danger) 70%, transparent);
  }

  .pathname {
    min-width: 0;
    white-space: nowrap;
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

  .resolve {
    flex-shrink: 0;
    margin-left: auto;
    font-size: var(--text-xs);
    font-weight: 500;
    font-family: var(--font-ui);
    padding: 2px 10px;
    border-radius: 999px;
    border: 1px solid transparent;
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
    transition:
      background var(--t-fast) var(--ease-out),
      border-color var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .resolve:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--clr-accent) 45%, transparent);
  }

  .resolve:disabled {
    opacity: 0.55;
  }

  .resolve.waiting {
    opacity: 1;
    color: var(--clr-accent-strong);
    border-color: color-mix(in srgb, var(--clr-accent) 40%, transparent);
    background: var(--clr-accent-dim);
  }

  .resolve-error {
    margin: 0;
    padding-left: calc(22px + var(--sp-2));
    font-size: var(--text-xs);
    color: var(--clr-danger);
  }

  .recover {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding-left: calc(22px + var(--sp-2));
    min-height: 24px;
  }

  .note {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .recover .note {
    flex: 1;
    min-width: 0;
  }

  .note.aside {
    margin: 0;
    padding-left: calc(22px + var(--sp-2));
  }

  .note code {
    font-family: var(--font-mono);
    font-size: 0.95em;
    color: var(--clr-text-2);
  }

  .targets {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
    padding-left: calc(22px + var(--sp-2));
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
