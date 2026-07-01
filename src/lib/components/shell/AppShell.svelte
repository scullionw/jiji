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
  import CommandPalette from "$lib/components/palette/CommandPalette.svelte";
  import { app, type Section } from "$lib/state/app.svelte";
  import {
    bootstrap,
    chooseRepo,
    refreshSnapshot,
    togglePalette,
  } from "$lib/state/actions";
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

  onMount(() => {
    bootstrap();
    void checkForAppUpdate();

    return tinykeys(window, {
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
  });
</script>

<div class="shell">
  <Sidebar />
  <div class="main">
    <TopBar />
    <div class="content">
      {#if !app.snapshot}
        <WelcomeView />
      {:else if app.section === "workbench"}
        <WorkbenchView />
      {:else if app.section === "operations"}
        <OperationsView />
      {:else}
        <PlaceholderView section={app.section} />
      {/if}
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
</style>
