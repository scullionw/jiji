<script lang="ts">
  // The drag gesture's plan card: what is being dragged and what releasing
  // right here would do, following the pointer. This is the plan step of
  // the mutation shape — the drop is the confirmation, so the card has to
  // carry the same consequences the rebase panel states before its ⌘↵.
  import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
  import { descendantsOf, findNode } from "$lib/components/inspector/inspect";
  import { drag } from "./dnd.svelte";

  let { snapshot }: { snapshot: RepoSnapshot } = $props();

  const source = $derived(
    drag.active ? findNode(snapshot, drag.sourceId) : undefined,
  );
  const sourceTitle = $derived(
    source?.description.split("\n")[0] || "no description",
  );
  const descendantCount = $derived(
    drag.active ? descendantsOf(snapshot, drag.sourceId).length : 0,
  );
  // The scope chip states what travels before a target is even hovered;
  // over a target the plan's op is authoritative (forced-alone included).
  const alone = $derived(
    drag.plan?.allowed ? drag.plan.op === "move" : drag.alone,
  );
  const scope = $derived(
    descendantCount === 0
      ? null
      : alone
        ? "alone"
        : `+${descendantCount} descendant${descendantCount === 1 ? "" : "s"}`,
  );

  const KIND_GLYPH = { workingCopy: "@", mutable: "○", immutable: "◆" } as const;

  // Follow the pointer, clamped so the card never leaves the viewport.
  const left = $derived(Math.min(drag.x + 14, window.innerWidth - 336));
  const top = $derived(Math.min(drag.y + 18, window.innerHeight - 220));
</script>

{#if drag.active && source}
  <div class="drag-card" style:left="{left}px" style:top="{top}px">
    <div class="held">
      <span class="glyph mono {source.kind}">{KIND_GLYPH[source.kind]}</span>
      <span class="id mono"><b>{source.id.slice(0, 2)}</b>{source.id.slice(2, 8)}</span>
      <span class="title truncate">{sourceTitle}</span>
      {#if scope}
        <span class="scope" class:alone>{scope}</span>
      {/if}
    </div>
    {#if drag.plan?.allowed}
      <p class="plan">{drag.plan.summary}</p>
      {#if drag.plan.consequences.length > 0}
        <ul class="consequences">
          {#each drag.plan.consequences as line (line)}
            <li>{line}</li>
          {/each}
        </ul>
      {/if}
    {:else if drag.plan}
      <p class="plan refused">{drag.plan.reason}</p>
    {:else}
      <p class="plan neutral">Drop onto the change that becomes the new parent</p>
    {/if}
    <p class="hints">
      {#if descendantCount > 0}
        <span class="hint"
          ><span class="mono">⌥</span>
          {drag.alone ? "brings descendants back" : "moves only this change"}</span
        >
        <span class="dot-sep">·</span>
      {/if}
      <span class="hint"><span class="mono">esc</span> cancels</span>
      <span class="dot-sep">·</span>
      <span class="hint">undo lives in Operations</span>
    </p>
  </div>
{/if}

<style>
  /* The card follows the pointer and must never swallow it, or
     elementFromPoint would hit the card instead of the row below. */
  .drag-card {
    position: fixed;
    z-index: 90;
    width: 320px;
    pointer-events: none;
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-l);
    box-shadow: var(--shadow-2);
    padding: var(--sp-3);
  }

  .held {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .glyph {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .glyph.workingCopy {
    color: var(--clr-working-copy);
    font-weight: 700;
  }

  .id {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .id b {
    color: var(--clr-accent-strong);
    font-weight: 600;
  }

  .title {
    min-width: 0;
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .scope {
    flex-shrink: 0;
    margin-left: auto;
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 1px 7px;
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
  }

  .scope.alone {
    background: color-mix(in srgb, var(--clr-warn) 14%, transparent);
    color: var(--clr-warn);
  }

  .plan {
    margin-top: var(--sp-2);
    padding-top: var(--sp-2);
    border-top: 1px solid var(--clr-border-2);
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-accent-strong);
  }

  .plan.refused {
    color: var(--clr-danger);
  }

  .plan.neutral {
    color: var(--clr-text-3);
    font-weight: 400;
  }

  .consequences {
    margin: var(--sp-1) 0 0;
    padding-left: 16px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .consequences li::marker {
    color: var(--clr-text-3);
  }

  .hints {
    display: flex;
    flex-wrap: wrap;
    column-gap: 5px;
    row-gap: 2px;
    margin-top: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .hint {
    white-space: nowrap;
  }

  .hints .mono {
    background: var(--clr-bg-3);
    border-radius: var(--radius-s);
    padding: 0 4px;
  }

  .dot-sep {
    margin: 0 2px;
  }

  /* The whole app drags: rows keep their grabbing cursor and nothing is
     text-selectable while a row is in hand. */
  :global(body.row-dragging),
  :global(body.row-dragging *) {
    cursor: grabbing !important;
    user-select: none;
  }
</style>
