<script lang="ts">
  import { onMount } from "svelte";
  import { fade } from "svelte/transition";
  import { tinykeys } from "tinykeys";
  import Sidebar from "./Sidebar.svelte";
  import TopBar from "./TopBar.svelte";
  import StatusBar from "./StatusBar.svelte";
  import WelcomeView from "$lib/components/views/WelcomeView.svelte";
  import WorkbenchView from "$lib/components/views/WorkbenchView.svelte";
  import PlaceholderView from "$lib/components/views/PlaceholderView.svelte";
  import LazyView from "$lib/components/ui/LazyView.svelte";
  import CommandPalette from "$lib/components/palette/CommandPalette.svelte";
  import SearchOverlay from "$lib/components/search/SearchOverlay.svelte";
  import ContextMenu from "$lib/components/contextmenu/ContextMenu.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { app, type Section } from "$lib/state/app.svelte";
  import { tabs } from "$lib/state/tabs.svelte";
  import {
    bootstrap,
    chooseRepo,
    closeActiveTab,
    cycleTab,
    newTab,
    openDroppedPaths,
    openSearch,
    refreshSnapshot,
    reopenClosedTab,
    sendIntent,
    toggleGraphPanel,
    togglePalette,
    toggleSidebar,
  } from "$lib/state/actions";
  import { syncForgeConnection } from "$lib/state/forge.svelte";
  import { motionMs } from "$lib/motion";
  import {
    startUpstreamChecks,
    syncUpstreamRepo,
  } from "$lib/state/upstream.svelte";
  import { checkForAppUpdateNow, startAppUpdateChecks } from "$lib/state/update.svelte";
  import {
    onFullscreenChange,
    onMenuAction,
    onPathDrop,
    setWindowTitle,
  } from "$lib/api";

  const sectionOrder: Section[] = [
    "workbench",
    "conflicts",
    "publish",
    "bookmarks",
    "operations",
    "workspaces",
  ];

  const loadConflicts = () => import("$lib/components/conflicts/ConflictsView.svelte");
  const loadPublish = () => import("$lib/components/publish/PublishView.svelte");
  const loadBookmarks = () => import("$lib/components/bookmarks/BookmarksView.svelte");
  const loadOperations = () => import("$lib/components/ops/OperationsView.svelte");

  function goTo(index: number) {
    if (app.snapshot) app.section = sectionOrder[index];
  }

  // The native menu bar's items (menu.rs) land here by id and reuse the
  // exact actions the in-webview shortcuts call.
  const menuActions: Record<string, () => void> = {
    "new-tab": newTab,
    "open-repo": () => void chooseRepo(),
    "close-tab": closeActiveTab,
    "reopen-tab": () => void reopenClosedTab(),
    palette: togglePalette,
    find: () => sendIntent({ kind: "find" }),
    search: () => openSearch(),
    sidebar: toggleSidebar,
    graph: toggleGraphPanel,
    refresh: () => void refreshSnapshot(),
    "prev-tab": () => cycleTab(-1),
    "next-tab": () => cycleTab(1),
    "check-updates": () => void checkForAppUpdateNow(),
  };

  // What a Finder drag hovering the window would open, for the drop
  // overlay's label; empty while nothing is being dragged over.
  let dragPaths = $state<string[]>([]);
  const dropLabel = $derived(
    dragPaths.length === 1
      ? `Open “${dragPaths[0].replace(/\/+$/, "").split("/").pop()}”`
      : `Open ${dragPaths.length} folders`,
  );

  function onMenu(id: string) {
    const section = /^section-(\d)$/.exec(id);
    if (section) return goTo(Number(section[1]));
    menuActions[id]?.();
  }

  // The forge connection follows the open repo. This refires on every
  // published snapshot, but the sync keys on the remotes' JSON and no-ops
  // while they are unchanged — auto-refresh ticks cost nothing.
  $effect(() => {
    void syncForgeConnection(app.snapshot?.gitRemotes ?? null);
  });

  // The upstream check follows the open repo the same way (keyed on the
  // repo path): opening a repo with remotes fetches once, then the
  // background cadence keeps remote state fresh.
  $effect(() => {
    syncUpstreamRepo(
      app.snapshot?.repoPath ?? null,
      (app.snapshot?.gitRemotes.length ?? 0) > 0,
    );
  });

  // The window's name in Mission Control, the Dock, and ⌘-Tab follows the
  // repo on screen (the overlay title bar itself never shows it). Derived
  // first, so a refresh that leaves the name alone sends no window call.
  const windowTitle = $derived(
    app.snapshot ? `${app.snapshot.repoName} — Jiji` : "Jiji",
  );
  $effect(() => {
    setWindowTitle(windowTitle);
  });

  onMount(() => {
    bootstrap();
    const stopUpdateChecks = startAppUpdateChecks();
    const stopUpstream = startUpstreamChecks();
    const unlistenMenu = onMenuAction(onMenu);
    // Native fullscreen hides the traffic lights; flag the root so the
    // title bar can drop its clearance (TopBar.svelte).
    const unlistenFullscreen = onFullscreenChange((fullscreen) => {
      document.documentElement.toggleAttribute("data-fullscreen", fullscreen);
    });
    const unlistenDrop = onPathDrop(
      (paths) => (dragPaths = paths),
      (paths) => void openDroppedPaths(paths),
    );

    const unbind = tinykeys(window, {
      "$mod+KeyK": (event) => {
        event.preventDefault();
        togglePalette();
      },
      "$mod+KeyO": (event) => {
        event.preventDefault();
        chooseRepo();
      },
      // ⌘F finds within the diff on screen; ⌘⇧F searches all of history.
      // ⌘F typed while writing text (a describe editor) is left alone —
      // except in the find bar's own input, which handles it itself.
      "$mod+KeyF": (event) => {
        if (!app.snapshot) return;
        const target = event.target;
        if (
          target instanceof Element &&
          target.closest("input, textarea, [contenteditable]")
        ) {
          return;
        }
        event.preventDefault();
        sendIntent({ kind: "find" });
      },
      "$mod+Shift+KeyF": (event) => {
        if (!app.snapshot) return;
        event.preventDefault();
        openSearch();
      },
      "$mod+KeyR": (event) => {
        event.preventDefault();
        refreshSnapshot();
      },
      "$mod+KeyB": (event) => {
        event.preventDefault();
        toggleSidebar();
      },
      // ⌘⇧B pairs with ⌘B: the graph pane joins the sidebar out of the
      // way, leaving the diff the whole window.
      "$mod+Shift+KeyB": (event) => {
        event.preventDefault();
        toggleGraphPanel();
      },
      // Tabs, like every browser: ⌘T new, ⌘W close, ⌘⇧T reopen the last
      // closed, ⌃Tab / ⌘⇧] cycle.
      "$mod+KeyT": (event) => {
        event.preventDefault();
        newTab();
      },
      "$mod+Shift+KeyT": (event) => {
        event.preventDefault();
        void reopenClosedTab();
      },
      "$mod+KeyW": (event) => {
        event.preventDefault();
        closeActiveTab();
      },
      "Control+Tab": (event) => {
        event.preventDefault();
        cycleTab(1);
      },
      "Control+Shift+Tab": (event) => {
        event.preventDefault();
        cycleTab(-1);
      },
      "$mod+Shift+BracketRight": (event) => {
        event.preventDefault();
        cycleTab(1);
      },
      "$mod+Shift+BracketLeft": (event) => {
        event.preventDefault();
        cycleTab(-1);
      },
      "$mod+Digit1": (event) => {
        event.preventDefault();
        goTo(0);
      },
      "$mod+Digit2": (event) => {
        event.preventDefault();
        goTo(1);
      },
      "$mod+Digit3": (event) => {
        event.preventDefault();
        goTo(2);
      },
      "$mod+Digit4": (event) => {
        event.preventDefault();
        goTo(3);
      },
      "$mod+Digit5": (event) => {
        event.preventDefault();
        goTo(4);
      },
      "$mod+Digit6": (event) => {
        event.preventDefault();
        goTo(5);
      },
    });
    return () => {
      stopUpdateChecks();
      stopUpstream();
      unbind();
      void unlistenMenu.then((stop) => stop());
      void unlistenFullscreen.then((stop) => stop());
      void unlistenDrop.then((stop) => stop());
    };
  });
