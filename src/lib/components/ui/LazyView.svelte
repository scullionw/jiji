<script lang="ts">
  import type { Component } from "svelte";

  let { load, label }: {
    load: () => Promise<{ default: Component }>;
    label: string;
  } = $props();
  let attempt = $state(0);
  const pending = $derived.by(() => { void attempt; return load(); });
</script>

{#await pending}
  <div class="loading" role="status">Loading {label.toLowerCase()}…</div>
{:then module}
  <module.default />
{:catch}
  <div class="loading" role="alert">
    <p>Couldn’t open {label.toLowerCase()}.</p>
    <button onclick={() => attempt++}>Try again</button>
  </div>
{/await}

<style>
  .loading {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-3);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }
  button { color: var(--clr-accent-strong); text-decoration: underline; }
</style>
