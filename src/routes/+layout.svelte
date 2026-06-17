<script lang="ts">
  import "$lib/styles/tokens.css";
  import "$lib/styles/global.css";
  import { onMount } from "svelte";
  import { initTheme, refreshTheme } from "$lib/state/theme.svelte";
  import { license, loadLicenseState } from "$lib/license/state.svelte";

  let { children } = $props();

  onMount(() => {
    initTheme();
    void loadLicenseState();
  });

  // Re-apply the theme whenever registration flips: activating a key unlocks
  // the saved palette; a revalidation that finds a refunded/revoked license
  // clamps back to the default light theme.
  let prevRegistered = license.registered;
  $effect(() => {
    if (license.registered !== prevRegistered) {
      prevRegistered = license.registered;
      refreshTheme();
    }
  });
</script>

{@render children()}
