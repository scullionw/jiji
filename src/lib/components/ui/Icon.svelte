<script lang="ts">
  import { icons, type IconName } from "./icons";

  let { name, size = 16 }: { name: IconName; size?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.8"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  <!-- eslint-disable-next-line svelte/no-at-html-tags — static app-owned path data -->
  {@html icons[name]}
</svg>

<style>
  svg {
    flex-shrink: 0;
    display: block;
  }
</style>
