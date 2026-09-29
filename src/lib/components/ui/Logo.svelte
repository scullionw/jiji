<script lang="ts">
  // The Jiji mark: a change node and the rail it hangs from, drawn as a
  // "j". Plain ink by default; `tile` sets it on a small solid plate for
  // places that need an app-icon-like anchor (the welcome screen).
  let { size = 32, tile = true }: { size?: number; tile?: boolean } = $props();
</script>

<span class="logo" class:tile style:--size="{size}px" aria-hidden="true">
  <svg viewBox="0 0 24 24" width={size * 0.62} height={size * 0.62} fill="none">
    <circle class="node" cx="14.5" cy="5.2" r="2.6" />
    <path
      class="rail"
      d="M14.5 10.4V15.6a4.2 4.2 0 0 1-4.2 4.2H7.2"
      stroke-width="2.6"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
    <circle class="node dim" cx="5.5" cy="12" r="1.7" />
  </svg>
</span>

<style>
  .logo {
    display: inline-grid;
    place-items: center;
    width: var(--size);
    height: var(--size);
    flex-shrink: 0;
    color: var(--clr-text-1);
  }

  .logo.tile {
    border-radius: calc(var(--size) * 0.24);
    background: var(--clr-bg-3);
    border: 1px solid var(--clr-border-1);
    color: var(--clr-accent-strong);
  }

  .node {
    fill: currentColor;
  }

  .node.dim {
    opacity: 0.45;
  }

  .rail {
    stroke: currentColor;
  }
</style>
