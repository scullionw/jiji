<script lang="ts" module>
  // The last query typed, for the next open: ⌘⇧F brings it back selected,
  // so re-running a search is one keystroke and a new one is just typing.
  let lastQuery = "";
</script>

<script lang="ts">
  import { tick } from "svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import type { HistorySearch } from "$lib/bindings/HistorySearch";
  import type { SearchHit } from "$lib/bindings/SearchHit";
  import * as api from "$lib/api";
  import { app } from "$lib/state/app.svelte";
  import { closeSearch, revealChange, runQuiet } from "$lib/state/actions";
  import { shortAge } from "$lib/time";
  import { fileHitLabel, MIN_QUERY_LENGTH, searchStatus } from "./search";

  let query = $state(app.searchSeed || lastQuery);
  // Raw: replaced per response, never edited.
  let result = $state.raw<HistorySearch | null>(null);
  let searching = $state(false);
  let error = $state<string | null>(null);
  let active = $state(0);
  let inputEl = $state<HTMLInputElement | undefined>();
  let listEl = $state<HTMLDivElement | undefined>();
  let requestSeq = 0;

  // Debounced search: the pickaxe walks and diffs history, so it fires on
  // a settled query, not per keystroke. Stale responses (an older request
  // resolving late) are dropped by sequence.
  $effect(() => {
    lastQuery = query;
    const trimmed = query.trim();
    const token = ++requestSeq;
    error = null;
    if (trimmed.length < MIN_QUERY_LENGTH) {
      result = null;
      searching = false;
      return;
    }
    searching = true;
    const timer = setTimeout(() => {
      api.searchHistory(trimmed).then(
        (response) => {
          if (token !== requestSeq) return;
          result = response;
          searching = false;
          active = 0;
        },
        (err) => {
          if (token !== requestSeq) return;
          error = api.errorMessage(err);
          searching = false;
        },
      );
    }, 300);
    return () => clearTimeout(timer);
  });

  const hits = $derived(result?.hits ?? []);

  $effect(() => {
    if (active >= hits.length) active = Math.max(0, hits.length - 1);
  });

  // Keep the active row in view under keyboard travel.
  $effect(() => {
    void active;
    tick().then(() => {
      listEl
        ?.querySelector(".hit-row.active")
        ?.scrollIntoView({ block: "nearest" });
    });
  });

  $effect(() => {
    inputEl?.focus();
    inputEl?.select();
  });

  function pick(hit: SearchHit) {
    closeSearch();
    void runQuiet(() => revealChange(hit.id));
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      active = Math.min(hits.length - 1, Math.max(0, active + delta));
    } else if (event.key === "Enter") {
      event.preventDefault();
      const hit = hits[active];
      if (hit) pick(hit);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeSearch();
    } else if (event.key === "Tab") {
      event.preventDefault();
    }
  }

  function title(hit: SearchHit): string {
    return hit.description.split("\n")[0] || "no description";
  }
</script>

<!-- The history search overlay (⌘⇧F): jj log -S for people. Styled like
     the command palette — same overlay, same manners — but its rows are
     backend search hits, not commands. -->
<div
  class="overlay"
  role="presentation"
  onpointerdown={(event) => {
    if (event.target === event.currentTarget) closeSearch();
  }}
>
  <div
    class="search popover"
    role="dialog"
    aria-label="Search history"
    tabindex="-1"
    onkeydown={onKeydown}
  >
    <div class="input-row">
      <Icon name="search" size={15} />
      <input
        bind:this={inputEl}
        bind:value={query}
        class="search-input"
        placeholder="Search history for text or a filename…"
        spellcheck="false"
        aria-label="Search history"
      />
      <kbd class="kbd">esc</kbd>
    </div>
    <div class="list" bind:this={listEl} role="listbox" aria-label="Matching changes">
      {#each hits as hit, index (hit.commitId)}
        <button
          class="hit-row"
          class:active={index === active}
          role="option"
          aria-selected={index === active}
          data-search-hit={hit.id}
          onpointerenter={() => (active = index)}
          onclick={() => pick(hit)}
        >
          <span class="hit-head">
            <span class="id mono">{hit.id.slice(0, 8)}</span>
            <span class="title truncate">{title(hit)}</span>
            <span class="meta truncate">{hit.author} · {shortAge(hit.timestamp)}</span>
          </span>
          <span class="hit-files">
            {#each hit.files.slice(0, 3) as file (file.path)}
              {@const label = fileHitLabel(file)}
              <span class="file mono truncate">
                {file.path}
                <span class="count {label.tone}">{label.text}</span>
              </span>
            {/each}
            {#if hit.files.length > 3 || hit.moreFiles > 0}
              <span class="more">
                +{Math.max(0, hit.files.length - 3) + hit.moreFiles} more
              </span>
            {/if}
          </span>
        </button>
      {:else}
        {#if !searching && !error}
          <span class="empty">
            {query.trim().length < MIN_QUERY_LENGTH
              ? "Find the changes that introduced or removed a string — or touched a file"
              : "No matching change"}
          </span>
        {/if}
      {/each}
      {#if error}
        <span class="empty error">{error}</span>
      {/if}
    </div>
    <div class="status" aria-live="polite">
      {#if searching}
        Searching…
      {:else if result}
        {searchStatus(result)}
      {/if}
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 90;
    display: flex;
    justify-content: center;
    align-items: flex-start;
    padding-top: 12vh;
    background: color-mix(in srgb, var(--clr-bg-0) 30%, transparent);
  }

  .search {
    width: min(700px, calc(100vw - 96px));
    max-height: min(520px, 72vh);
    display: flex;
    flex-direction: column;
    border-radius: var(--radius-xl);
    overflow: hidden;
  }

  .input-row {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 48px;
    padding: 0 var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
  }

  .search-input {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    outline: none;
    font: inherit;
    font-size: 15px;
    color: var(--clr-text-1);
  }

  .search-input::placeholder {
    color: var(--clr-text-3);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-2);
    display: flex;
    flex-direction: column;
  }

  .hit-row {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    min-width: 0;
    flex-shrink: 0;
    text-align: left;
    padding: 6px var(--sp-2);
    border-radius: var(--radius-m);
    color: var(--clr-text-1);
    font-size: var(--text-m);
  }

  .hit-row.active {
    background: color-mix(in srgb, var(--clr-text-1) 8%, transparent);
  }

  .hit-head {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }

  .id {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .title {
    flex-shrink: 1;
    min-width: 0;
  }

  .meta {
    flex-shrink: 0;
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .hit-files {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    min-width: 0;
    padding-left: calc(8ch + var(--sp-2));
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .file {
    flex: 0 1 auto;
    min-width: 0;
  }

  .count {
    margin-left: 2px;
  }

  .count.add {
    color: var(--diff-add-fg);
  }

  .count.del {
    color: var(--diff-del-fg);
  }

  .count.name {
    color: var(--clr-accent-strong);
  }

  .more {
    flex-shrink: 0;
  }

  .empty {
    padding: var(--sp-5);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    text-align: center;
  }

  .empty.error {
    color: var(--clr-warn);
  }

  .status {
    flex-shrink: 0;
    min-height: 28px;
    display: flex;
    align-items: center;
    padding: 0 var(--sp-4);
    border-top: 1px solid var(--clr-border-2);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
