<script lang="ts">
  import "$lib/styles/tokens.css";
  import "$lib/styles/global.css";
  import { onMount } from "svelte";
  import { initTheme } from "$lib/state/theme.svelte";

  let { children } = $props();

  onMount(initTheme);
</script>

{@render children()}
