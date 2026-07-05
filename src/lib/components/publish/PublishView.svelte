<script lang="ts">
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type { TokenSource } from "$lib/bindings/TokenSource";
  import { errorMessage } from "$lib/api";
  import Button from "$lib/components/ui/Button.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import {
    connectForge,
    disconnectForge,
    forge,
  } from "$lib/state/forge.svelte";

  // The forge connection: which GitHub repo this jj repo publishes to and
  // whose token Jiji would act with. The state itself is shared (synced at
  // the shell whenever the repo's remotes change) — this surface renders
  // it and hosts the connect/disconnect flow. Submit and landing build on
  // it in the following slices.

  let token = $state("");
  let busy = $state(false);
  // Connect/disconnect refusals render here; connection-level failures
  // from the shared sync arrive via forge.error.
  let actionError = $state<string | null>(null);

  const sourceLabel: Record<TokenSource, string> = {
    keychain: "stored in your keychain",
    environment: "from GITHUB_TOKEN",
    ghCli: "from the gh CLI",
  };

  async function connect() {
    if (!token.trim() || busy) return;
    busy = true;
    actionError = null;
    try {
      await connectForge(token);
      token = "";
    } catch (err) {
      actionError = errorMessage(err);
    }
    busy = false;
  }

  async function disconnect() {
    if (busy) return;
    busy = true;
    actionError = null;
    try {
      await disconnectForge();
    } catch (err) {
      actionError = errorMessage(err);
    }
    busy = false;
  }

  const phase = $derived(forge.phase === "idle" ? "loading" : forge.phase);
  const error = $derived(actionError ?? forge.error);
  const repo = $derived(forge.status?.repo ?? null);
  const auth = $derived(forge.status?.auth ?? null);
  const connected = $derived(auth?.source != null && auth?.login != null);
  const openPrs = $derived(forge.prs?.report.prs.length ?? 0);
</script>

