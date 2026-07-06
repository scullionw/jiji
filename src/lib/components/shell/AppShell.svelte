<script lang="ts">
  import { onMount } from "svelte";
  import { tinykeys } from "tinykeys";
  import Sidebar from "./Sidebar.svelte";
  import TopBar from "./TopBar.svelte";
  import StatusBar from "./StatusBar.svelte";
  import WelcomeView from "$lib/components/views/WelcomeView.svelte";
  import WorkbenchView from "$lib/components/views/WorkbenchView.svelte";
  import PlaceholderView from "$lib/components/views/PlaceholderView.svelte";
  import OperationsView from "$lib/components/ops/OperationsView.svelte";
  import ConflictsView from "$lib/components/conflicts/ConflictsView.svelte";
  import PublishView from "$lib/components/publish/PublishView.svelte";
  import CommandPalette from "$lib/components/palette/CommandPalette.svelte";
  import { app, type Section } from "$lib/state/app.svelte";
  import {
    bootstrap,
    chooseRepo,
    refreshSnapshot,
    togglePalette,
  } from "$lib/state/actions";
  import { syncForgeConnection } from "$lib/state/forge.svelte";
  import { viewIn } from "$lib/motion";
  import {
    startUpstreamChecks,
    syncUpstreamRepo,
  } from "$lib/state/upstream.svelte";
  import { checkForAppUpdate } from "$lib/update";

  const sectionOrder: Section[] = [
    "workbench",
    "conflicts",
    "publish",
    "operations",
    "workspaces",
  ];

  function goTo(index: number) {
    if (app.snapshot) app.section = sectionOrder[index];
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

  onMount(() => {
    bootstrap();
    void checkForAppUpdate();
    const stopUpstream = startUpstreamChecks();

    const unbind = tinykeys(window, {
      "$mod+KeyK": (event) => {
        event.preventDefault();
        togglePalette();
      },
      "$mod+KeyO": (event) => {
        event.preventDefault();
        chooseRepo();
      },
      "$mod+KeyR": (event) => {
        event.preventDefault();
        refreshSnapshot();
      },
      "$mod+Digit1": () => goTo(0),
      "$mod+Digit2": () => goTo(1),
      "$mod+Digit3": () => goTo(2),
      "$mod+Digit4": () => goTo(3),
      "$mod+Digit5": () => goTo(4),
    });
    return () => {
      stopUpstream();
      unbind();
    };
  });
</script>

<div class="shell">
  <Sidebar />
  <div class="main">
    <TopBar />
    <div class="content">
      <!-- Keyed on the section so switching settles the new surface in with
           one quiet gesture instead of an instant swap. -->
      {#key app.snapshot ? app.section : "welcome"}
        <div class="section" in:viewIn>
          {#if !app.snapshot}
            <WelcomeView />
          {:else if app.section === "workbench"}
            <WorkbenchView />
          {:else if app.section === "conflicts"}
            <ConflictsView />
          {:else if app.section === "publish"}
            <PublishView />
          {:else if app.section === "operations"}
            <OperationsView />
          {:else}
            <PlaceholderView section={app.section} />
          {/if}
        </div>
      {/key}
    </div>
    <StatusBar />
  </div>
  {#if app.paletteOpen}
    <CommandPalette />
  {/if}
</div>

<style>
  .shell {
    height: 100vh;
    display: flex;
    background: var(--clr-bg-0);
  }

  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    padding-right: var(--sp-2);
  }

  .content {
    flex: 1;
    min-height: 0;
    background: var(--clr-bg-1);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
    overflow: hidden;
  }

  .section {
    height: 100%;
  }
</style>