</script>

<div class="shell">
  <TopBar />
  <div class="body">
    <Sidebar />
    <main class="content">
      <!-- Keyed on the tab and section so no component-local state (open
           panels, scroll) leaks from one repo's view into another's. The
           swap itself is instant: switching is a keystroke, not a scene. -->
      {#key `${tabs.activeId}:${app.snapshot ? app.section : "welcome"}`}
        <div class="section">
          <!-- A render error stays inside the view it broke: the frame,
               tabs and palette keep working, and the view says so instead
               of silently freezing on stale content. -->
          <svelte:boundary onerror={(error) => console.error("view failed to render", error)}>
            {#if !app.snapshot}
              <WelcomeView />
            {:else if app.section === "workbench"}
              <WorkbenchView />
            {:else if app.section === "conflicts"}
              <LazyView load={loadConflicts} label="Conflicts" />
            {:else if app.section === "publish"}
              <LazyView load={loadPublish} label="Publish" />
            {:else if app.section === "bookmarks"}
              <LazyView load={loadBookmarks} label="Bookmarks" />
            {:else if app.section === "operations"}
              <LazyView load={loadOperations} label="Operations" />
            {:else}
              <PlaceholderView section={app.section} />
            {/if}
            {#snippet failed(error, reset)}
              <div class="view-error" role="alert">
                <h3>This view hit an error</h3>
                <p class="mono selectable">
                  {error instanceof Error ? error.message : String(error)}
                </p>
                <button class="btn" onclick={reset}>Try again</button>
              </div>
            {/snippet}
          </svelte:boundary>
        </div>
      {/key}
    </main>
  </div>
  <StatusBar />
  {#if app.paletteOpen}
    <CommandPalette />
  {/if}
  {#if app.searchOpen}
    <SearchOverlay />
  {/if}
  <ContextMenu />
  {#if dragPaths.length > 0}
    <!-- Finder drags open repos on drop, like the Dock icon does. The
         overlay only answers the drag; it never takes pointer events. -->
    <div class="drop-overlay" transition:fade={{ duration: motionMs(80) }}>
      <div class="drop-card popover">
        <Icon name="folder" size={16} />
        <span class="truncate">{dropLabel}</span>
      </div>
    </div>
  {/if}
</div>

<style>
  /* The frame: a title bar across the top, the section rail down the
     left, a status line along the bottom — all on bg-0 — around one
     full-bleed working area on bg-1. Hairlines are the joinery. */
  .shell {
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--clr-bg-0);
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }

  .content {
    position: relative;
    flex: 1;
    min-width: 0;
    min-height: 0;
    background: var(--clr-bg-1);
    border-top: 1px solid var(--clr-border-2);
    overflow: hidden;
  }

  .section {
    height: 100%;
  }

  .view-error {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-3);
    padding: var(--sp-8);
    text-align: center;
  }

  .view-error h3 {
    font-size: var(--text-m);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .view-error p {
    max-width: 520px;
    font-size: var(--text-xs);
    line-height: 1.55;
    color: var(--clr-text-2);
    overflow-wrap: anywhere;
  }

  .drop-overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: grid;
    place-items: center;
    pointer-events: none;
    background: color-mix(in srgb, var(--clr-bg-0) 60%, transparent);
    box-shadow: inset 0 0 0 2px var(--clr-accent);
  }

  .drop-card {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    max-width: min(420px, 70vw);
    padding: var(--sp-3) var(--sp-4);
    color: var(--clr-text-1);
    font-size: var(--text-m);
    font-weight: 500;
  }

  .drop-card :global(svg) {
    color: var(--clr-text-3);
  }
</style>
