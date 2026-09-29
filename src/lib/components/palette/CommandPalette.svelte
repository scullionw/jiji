<script lang="ts">
  import { tick } from "svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { app } from "$lib/state/app.svelte";
  import { license } from "$lib/license/state.svelte";
  import { selectTheme, setMode, themes } from "$lib/state/theme.svelte";
  import {
    activateTab,
    chooseRepo,
    closeActiveTab,
    closePalette,
    editChange,
    goToSection,
    jumpToChange,
    newChange,
    newTab,
    openRepoAt,
    openSearch,
    refreshSnapshot,
    reopenClosedTab,
    revertChange,
    runQuiet,
    sendIntent,
    setSnapshotView,
    toggleGraphPanel,
    toggleSidebar,
    undoLastMutation,
  } from "$lib/state/actions";
  import { lastClosedTab, tabLabel, tabTitles, tabs } from "$lib/state/tabs.svelte";
  import { setDensity } from "$lib/components/graph/density.svelte";
  import { fetchUpstreamNow } from "$lib/state/upstream.svelte";
  import {
    cli,
    installCli as installCliLauncher,
    uninstallCli as uninstallCliLauncher,
  } from "$lib/state/cli.svelte";
  import { forge } from "$lib/state/forge.svelte";
  import {
    autoland,
    startAutoLand,
    stopAutoLand,
  } from "$lib/state/autoland.svelte";
  import { findNode } from "$lib/components/inspector/inspect";
  import { visibleAutolandJobs } from "$lib/components/publish/autoland";
  import { shippableStacks } from "$lib/components/publish/ship";
  import { publishableStacks } from "$lib/components/publish/submit";
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
  // Land rows need a verified connection (badges' rule — the plan they
  // open asks GitHub); the job rows follow the record's repo like the
  // Publish job card does.
  const forgeReady = $derived(
    forge.status?.repo != null && forge.status?.auth.login != null,
  );
  const landableStacks = $derived(
    forgeReady && app.snapshot
      ? publishableStacks(
          app.snapshot,
          new Set(Object.keys(forge.prs?.byBranch ?? {})),
        )
          // Landing needs a PR behind a bookmark; derived-name stacks
          // publish first.
          .flatMap((s) =>
            s.headBookmark != null
              ? [{ bookmark: s.headBookmark, title: s.title }]
              : [],
          )
      : [],
  );
  // Ship rows need only a detected GitHub remote (the push authenticates
  // like any git push), matching the Publish group's own gate.
  const paletteShipStacks = $derived(
    forge.status?.repo != null && app.snapshot
      ? shippableStacks(app.snapshot).map((s) => ({
          head: s.headChange,
          title: s.headTitle,
          changeCount: s.changeCount,
        }))
      : [],
  );
  const autolandJobs = $derived(
    visibleAutolandJobs(autoland.jobs, app.snapshot?.repoPath),
  );
  const reopenable = $derived(lastClosedTab());
  const titles = $derived(tabTitles(tabs.all));
  const items = $derived(
    paletteResults(
      {
        snapshot: app.snapshot,
        selected,
        recentRepos: app.recentRepos,
        canUndo: app.lastMutation?.outcome.operationId != null,
        registered: license.registered,
        themes,
        landableStacks,
        shippableStacks: paletteShipStacks,
        autolandJobs,
        cli: cli.status,
        openTabs: tabs.all.map((t) => {
          const title = titles.get(t.id);
          return {
            id: t.id,
            label: title?.hint ? `${title.label} — ${title.hint}` : tabLabel(t),
            active: t.id === tabs.activeId,
          };
        }),
        reopenableTab: reopenable ? tabLabel(reopenable) : null,
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
      case "newTab":
        newTab();
        break;
      case "closeTab":
        closeActiveTab();
        break;
      case "reopenTab":
        void reopenClosedTab();
        break;
      case "switchTab":
        void activateTab(action.id);
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
      case "revert":
        void runQuiet(() => revertChange(action.id));
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
      case "searchHistory":
        openSearch(action.query ?? "");
        break;
      case "mode":
        setMode(action.mode);
        break;
      case "theme":
        selectTheme(action.id);
        break;
      case "snapshotView":
        void runQuiet(() => setSnapshotView(action.view));
        break;
      case "density":
        setDensity(action.density);
        break;
      case "toggleSidebar":
        toggleSidebar();
        break;
      case "toggleGraphPanel":
        toggleGraphPanel();
        break;
      case "autolandStop":
        void runQuiet(() => stopAutoLand());
        break;
      case "autolandResume":
        void runQuiet(() => startAutoLand(action.bookmark));
        break;
      case "installCli":
        void runQuiet(installCliLauncher);
        break;
      case "uninstallCli":
        void runQuiet(uninstallCliLauncher);
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

<!-- Opens and closes instantly: the palette is a keystroke, and whatever
     a command opens (a panel, a jumped selection) appears without a lag. -->
<div
  class="overlay"
  role="presentation"
  onpointerdown={(event) => {
    if (event.target === event.currentTarget) closePalette();
  }}
>
  <div
    class="palette popover"
    role="dialog"
    aria-label="Command palette"
    tabindex="-1"
    onkeydown={onKeydown}
  >
    <div class="input-row">
      <Icon name="search" size={15} />
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
      <kbd class="kbd">esc</kbd>
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
            <span class="row-icon"><Icon name={item.icon} size={15} /></span>
          {/if}
          <span class="title truncate" class:mono={item.glyph !== undefined}>
            {item.title}
          </span>
          {#if item.hint}
            <span class="hint truncate">{item.hint}</span>
          {/if}
          {#if item.shortcut}
            <kbd class="kbd hint-key">{item.shortcut}</kbd>
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
    padding-top: 12vh;
    background: color-mix(in srgb, var(--clr-bg-0) 30%, transparent);
  }

  .palette {
    width: min(620px, calc(100vw - 96px));
    max-height: min(480px, 72vh);
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

  .palette-input {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    outline: none;
    font: inherit;
    font-size: 15px;
    color: var(--clr-text-1);
  }

  .palette-input::placeholder {
    color: var(--clr-text-3);
  }

  .list {
    overflow-y: auto;
    padding: var(--sp-1) var(--sp-2) var(--sp-2);
    display: flex;
    flex-direction: column;
  }

  .group-label {
    padding: var(--sp-3) var(--sp-2) var(--sp-1);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--clr-text-3);
  }

  .group-label:first-child {
    padding-top: var(--sp-2);
  }

  .palette-row {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    min-width: 0;
    flex-shrink: 0;
    text-align: left;
    height: 32px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-m);
    color: var(--clr-text-1);
    font-size: var(--text-m);
  }

  .palette-row.active {
    background: color-mix(in srgb, var(--clr-text-1) 8%, transparent);
  }

  .row-icon {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 18px;
    color: var(--clr-text-3);
  }

  .palette-row.active .row-icon {
    color: var(--clr-text-1);
  }

  .palette-row.danger .row-icon,
  .palette-row.danger .title {
    color: var(--clr-danger);
  }

  .palette-row.danger.active {
    background: var(--clr-danger-dim);
  }

  .glyph {
    flex-shrink: 0;
    width: 18px;
    text-align: center;
    font-size: var(--text-s);
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

  .title.mono {
    font-size: var(--text-s);
  }

  .hint {
    flex: 1;
    min-width: 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .palette-row .hint-key {
    margin-left: auto;
  }

  .empty {
    padding: var(--sp-5);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    text-align: center;
  }
</style>
