<script lang="ts">
  // App-level preferences, popover-sized: the machine integrations (which
  // editor and terminal menus launch, the jiji command) and the upstream
  // check cadence. Reading preferences (diff layout, density, view) stay
  // on the surfaces they act on; appearance keeps its own panel next door
  // (ThemeMenu).

  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import Icon from "$lib/components/ui/Icon.svelte";
  import * as api from "$lib/api";
  import { editorState, setPreferredEditor } from "$lib/state/editor.svelte";
  import {
    setPreferredTerminal,
    terminalState,
  } from "$lib/state/terminal.svelte";
  import {
    setUpstreamInterval,
    upstream,
    UPSTREAM_INTERVALS,
  } from "$lib/state/upstream.svelte";
  import { cli, installCli, uninstallCli } from "$lib/state/cli.svelte";

  let open = $state(false);
  let anchor: HTMLDivElement | undefined = $state();
  let cliError = $state<string | null>(null);

  // The running build's version, from the bundle (tauri.conf.json); empty
  // outside Tauri (vite dev in a plain browser, the visual harness).
  let version = $state("");
  onMount(() => {
    getVersion()
      .then((v) => (version = v))
      .catch(() => {});
  });

  // What "Last used" resolves to right now, shown as a quiet hint.
  const lastUsed = $derived(
    editorState.detected.find((e) => e.id === editorState.lastUsed) ??
      editorState.detected[0],
  );

  // The terminal a launch would pick today: the pin while installed,
  // else the platform default (first detected).
  const activeTerminal = $derived(
    terminalState.detected.find((t) => t.id === terminalState.preferred)?.id ??
      terminalState.detected[0]?.id,
  );

  async function runCli(action: () => Promise<void>) {
    cliError = null;
    try {
      await action();
    } catch (error) {
      cliError = api.errorMessage(error);
    }
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (open && event.key === "Escape") open = false;
  }

  function onWindowMousedown(event: MouseEvent) {
    if (open && anchor && !anchor.contains(event.target as Node)) {
      open = false;
    }
  }
</script>

<svelte:window onkeydown={onWindowKeydown} onmousedown={onWindowMousedown} />

<div class="anchor" bind:this={anchor}>
  <button
    class="icon-btn"
    class:open
    title="Settings"
    aria-label="Settings"
    aria-expanded={open}
    onclick={() => (open = !open)}
  >
    <Icon name="settings" size={15} />
  </button>

  {#if open}
    <div class="panel popover" role="dialog" aria-label="Settings">
      <div class="group">
        <div class="menu-label">Preferred editor</div>
        {#if editorState.detected.length === 0}
          <p class="empty">No editor command-line tools found on this machine.</p>
        {:else}
          <button
            class="row"
            class:selected={editorState.preferred === null}
            onclick={() => setPreferredEditor(null)}
          >
            <span class="row-label">Last used</span>
            {#if lastUsed}<span class="hint">{lastUsed.label}</span>{/if}
            <span class="tick">
              {#if editorState.preferred === null}<Icon name="check" size={13} />{/if}
            </span>
          </button>
          {#each editorState.detected as editor (editor.id)}
            <button
              class="row"
              class:selected={editorState.preferred === editor.id}
              onclick={() => setPreferredEditor(editor.id)}
            >
              <span class="row-label">{editor.label}</span>
              <span class="tick">
                {#if editorState.preferred === editor.id}<Icon name="check" size={13} />{/if}
              </span>
            </button>
          {/each}
        {/if}
      </div>

      {#if terminalState.detected.length > 0}
        <div class="group">
          <div class="menu-label">Terminal</div>
          {#each terminalState.detected as terminal (terminal.id)}
            <button
              class="row"
              class:selected={activeTerminal === terminal.id}
              onclick={() => setPreferredTerminal(terminal.id)}
            >
              <span class="row-label">{terminal.label}</span>
              <span class="tick">
                {#if activeTerminal === terminal.id}<Icon name="check" size={13} />{/if}
              </span>
            </button>
          {/each}
        </div>
      {/if}

      <div class="group">
        <div class="menu-label">Check upstream</div>
        <div class="segmented segments">
          {#each UPSTREAM_INTERVALS as interval (interval.minutes)}
            <button
              class="segment"
              class:active={upstream.intervalMinutes === interval.minutes}
              onclick={() => setUpstreamInterval(interval.minutes)}
            >
              {interval.label}
            </button>
          {/each}
        </div>
      </div>

      {#if cli.status}
        <div class="group">
          <div class="menu-label">Command line</div>
          {#if cli.status.installed}
            <div class="cli-line">
              <span class="hint truncate" title={cli.status.path}
                >Installed at {cli.status.path}</span
              >
              <button
                class="btn danger"
                onclick={() => void runCli(uninstallCli)}
              >
                Uninstall
              </button>
            </div>
          {:else if cli.status.conflict}
            <p class="empty">
              Another <span class="mono">jiji</span> occupies {cli.status.conflict}.
            </p>
          {:else}
            <button
              class="btn wide"
              disabled={cli.installing}
              onclick={() => void runCli(installCli)}
            >
              {cli.installing ? "Installing…" : "Install jiji command"}
            </button>
          {/if}
          {#if cliError}
            <p class="cli-error">{cliError}</p>
          {/if}
        </div>
      {/if}

      {#if version}
        <p class="version">Jiji <span class="mono">{version}</span></p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .anchor {
    position: relative;
  }

  .panel {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 50;
    width: 264px;
    padding: var(--sp-1) var(--sp-1) var(--sp-2);
  }

  .group + .group {
    margin-top: var(--sp-1);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-s);
    font-size: var(--text-m);
    color: var(--clr-text-1);
    transition: background var(--t-fast) var(--ease-out);
  }

  .row:hover {
    background: var(--clr-bg-hover);
  }

  .row-label {
    flex: 1;
    min-width: 0;
    text-align: left;
  }

  .hint {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .tick {
    width: 16px;
    display: grid;
    place-items: center;
    color: var(--clr-accent);
  }

  .empty {
    padding: 0 var(--sp-2);
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-3);
  }

  .segments {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    width: calc(100% - 2 * var(--sp-2));
    margin: 0 var(--sp-2);
  }

  .cli-line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 0 var(--sp-2);
    min-height: 28px;
  }

  .cli-line .hint {
    flex: 1;
    min-width: 0;
  }

  .wide {
    width: calc(100% - 2 * var(--sp-2));
    margin: 0 var(--sp-2);
  }

  .cli-error {
    margin-top: var(--sp-1);
    padding: 0 var(--sp-2);
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--clr-danger);
  }

  .version {
    margin: var(--sp-2) var(--sp-2) 0;
    padding-top: var(--sp-2);
    border-top: 1px solid var(--clr-border-2);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
