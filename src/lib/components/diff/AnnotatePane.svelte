<script lang="ts">
  // One file annotated line by line (`jj file annotate`): every line wears
  // the change that introduced it, grouped into runs. Read-only — the one
  // verb is the origin gutter itself, which jumps the graph to that
  // change (revealing it under a transient revset when the current lens
  // does not draw it). Replaces the diff surface while open; any
  // selection move — including that jump — closes it.

  import Icon from "$lib/components/ui/Icon.svelte";
  import type { FileAnnotation } from "$lib/bindings/FileAnnotation";
  import * as api from "$lib/api";
  import { revealChange, runQuiet } from "$lib/state/actions";
  import { fromNow, shortAge } from "$lib/time";

  let {
    changeId,
    path,
    opId,
    onclose,
  }: {
    changeId: string;
    path: string;
    /// Current operation id; a new operation refetches (the change may
    /// have been rewritten under the pane).
    opId: string;
    onclose: () => void;
  } = $props();

  // Raw: a whole file's lines, replaced per fetch, never edited.
  let annotation = $state.raw<FileAnnotation | null>(null);
  let error = $state<string | null>(null);
  let requestSeq = 0;

  $effect(() => {
    const id = changeId;
    const file = path;
    void opId; // refetch when a new operation lands
    const token = ++requestSeq;
    api.fileAnnotate(id, file).then(
      (result) => {
        if (token !== requestSeq) return;
        annotation = result;
        error = null;
      },
      (err) => {
        if (token !== requestSeq) return;
        annotation = null;
        error = api.errorMessage(err);
      },
    );
  });

  // Only an answer for the shown subject renders; a stale one is dropped
  // by the token check on arrival and by this guard while in flight.
  const view = $derived(
    annotation && annotation.id === changeId && annotation.path === path
      ? annotation
      : null,
  );

  // Hovering any line tints its whole origin's run — the fastest way to
  // read "what else came from this change" in a multi-author file.
  let hoverOrigin = $state<number | null>(null);

  function originTitle(index: number): string {
    const origin = view?.origins[index];
    if (!origin) return "";
    const head = `${origin.changeId} · ${origin.author} · ${fromNow(origin.timestamp)}`;
    return origin.summary
      ? `${head}\n${origin.summary}\nClick to jump to this change`
      : `${head}\nClick to jump to this change`;
  }

  function jump(index: number): void {
    const origin = view?.origins[index];
    if (origin) void runQuiet(() => revealChange(origin.id));
  }
</script>

