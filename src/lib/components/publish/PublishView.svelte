<script lang="ts">
  import { app } from "$lib/state/app.svelte";
  import { publishSnapshot, errorMessage } from "$lib/api";
  import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
  import PublishWorkspace from "./PublishWorkspace.svelte";

  // Raw like app.snapshot: replaced wholesale, never deep-proxied.
  let complete = $state.raw<RepoSnapshot | null>(null);
  let error = $state<string | null>(null);
  const needsSnapshot = $derived(
    app.snapshot != null &&
      !["workstreams", "team", "recent"].includes(app.snapshot.view.kind),
  );
  const snapshot = $derived(needsSnapshot ? complete : app.snapshot);
  $effect(() => {
    const current = app.snapshot;
    if (!needsSnapshot || !current) return;
    let cancelled = false;
    error = null;
    publishSnapshot()
      .then((answer) => {
        if (!cancelled) complete = answer;
      })
      .catch((err) => {
        if (!cancelled) error = errorMessage(err);
      });
    return () => {
      cancelled = true;
    };
  });
</script>

{#if snapshot && snapshot.repoPath === app.snapshot?.repoPath}
  {#key snapshot.repoPath}
    <PublishWorkspace {snapshot} />
  {/key}
{:else}
  <div class="loading" role="status">{error ?? "Reading your stacks…"}</div>
{/if}

<style>
  .loading {
    padding: 48px;
    color: var(--clr-text-2);
  }
</style>
