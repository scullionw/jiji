<script lang="ts">
  import Button from "$lib/components/ui/Button.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import Logo from "$lib/components/ui/Logo.svelte";
  import { app } from "$lib/state/app.svelte";
  import {
    chooseRepo,
    forgetRecentRepo,
    openRepoAt,
    runQuiet,
    togglePalette,
  } from "$lib/state/actions";
  import { cli, dismissCliOffer, installCli } from "$lib/state/cli.svelte";
  import { fromNow } from "$lib/time";

  // The one-time offer to install the terminal launcher. A foreign `jiji`
  // on PATH means they already have a CLI by that name — no offer. After
  // an install from this card, `justInstalled` keeps it up as the
  // confirmation of where the command landed.
  const cliOffer = $derived(
    cli.status !== null &&
      !cli.dismissed &&
      cli.status.conflict === null &&
      (!cli.status.installed || cli.justInstalled),
  );

  function shortPath(path: string): string {
    return path.replace(/^\/Users\/[^/]+/, "~");
  }
</script>

<div class="welcome">
  <div class="stage">
    <header class="head">
      <Logo size={40} />
      <div class="head-text">
        <h1>Jiji</h1>
        <p class="tagline">A workbench for <span class="mono">jj</span> repositories</p>
      </div>
    </header>

    <div class="cta">
      <Button variant="primary" onclick={chooseRepo} disabled={app.opening}>
        <Icon name="folder" size={14} />
        {app.opening ? "Opening…" : "Open Repository…"}
      </Button>
      <button class="btn palette-btn" onclick={togglePalette}>
        Command Palette
        <span class="kbd">⌘K</span>
      </button>
    </div>
    {#if app.error}
      <p class="error">{app.error}</p>
    {:else}
      <p class="hint">A plain Git repository works too: Jiji offers to set jj up in it.</p>
    {/if}

    {#if app.recentRepos.length > 0}
      <section class="recents">
        <h2>Recent</h2>
        <div class="list">
          {#each app.recentRepos as repo (repo.path)}
            <div class="recent">
              <button class="recent-row" onclick={() => openRepoAt(repo.path)}>
                <span class="name truncate">{repo.name}</span>
                <span class="path truncate" title={repo.path}>{shortPath(repo.path)}</span>
                <span class="when">{fromNow(repo.lastOpenedAt)}</span>
              </button>
              <button
                class="forget"
                title="Remove from recent repositories"
                aria-label="Remove {repo.name} from recent repositories"
                onclick={() => void forgetRecentRepo(repo.path)}
              >
                <Icon name="close" size={11} />
              </button>
            </div>
          {/each}
        </div>
      </section>
    {/if}

    {#if cliOffer && cli.status}
      <div class="cli-card">
        {#if cli.status.installed}
          <span class="cli-icon done"><Icon name="check" size={15} /></span>
          <div class="cli-text">
            <span class="cli-title">The <code>jiji</code> command is ready</span>
            <span class="cli-body">
              Installed at {cli.status.path} — run <code>jiji</code> inside any
              repository to open it here.
            </span>
          </div>
        {:else}
          <span class="cli-icon"><Icon name="terminal" size={15} /></span>
          <div class="cli-text">
            <span class="cli-title">Open repos from your terminal</span>
            <span class="cli-body">
              Install the <code>jiji</code> command — running <code>jiji</code>
              in a repository opens it here.
            </span>
          </div>
          <div class="cli-actions">
            <Button variant="ghost" onclick={dismissCliOffer}>Not now</Button>
            <Button
              onclick={() => void runQuiet(installCli)}
              disabled={cli.installing}
            >
              {cli.installing ? "Installing…" : "Install"}
            </Button>
          </div>
        {/if}
      </div>
    {/if}

    <div class="hints">
      <span class="hint"><span class="kbd">⌘O</span> Open</span>
      <span class="hint"><span class="kbd">⌘T</span> New tab</span>
      <span class="hint"><span class="kbd">⌘⇧T</span> Reopen closed tab</span>
    </div>
  </div>
</div>

<style>
  .welcome {
    height: 100%;
    overflow-y: auto;
  }

  .stage {
    width: 560px;
    max-width: 100%;
    min-height: 100%;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: var(--sp-5);
    padding: var(--sp-8) var(--sp-6) 12vh;
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .head-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  h1 {
    font-size: var(--text-xl);
    font-weight: 600;
    line-height: 1.2;
    color: var(--clr-text-1);
  }

  .tagline {
    font-size: var(--text-m);
    color: var(--clr-text-2);
  }

  .tagline .mono {
    font-size: var(--text-s);
    color: var(--clr-text-1);
  }

  .cta {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  .cta :global(.btn) {
    height: 30px;
    padding: 0 12px;
    font-size: var(--text-m);
  }

  .palette-btn .kbd {
    margin-right: -4px;
  }

  .error {
    font-size: var(--text-s);
    color: var(--clr-danger);
  }

  .hint {
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  h2 {
    padding: 0 0 6px;
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-2);
  }

  .list {
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--clr-border-2);
  }

  .recent {
    position: relative;
    display: flex;
    align-items: center;
    border-bottom: 1px solid var(--clr-border-2);
    transition: background var(--t-fast) var(--ease-out);
  }

  .recent:hover {
    background: var(--clr-bg-hover);
  }

  .recent-row {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 34px;
    padding: 0 var(--sp-2);
    text-align: left;
  }

  .name {
    flex-shrink: 0;
    max-width: 40%;
    font-size: var(--text-m);
    font-weight: 500;
    color: var(--clr-text-1);
  }

  .path {
    flex: 1;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .when {
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  /* Like a tab's close: keeps its footprint, surfaces on hover or focus. */
  .forget {
    flex-shrink: 0;
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    margin-right: var(--sp-2);
    border-radius: var(--radius-s);
    color: var(--clr-text-3);
    opacity: 0;
    transition:
      opacity var(--t-fast) var(--ease-out),
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .recent:hover .forget,
  .forget:focus-visible {
    opacity: 1;
  }

  .forget:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .cli-card {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
    padding: var(--sp-3);
  }

  .cli-icon {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-m);
    background: var(--clr-bg-hover);
    color: var(--clr-text-2);
  }

  .cli-icon.done {
    color: var(--clr-ok);
    background: var(--clr-ok-dim);
  }

  .cli-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .cli-title {
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .cli-body {
    font-size: var(--text-xs);
    line-height: 1.45;
    color: var(--clr-text-3);
  }

  .cli-card code {
    font-family: var(--font-mono);
    color: var(--clr-text-2);
  }

  .cli-actions {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }

  .hints {
    display: flex;
    align-items: center;
    gap: var(--sp-5);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .hint {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
</style>