<section class="annotate" aria-label="File annotation">
  <header class="head">
    <Icon name="eye" size={13} />
    <span class="title">Annotate</span>
    <span class="path mono truncate" title={path}>{path}</span>
    <span class="chip mono" title="The file as it stands in this change's tree">
      in <b>{changeId.slice(0, 2)}</b>{changeId.slice(2, 8)}
    </span>
    <button class="head-close" title="Back to the diff (esc)" onclick={onclose}>
      <Icon name="close" size={13} />
    </button>
  </header>
  <!-- The hover handlers only drive the same-origin tint; the interactive
       element is the origin gutter's button. -->
  <div
    class="body"
    role="presentation"
    onpointerleave={() => (hoverOrigin = null)}
  >
    {#if view}
      {#if view.lines.length === 0}
        <p class="note">This file is empty in {changeId.slice(0, 8)}.</p>
      {:else}
        {#each view.lines as line, i (i)}
          {@const runStart = i === 0 || view.lines[i - 1].origin !== line.origin}
          {@const origin = view.origins[line.origin]}
          <div
            class="row"
            class:run-start={runStart}
            class:tinted={hoverOrigin === line.origin}
            role="presentation"
            onpointerenter={() => (hoverOrigin = line.origin)}
          >
            <button
              class="origin"
              tabindex={runStart ? 0 : -1}
              title={originTitle(line.origin)}
              onclick={() => jump(line.origin)}
            >
              {#if runStart}
                <span class="origin-id mono">
                  <b>{origin.id.slice(0, 2)}</b>{origin.id.slice(2, 8)}
                </span>
                <span class="origin-author truncate">{origin.author}</span>
                <span class="origin-age mono">{shortAge(origin.timestamp)}</span>
              {/if}
            </button>
            <span class="num mono">{i + 1}</span>
            <span class="text mono">{line.text}</span>
          </div>
        {/each}
        {#if view.truncated}
          <p class="note">
            Annotation truncated — the file has more lines than Jiji renders
          </p>
        {/if}
      {/if}
    {:else if error}
      <p class="error" role="alert">{error}</p>
    {:else}
      <div class="skeleton" aria-hidden="true">
        <i></i><i></i><i></i><i></i><i></i>
      </div>
    {/if}
  </div>
</section>

<style>
  .annotate {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .head {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 34px;
    padding: 0 var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
  }

  .title {
    flex-shrink: 0;
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .path {
    flex: 0 1 auto;
    min-width: 0;
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .chip {
    flex-shrink: 0;
    height: 17px;
    line-height: 17px;
    font-size: var(--text-xs);
    padding: 0 5px;
    border-radius: var(--radius-s);
    background: color-mix(in srgb, var(--clr-text-1) 7%, transparent);
    color: var(--clr-text-2);
  }

  .chip b {
    color: var(--clr-text-1);
    font-weight: 600;
  }

  .head-close {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    margin-left: auto;
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .head-close:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding-bottom: var(--sp-6);
  }

  /* One line per row: the origin gutter, the line number, the code. Rows
     stretch to the widest line so the run hairlines and hover tint span
     the full scroll width. */
  .row {
    display: flex;
    align-items: stretch;
    width: max-content;
    min-width: 100%;
  }

  /* A hairline where a new origin's run begins — the annotation's actual
     information is the boundaries. */
  .row.run-start:not(:first-child) {
    border-top: 1px solid var(--clr-border-2);
  }

  /* The gutter stays put while long lines scroll horizontally; opaque so
     code slides under it. */
  .origin {
    position: sticky;
    left: 0;
    z-index: 1;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 224px;
    padding: 0 var(--sp-2) 0 var(--sp-3);
    text-align: left;
    background: var(--clr-bg-1);
    border-right: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
    cursor: pointer;
  }

  .origin-id {
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-2);
  }

  .origin-id b {
    color: var(--clr-accent-strong);
    font-weight: 600;
  }

  .origin-author {
    flex: 1;
    min-width: 0;
    font-size: var(--text-xs);
  }

  .origin-age {
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .row:hover .origin,
  .origin:focus-visible {
    color: var(--clr-text-1);
  }

  .row:hover .origin .origin-id,
  .origin:focus-visible .origin-id {
    color: var(--clr-accent-strong);
  }

  .num {
    flex-shrink: 0;
    width: 40px;
    padding-right: var(--sp-2);
    text-align: right;
    font-size: var(--text-xs);
    line-height: var(--lh-code);
    color: var(--clr-text-3);
    opacity: 0.6;
    user-select: none;
  }

  .text {
    flex: 1;
    padding-right: var(--sp-4);
    white-space: pre;
    font-size: var(--text-code);
    line-height: var(--lh-code);
    color: var(--clr-text-1);
  }

  /* The hovered origin's whole run tints, sticky gutter included. */
  .row.tinted,
  .row.tinted .origin {
    background: color-mix(in srgb, var(--clr-accent) 7%, var(--clr-bg-1));
  }

  .note {
    padding: var(--sp-3) var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    font-style: italic;
  }

  .error {
    padding: var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-warn);
  }

  .skeleton {
    display: flex;
    flex-direction: column;
    gap: 9px;
    padding: var(--sp-4);
  }

  .skeleton i {
    height: 12px;
    border-radius: 4px;
    background: var(--clr-bg-3);
    animation: pulse 1.1s ease-in-out infinite alternate;
  }

  .skeleton i:nth-child(2) {
    width: 86%;
    animation-delay: 100ms;
  }

  .skeleton i:nth-child(3) {
    width: 64%;
    animation-delay: 200ms;
  }

  .skeleton i:nth-child(4) {
    width: 78%;
    animation-delay: 300ms;
  }

  .skeleton i:nth-child(5) {
    width: 42%;
    animation-delay: 400ms;
  }

  @keyframes pulse {
    from {
      opacity: 0.45;
    }
    to {
      opacity: 1;
    }
  }
</style>
