<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import {
    theme,
    themes,
    setMode,
    selectTheme,
    type ThemeMode,
  } from "$lib/state/theme.svelte";

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
    <div class="panel" role="dialog" aria-label="Theme">
      <div class="modes">
        {#each modes as mode (mode.id)}
          <button
            class="mode"
            class:selected={theme.mode === mode.id}
            onclick={() => setMode(mode.id)}
          >
            {mode.label}
          </button>
        {/each}
      </div>

      {#each groups as group (group.label)}
        <div class="group-label">{group.label}</div>
        <div class="swatches">
          {#each group.list as t (t.id)}
            <button
              class="swatch-item"
              class:selected={group.current === t.id}
              title={t.label}
              onclick={() => selectTheme(t.id)}
            >
              <span class="swatch" style:background={t.swatch.bg}>
                <span class="dot" style:background={t.swatch.accent}></span>
              </span>
              <span class="swatch-name truncate">{t.label}</span>
            </button>
          {/each}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .anchor {
    position: relative;
  }

  .icon-btn {
    width: 30px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-m);
    color: var(--clr-text-2);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .icon-btn:hover,
  .icon-btn.open {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .panel {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 50;
    width: 300px;
    padding: var(--sp-3);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-1);
    border-radius: var(--radius-l);
    box-shadow: var(--shadow-2);
    animation: pop var(--t-fast) var(--ease-out);
  }

  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(-4px) scale(0.98);
    }
  }

  .modes {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 2px;
    padding: 2px;
    background: var(--clr-bg-0);
    border-radius: var(--radius-m);
  }

  .mode {
    height: 24px;
    border-radius: 5px;
    font-size: var(--text-s);
    font-weight: 500;
    color: var(--clr-text-2);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .mode:hover {
    color: var(--clr-text-1);
  }

  .mode.selected {
    background: var(--clr-bg-3);
    color: var(--clr-text-1);
    box-shadow: var(--shadow-1);
  }

  .group-label {
    margin: var(--sp-3) 0 var(--sp-1);
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--clr-text-3);
  }

  .swatches {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: var(--sp-1);
  }

  .swatch-item {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    min-width: 0;
    padding: 5px 2px;
    border-radius: var(--radius-m);
    transition: background var(--t-fast) var(--ease-out);
  }

  .swatch-item:hover {
    background: var(--clr-bg-hover);
  }

  .swatch {
    width: 38px;
    height: 26px;
    display: grid;
    place-items: center;
    border: 1px solid var(--clr-border-1);
    border-radius: 6px;
  }

  .swatch-item.selected .swatch {
    outline: 2px solid var(--clr-accent);
    outline-offset: 1px;
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }

  .swatch-name {
    max-width: 100%;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .swatch-item.selected .swatch-name {
    color: var(--clr-text-1);
    font-weight: 500;
  }
</style>