<div class="view">
  <div class="column">
    <header class="head">
      <h2>Publish &amp; review</h2>
      <p>
        Where your stacks meet GitHub: this connection is what stack
        submission, PR state on the graph, and landing will act through.
      </p>
    </header>

    <section class="group">
      <div class="group-head">
        <span class="group-label">Repository</span>
      </div>
      {#if repo}
        <div class="card" data-forge-repo>
          <span class="repo mono">{repo.owner}/{repo.name}</span>
          <span class="meta">
            via {repo.remote}{repo.host === "github.com"
              ? ""
              : ` on ${repo.host}`}
          </span>
        </div>
      {:else}
        <p class="blurb">
          No GitHub remote detected on this repository. Add one with
          <span class="mono">jj git remote add origin &lt;url&gt;</span> and
          publishing lights up.
        </p>
      {/if}
    </section>

    <section class="group">
      <div class="group-head">
        <span class="group-label">Connection</span>
      </div>

      {#if phase === "loading"}
        <p class="blurb quiet" data-forge-state="loading">
          Checking the GitHub connection…
        </p>
      {:else if phase === "verifying"}
        <p class="blurb quiet" data-forge-state="verifying">
          Verifying the token with GitHub…
        </p>
      {:else if connected && auth}
        <div class="card" data-forge-state="connected">
          <span class="who">
            <Icon name="publish" size={14} />
            Connected as <strong>@{auth.login}</strong>
          </span>
          <span class="meta">token {sourceLabel[auth.source!]}</span>
          <span class="spacer"></span>
          {#if auth.source === "keychain"}
            <Button variant="secondary" disabled={busy} onclick={disconnect}>
              Disconnect
            </Button>
          {:else}
            <span class="meta managed">managed outside Jiji</span>
          {/if}
        </div>
      {:else}
        <div class="connect" data-forge-state="disconnected">
          <p class="blurb">
            Connect with a GitHub personal access token. Jiji validates it,
            then keeps it in your system keychain — never on disk. Already
            signed into the <span class="mono">gh</span> CLI or exporting
            <span class="mono">GITHUB_TOKEN</span>? That works with no setup.
          </p>
          <form
            class="token-row"
            onsubmit={(event) => {
              event.preventDefault();
              connect();
            }}
          >
            <input
              class="token-input mono"
              type="password"
              placeholder="ghp_… or github_pat_…"
              autocomplete="off"
              spellcheck="false"
              bind:value={token}
              disabled={busy}
              data-forge-token
            />
            <Button variant="primary" disabled={busy || !token.trim()}>
              {busy ? "Connecting…" : "Connect"}
            </Button>
          </form>
          <button
            class="link"
            onclick={() => openUrl("https://github.com/settings/tokens")}
          >
            Create a token on GitHub ↗
          </button>
        </div>
      {/if}

      {#if error}
        <p class="error" data-forge-error>{error}</p>
      {/if}
    </section>

    {#if repo && connected}
      <section class="group">
        <div class="group-head">
          <span class="group-label">Pull requests</span>
        </div>
        {#if forge.prsLoading && !forge.prs}
          <p class="blurb quiet" data-forge-prs="loading">
            Fetching open pull requests…
          </p>
        {:else if forge.prsError}
          <p class="error" data-forge-prs="error">{forge.prsError}</p>
        {:else if forge.prs}
          <p class="blurb" data-forge-prs="ready">
            {openPrs === 0
              ? "No open pull requests"
              : `${openPrs} open pull request${openPrs === 1 ? "" : "s"}`}
            on <span class="mono">{repo.owner}/{repo.name}</span>.
            {#if forge.prs.report.truncated}
              Only the 100 most recently updated are tracked, so badges may
              miss older ones.
            {:else if openPrs > 0}
              Ones matching a bookmark wear their badge on the workbench
              graph.
            {/if}
          </p>
        {/if}
      </section>
    {/if}

    <span class="hint">stack submission arrives next</span>
  </div>
</div>

<style>
  .view {
    height: 100%;
    overflow-y: auto;
  }

  .column {
    max-width: 760px;
    margin-inline: auto;
    padding: var(--sp-6) var(--sp-6) var(--sp-8);
  }

  .head {
    margin-bottom: var(--sp-5);
  }

  .head h2 {
    font-size: var(--text-l);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .head p {
    margin-top: var(--sp-1);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    max-width: 52em;
  }

  .group {
    margin-bottom: var(--sp-5);
  }

  .group-head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) 0;
  }

  .group-head::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--clr-border-2);
  }

  .group-label {
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--clr-text-3);
  }

  .card {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-4);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-m);
    min-height: 46px;
  }

  .repo {
    font-size: var(--text-m);
    color: var(--clr-text-1);
    font-weight: 550;
  }

  .who {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-m);
    color: var(--clr-text-1);
  }

  .who strong {
    font-weight: 600;
  }

  .meta {
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .managed {
    font-style: italic;
  }

  .spacer {
    flex: 1;
  }

  .blurb {
    font-size: var(--text-s);
    color: var(--clr-text-2);
    max-width: 58em;
  }

  .quiet {
    color: var(--clr-text-3);
  }

  .connect .blurb {
    margin-bottom: var(--sp-3);
  }

  .token-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  .token-input {
    flex: 1;
    min-width: 0;
    max-width: 28em;
    height: 30px;
    padding: 3px var(--sp-3);
    font-size: var(--text-s);
    color: var(--clr-text-1);
    background: var(--clr-bg-1);
    border: 1px solid var(--clr-border-1);
    border-radius: 999px;
    transition: border-color var(--t-fast) var(--ease-out);
  }

  .token-input:focus {
    outline: none;
    border-color: var(--clr-accent-strong);
  }

  .token-input:disabled {
    opacity: 0.6;
  }

  .link {
    margin-top: var(--sp-2);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    text-align: left;
    padding: 0;
  }

  .link:hover {
    color: var(--clr-accent-strong);
    cursor: pointer;
  }

  .error {
    margin-top: var(--sp-3);
    font-size: var(--text-s);
    color: var(--clr-danger);
  }

  .hint {
    display: inline-block;
    margin-top: var(--sp-2);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    padding: 3px 10px;
  }
</style>
