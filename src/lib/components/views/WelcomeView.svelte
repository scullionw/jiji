<script lang="ts">
  import Button from "$lib/components/ui/Button.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { app } from "$lib/state/app.svelte";
  import { chooseRepo, openRepoAt } from "$lib/state/actions";
  import { fromNow } from "$lib/time";
</script>

<div class="welcome">
  <div class="hero">
    <div class="mark mono">jj</div>
    <h1>Jiji</h1>
    <p class="tagline">A JJ-native desktop workbench</p>
    <div class="cta">
      <Button variant="primary" onclick={chooseRepo} disabled={app.opening}>
        {app.opening ? "Opening…" : "Open repository"}
      </Button>
      <span class="kbd">⌘O</span>
    </div>
    {#if app.error}
      <p class="error">{app.error}</p>
    {/if}
  </div>

  {#if app.recentRepos.length > 0}
    <div class="recents">
      <h2>Recent repositories</h2>
      {#each app.recentRepos as repo (repo.path)}
        <button class="recent-row" onclick={() => openRepoAt(repo.path)}>
          <Icon name="folder" size={14} />
          <span class="name">{repo.name}</span>
          <span class="path truncate" title={repo.path}>{repo.path}</span>
          <span class="when">{fromNow(repo.lastOpenedAt)}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .welcome {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-8);
    overflow-y: auto;
    padding: var(--sp-8);
  }

  .hero {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-3);
  }

  .mark {
    width: 56px;
    height: 56px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-xl);
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
    font-size: var(--text-xl);
    font-weight: 700;
    letter-spacing: -1px;
    margin-bottom: var(--sp-1);
  }

  h1 {
    font-size: 26px;
    font-weight: 650;
    letter-spacing: -0.4px;
  }

  .tagline {
    color: var(--clr-text-2);
    margin-bottom: var(--sp-2);
  }

  .cta {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .error {
    margin-top: var(--sp-2);
    font-size: var(--text-s);
    color: var(--clr-danger);
    max-width: 420px;
    text-align: center;
  }

  .recents {
    width: 440px;
    max-width: 100%;
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
    padding: var(--sp-2);
  }

  h2 {
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--clr-text-3);
    padding: var(--sp-2) var(--sp-2) var(--sp-2);
  }

  .recent-row {
    width: 100%;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    text-align: left;
    transition: background var(--t-fast) var(--ease-out);
  }

  .recent-row:hover {
    background: var(--clr-bg-hover);
  }

  .name {
    font-weight: 550;
    color: var(--clr-text-1);
    flex-shrink: 0;
  }

  .path {
    flex: 1;
    font-size: var(--text-s);
  }

  .when {
    font-size: var(--text-xs);
    flex-shrink: 0;
  }
</style>
