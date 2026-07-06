<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { IconName } from "./icons";

  let {
    icon,
    title,
    description,
  }: { icon: IconName; title: string; description: string } = $props();
</script>

<!-- The section hero: every full-page view opens with the same anchor —
     a tinted glyph tile, a real title, and the one-paragraph promise of
     what this surface does. -->
<header class="sec-head">
  <span class="tile"><Icon name={icon} size={16} /></span>
  <div class="text">
    <h2>{title}</h2>
    <p>{description}</p>
  </div>
</header>

<style>
  .sec-head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    margin-bottom: var(--sp-6);
  }

  .tile {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    margin-top: 2px;
    border-radius: var(--radius-m);
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
  }

  .text {
    min-width: 0;
  }

  h2 {
    font-size: var(--text-title);
    font-weight: 650;
    letter-spacing: -0.01em;
    color: var(--clr-text-1);
  }

  p {
    margin-top: 3px;
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-2);
    max-width: 56em;
  }
</style>
