<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import {
    theme,
    themes,
    setMode,
    selectTheme,
    type ThemeMode,
  } from "$lib/state/theme.svelte";
  import { license, openRegistration } from "$lib/license/state.svelte";

  let open = $state(false);
  let anchor: HTMLDivElement | undefined = $state();

  const modes: { id: ThemeMode; label: string }[] = [
    { id: "system", label: "System" },
    { id: "light", label: "Light" },
    { id: "dark", label: "Dark" },
  ];

  const icon = $derived(
    theme.mode === "system" ? "sunMoon" : theme.mode === "light" ? "sun" : "moon",
  );

  const groups = $derived([
    {
      label: "Dark",
      list: themes.filter((t) => t.scheme === "dark"),
      current: theme.dark,
    },
    {
      label: "Light",
      list: themes.filter((t) => t.scheme === "light"),
      current: theme.light,
    },
  ]);

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
    title="Theme"
    aria-label="Theme"
    aria-expanded={open}
    onclick={() => (open = !open)}
  >
    <Icon name={icon} size={15} />
  </button>

  {#if open}
    <div class="panel popover" role="dialog" aria-label="Theme">
      {#if license.registered}
        <div class="segmented modes">
          {#each modes as mode (mode.id)}
            <button
              class="mode"
              class:active={theme.mode === mode.id}
              onclick={() => setMode(mode.id)}
            >
              {mode.label}
            </button>
          {/each}
        </div>

        {#each groups as group (group.label)}
          <div class="menu-label">{group.label}</div>
          <div class="swatches">
            {#each group.list as t (t.id)}
              <button
                class="swatch-item"
                class:selected={group.current === t.id}
                title={t.label}
                onclick={() => selectTheme(t.id)}
              >
                <span class="swatch" style:background={t.swatch.bg}>
                  <span class="bar" style:background={t.swatch.accent}></span>
                </span>
                <span class="swatch-name truncate">{t.label}</span>
              </button>
            {/each}
          </div>
        {/each}
      {:else}
        <div class="locked">
          <span class="locked-icon"><Icon name="lock" size={16} /></span>
          <p class="locked-title">Themes come with a license</p>
          <p class="locked-copy">
            Jiji ships in ten palettes. A one-time license unlocks them all and
            keeps development going.
          </p>
          <button
            class="btn primary unlock"
            onclick={() => {
              open = false;
              openRegistration();
            }}
          >
            Unlock themes
          </button>
        </div>
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
    width: 292px;
    padding: var(--sp-2);
  }

  .modes {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    width: 100%;
  }

  .swatches {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: 2px;
  }

  .swatch-item {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 5px;
    min-width: 0;
    padding: 6px 2px 5px;
    border-radius: var(--radius-m);
    transition: background var(--t-fast) var(--ease-out);
  }

  .swatch-item:hover {
    background: var(--clr-bg-hover);
  }

  /* A miniature of the theme: its pane color with its accent along the
     bottom edge, the way a selection sits in a list. */
  .swatch {
    position: relative;
    width: 40px;
    height: 26px;
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-s);
    overflow: hidden;
  }

  .bar {
    position: absolute;
    left: 5px;
    right: 5px;
    bottom: 6px;
    height: 4px;
    border-radius: 2px;
  }

  .swatch-item.selected .swatch {
    outline: 2px solid var(--clr-accent);
    outline-offset: 1px;
  }

  .swatch-name {
    max-width: 100%;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .swatch-item.selected .swatch-name {
    color: var(--clr-text-1);
  }

  .locked {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    padding: var(--sp-2);
  }

  .locked-icon {
    color: var(--clr-text-3);
    margin-bottom: var(--sp-2);
  }

  .locked-title {
    font-size: var(--text-m);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .locked-copy {
    margin-top: 4px;
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-2);
  }

  .unlock {
    margin-top: var(--sp-3);
    width: 100%;
  }
</style>
