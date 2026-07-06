<script lang="ts">
  import { tick } from "svelte";
  import { fade, fly } from "svelte/transition";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { motionMs } from "$lib/motion";
  import { app } from "$lib/state/app.svelte";
  import { license } from "$lib/license/state.svelte";
  import { selectTheme, setMode, themes } from "$lib/state/theme.svelte";
  import {
    chooseRepo,
    closePalette,
    editChange,
    goToSection,
    jumpToChange,
    newChange,
    openRepoAt,
    refreshSnapshot,
    runQuiet,
    sendIntent,
    undoLastMutation,
  } from "$lib/state/actions";
  import { fetchUpstreamNow } from "$lib/state/upstream.svelte";
  import { findNode } from "$lib/components/inspector/inspect";
  import { paletteResults, type PaletteItem } from "./palette";

  let query = $state("");
  let active = $state(0);
  let inputEl = $state<HTMLInputElement | undefined>();
  let listEl = $state<HTMLDivElement | undefined>();

  const selected = $derived(
    app.snapshot && app.selectedNodeId
      ? (findNode(app.snapshot, app.selectedNodeId) ?? null)
      : null,
  );
  const items = $derived(
    paletteResults(
      {
        snapshot: app.snapshot,
        selected,
        recentRepos: app.recentRepos,
        canUndo: app.lastMutation?.outcome.operationId != null,
        registered: license.registered,
        themes,
      },
      query,
    ),
  );
  // Group headers only make sense for the browsable default list; query
  // results are one flat ranked list.
  const grouped = $derived(query.trim() === "");

  $effect(() => {
    void query;
    active = 0;
  });
  $effect(() => {
    if (active >= items.length) active = Math.max(0, items.length - 1);
  });

  // Keep the active row in view under keyboard travel.
  $effect(() => {
    void active;
    tick().then(() => {
      listEl
        ?.querySelector(".palette-row.active")
        ?.scrollIntoView({ block: "nearest" });
    });
  });

  $effect(() => {
    inputEl?.focus();
  });

  function run(item: PaletteItem) {
    closePalette();
    const action = item.action;
    switch (action.type) {
      case "chooseRepo":
        void chooseRepo();
        break;
      case "openRecent":
        void openRepoAt(action.path);
        break;
      case "refresh":
        void refreshSnapshot();
        break;
      case "fetchUpstream":
        void fetchUpstreamNow();
        break;
      case "undo":
        void undoLastMutation();
        break;
      case "newChild":
        void runQuiet(() => newChange(action.id));
        break;
      case "edit":
        void runQuiet(() => editChange(action.id));
        break;
      case "intent":
        sendIntent(action.intent);
        break;
      case "section":
        goToSection(action.section);
        break;
      case "goto":
        jumpToChange(action.id);
        break;
      case "mode":
        setMode(action.mode);
        break;
      case "theme":
        selectTheme(action.id);
        break;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      active = Math.min(items.length - 1, Math.max(0, active + delta));
    } else if (event.key === "Enter") {
      event.preventDefault();
      const item = items[active];
      if (item) run(item);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closePalette();
    } else if (event.key === "Tab") {
      // The palette is a single control; focus stays on the input.
      event.preventDefault();
    }
  }
</script>

<!-- Intro-only motion: closing is instant, so a run command's surface
     (a panel opening, a jumped selection) appears without a lag. -->
<div
  class="overlay"
  role="presentation"
  in:fade={{ duration: motionMs(110) }}
  onpointerdown={(event) => {
    if (event.target === event.currentTarget) closePalette();
  }}
>
  <div
    class="palette"
    role="dialog"
    aria-label="Command palette"
    tabindex="-1"
    in:fly={{ y: -8, duration: motionMs(140) }}
    onkeydown={onKeydown}
  >
    <div class="input-row">
      <Icon name="search" size={14} />
      <input
        bind:this={inputEl}
        bind:value={query}
        class="palette-input"
        placeholder={app.snapshot
          ? "Type a command, change id, or bookmark…"
          : "Type a command…"}
        spellcheck="false"
        aria-label="Search commands"
      />
      <kbd class="hint-key">esc</kbd>
    </div>
    <div class="list" bind:this={listEl} role="listbox" aria-label="Commands">
      {#each items as item, index (item.id)}
        {#if grouped && (index === 0 || items[index - 1].group !== item.group)}
          <span class="group-label">{item.group}</span>
        {/if}
        <button
          class="palette-row"
          class:active={index === active}
          class:danger={item.danger}
          role="option"
          aria-selected={index === active}
          data-command={item.id}
          onpointerenter={() => (active = index)}
          onclick={() => run(item)}
        >
          {#if item.glyph}
            <span class="glyph mono {item.glyphTone ?? ''}">{item.glyph}</span>
          {:else if item.icon}
            <span class="row-icon"><Icon name={item.icon} size={13} /></span>
          {/if}
          <span class="title truncate" class:mono={item.glyph !== undefined}>
            {item.title}
          </span>
          {#if item.hint}
            <span class="hint truncate">{item.hint}</span>
          {/if}
          {#if item.shortcut}
            <kbd class="hint-key">{item.shortcut}</kbd>
          {/if}
        </button>
      {:else}
        <span class="empty">No matching command</span>
      {/each}
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
    padding-top: 11vh;
    background: color-mix(in srgb, var(--clr-bg-0) 45%, transparent);
  }

  .palette {
    width: min(560px, calc(100vw - 96px));
    max-height: min(430px, 72vh);
    display: flex;
    flex-direction: column;
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-l);
    box-shadow: var(--shadow-2);
    overflow: hidden;
  }

  .input-row {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-3);
    border-bottom: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
  }

  .palette-input {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    outline: none;
    font: inherit;
    font-size: var(--text-m);
    color: var(--clr-text-1);
  }

  .palette-input::placeholder {
    color: var(--clr-text-3);
  }

  .hint-key {
    flex-shrink: 0;
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-s);
    padding: 1px 5px;
  }

  .list {
    overflow-y: auto;
    padding: var(--sp-2);
    display: flex;
    flex-direction: column;
  }

  .group-label {
    padding: var(--sp-2) var(--sp-2) 3px;
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--clr-text-3);
  }

  .palette-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-width: 0;
    text-align: left;
    padding: 5px var(--sp-2);
    border-radius: var(--radius-s);
    color: var(--clr-text-2);
    font-size: var(--text-s);
  }

  .palette-row.active {
    background: var(--clr-accent-dim);
    color: var(--clr-text-1);
  }

  .row-icon {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 16px;
    color: var(--clr-text-3);
  }

  .palette-row.active .row-icon {
    color: var(--clr-accent-strong);
  }

  .palette-row.danger .row-icon {
    color: color-mix(in srgb, var(--clr-danger) 75%, var(--clr-text-3));
  }

  .palette-row.danger.active {
    background: color-mix(in srgb, var(--clr-danger) 12%, transparent);
  }

  .palette-row.danger.active .row-icon {
    color: var(--clr-danger);
  }

  .glyph {
    flex-shrink: 0;
    width: 16px;
    text-align: center;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .glyph.workingCopy {
    color: var(--clr-working-copy);
  }

  .glyph.mutable {
    color: var(--clr-accent);
  }

  .title {
    flex-shrink: 0;
    max-width: 60%;
  }

  .hint {
    flex: 1;
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .palette-row .hint-key {
    margin-left: auto;
  }

  .empty {
    padding: var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    text-align: center;
  }
</style>
