<script lang="ts">
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type { LandOutcome } from "$lib/bindings/LandOutcome";
  import type { LandPlan } from "$lib/bindings/LandPlan";
  import type { SubmitOutcome } from "$lib/bindings/SubmitOutcome";
  import type { SubmitPlan } from "$lib/bindings/SubmitPlan";
  import type { TokenSource } from "$lib/bindings/TokenSource";
  import * as api from "$lib/api";
  import { errorMessage } from "$lib/api";
  import Button from "$lib/components/ui/Button.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { app } from "$lib/state/app.svelte";
  import {
    connectForge,
    disconnectForge,
    forge,
    refreshForgePrs,
  } from "$lib/state/forge.svelte";
  import { landActionRow, segmentChip } from "./land";
  import { actionRow, publishableStacks } from "./submit";

  // The forge connection: which GitHub repo this jj repo publishes to and
  // whose token Jiji would act with. The state itself is shared (synced at
  // the shell whenever the repo's remotes change) — this surface renders
  // it and hosts the connect/disconnect flow. On top of it sits the first
  // real workflow: publish a stack — plan (pushes, PR creations, base
  // retargets, derived Rust-side by jiji-forge), confirm, execute.

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

  // The publish-stack workflow. Planning is explicit (a click), matching
  // the deliberate fetch cadence; the plan card is the confirm step and
  // executing swaps it for the per-step results.
  const stacks = $derived(
    app.snapshot
      ? publishableStacks(
          app.snapshot,
          new Set(Object.keys(forge.prs?.byBranch ?? {})),
        )
      : [],
  );
  let planFor = $state<string | null>(null);
  let plan = $state<SubmitPlan | null>(null);
  let planLoading = $state(false);
  let planError = $state<string | null>(null);
  let publishing = $state(false);
  let outcome = $state<SubmitOutcome | null>(null);
  let planSeq = 0;

  async function loadPlan(headBookmark: string) {
    const seq = ++planSeq;
    planFor = headBookmark;
    plan = null;
    outcome = null;
    planError = null;
    planLoading = true;
    try {
      const answer = await api.submitPlan(headBookmark);
      if (seq !== planSeq) return;
      plan = answer;
    } catch (err) {
      if (seq !== planSeq) return;
      planError = errorMessage(err);
    } finally {
      if (seq === planSeq) planLoading = false;
    }
  }

  async function publish() {
    if (!plan || !planFor || publishing || plan.blockers.length > 0) return;
    publishing = true;
    planError = null;
    try {
      outcome = await api.submitStack(planFor, plan);
      // Fresh badges and PR counts follow what just landed on GitHub.
      await refreshForgePrs();
    } catch (err) {
      planError = errorMessage(err);
    }
    publishing = false;
  }

  const stepGlyph: Record<string, string> = {
    done: "✓",
    failed: "×",
    skipped: "–",
  };

  // The land-stack workflow: the same plan → confirm → execute shape as
  // publishing, over jjpr's merge → fetch → reconcile loop. Landing is one
  // merge round per run; re-running Land after GitHub finishes (checks,
  // auto-merge, the queue) is the continue flow.
  let landFor = $state<string | null>(null);
  let land = $state<LandPlan | null>(null);
  let landLoading = $state(false);
  let landError = $state<string | null>(null);
  let landing = $state(false);
  let landOutcome = $state<LandOutcome | null>(null);
  let landSeq = 0;

  async function loadLandPlan(headBookmark: string) {
    const seq = ++landSeq;
    landFor = headBookmark;
    land = null;
    landOutcome = null;
    landError = null;
    landLoading = true;
    try {
      const answer = await api.landPlan(headBookmark);
      if (seq !== landSeq) return;
      land = answer;
    } catch (err) {
      if (seq !== landSeq) return;
      landError = errorMessage(err);
    } finally {
      if (seq === landSeq) landLoading = false;
    }
  }

  async function landStack() {
    if (!land || !landFor || landing || land.blockers.length > 0) return;
    landing = true;
    landError = null;
    try {
      landOutcome = await api.landStack(landFor, land);
      // The land run's mutations republished the snapshot step by step;
      // pulling it once more makes the flow self-contained and
      // deterministic (the same posture as runMutation), and badges and
      // PR counts follow what just landed on GitHub.
      const snapshot = await api.currentSnapshot();
      if (snapshot) app.snapshot = snapshot;
      await refreshForgePrs();
    } catch (err) {
      landError = errorMessage(err);
    }
    landing = false;
  }
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

      <section class="group">
        <div class="group-head">
          <span class="group-label">Publish a stack</span>
        </div>
        {#if stacks.length === 0}
          <p class="blurb quiet" data-submit-state="no-stacks">
            No bookmarked stacks yet. Bookmark a change on the workbench and
            it becomes publishable here.
          </p>
        {:else}
          <div class="stacks">
            {#each stacks as stack (stack.workstreamId)}
              <button
                class="stack-row"
                class:picked={planFor === stack.headBookmark}
                data-submit-stack={stack.headBookmark}
                onclick={() => loadPlan(stack.headBookmark)}
              >
                <span class="stack-name mono">{stack.headBookmark}</span>
                <span class="stack-title">{stack.title}</span>
                <span class="stack-meta">
                  {stack.changeCount} change{stack.changeCount === 1
                    ? ""
                    : "s"}{stack.isActive ? " · active" : ""}
                </span>
              </button>
            {/each}
          </div>

          {#if planLoading}
            <p class="blurb quiet" data-submit-state="planning">
              Working out what publishing {planFor} needs…
            </p>
          {:else if plan && outcome}
            <div class="plan" data-submit-outcome={outcome.failed ? "failed" : "done"}>
              <p class="plan-head">
                {outcome.failed
                  ? "Publishing stopped partway — the steps below tell the story."
                  : `Published ${plan.headBookmark}.`}
              </p>
              <ul class="steps">
                {#each outcome.steps as step, index (index)}
                  <li class="step" data-step={step.status}>
                    <span class="step-glyph {step.status}">
                      {stepGlyph[step.status]}
                    </span>
                    <span class="step-text">
                      {actionRow(step.action, plan.remote).text}
                      {#if step.detail}
                        <span class="step-detail">{step.detail}</span>
                      {/if}
                    </span>
                    {#if step.pr}
                      {@const pr = step.pr}
                      <button class="pr-link" onclick={() => openUrl(pr.url)}>
                        #{pr.number} ↗
                      </button>
                    {/if}
                  </li>
                {/each}
              </ul>
            </div>
          {:else if plan}
            <div class="plan" data-submit-plan={plan.headBookmark}>
              <div class="plan-stack">
                {#each plan.segments as segment (segment.bookmark)}
                  <div class="segment" data-submit-segment={segment.bookmark}>
                    <span class="seg-bookmark mono">{segment.bookmark}</span>
                    <span class="seg-arrow">→</span>
                    <span class="seg-base mono">{segment.base}</span>
                    <span class="seg-meta">
                      {segment.changeIds.length} change{segment.changeIds
                        .length === 1
                        ? ""
                        : "s"}
                      {#if segment.pr}
                        · #{segment.pr.number} open
                      {/if}
                    </span>
                  </div>
                {/each}
              </div>

              {#if plan.actions.length === 0 && plan.blockers.length === 0}
                <p class="blurb" data-submit-state="up-to-date">
                  Everything is already on GitHub the way the stack reads
                  here — nothing to publish.
                </p>
              {:else}
                <ul class="actions">
                  {#each plan.actions as action, index (index)}
                    {@const row = actionRow(action, plan.remote)}
                    <li class="action" data-submit-action={action.kind}>
                      <span class="action-glyph {row.tone}">{row.glyph}</span>
                      <span>{row.text}</span>
                    </li>
                  {/each}
                </ul>
              {/if}

              {#if plan.stackCommentPreview}
                <details class="comment-preview" data-submit-comment-preview>
                  <summary>The stack comment those PRs will carry</summary>
                  <pre>{plan.stackCommentPreview
                    .split("\n")
                    .filter((line) => !line.startsWith("<!--"))
                    .join("\n")
                    .trim()}</pre>
                </details>
              {/if}

              {#each plan.blockers as blocker (blocker)}
                <p class="blocker" data-submit-blocker>{blocker}</p>
              {/each}
              {#each plan.warnings as warning (warning)}
                <p class="warning" data-submit-warning>{warning}</p>
              {/each}

              {#if plan.actions.length > 0}
                <div class="plan-go" data-submit-go>
                  <Button
                    variant="primary"
                    disabled={publishing || plan.blockers.length > 0}
                    onclick={publish}
                  >
                    {publishing
                      ? "Publishing…"
                      : `Publish ${plan.headBookmark}`}
                  </Button>
                  {#if plan.blockers.length > 0}
                    <span class="go-note">fix the blockers first</span>
                  {/if}
                </div>
              {/if}
            </div>
          {/if}
          {#if planError}
            <p class="error" data-submit-error>{planError}</p>
          {/if}
        {/if}
      </section>

      {#if stacks.length > 0}
        <section class="group">
          <div class="group-head">
            <span class="group-label">Land a stack</span>
          </div>
          <p class="blurb quiet">
            Merge the bottom pull request when it is ready — or hand it to
            GitHub's auto-merge or merge queue — then rebase what remains
            onto the new trunk and clean up the landed bookmark. Run Land
            again after GitHub finishes a hand-off.
          </p>
          <div class="stacks">
            {#each stacks as stack (stack.workstreamId)}
              <button
                class="stack-row"
                class:picked={landFor === stack.headBookmark}
                data-land-stack={stack.headBookmark}
                onclick={() => loadLandPlan(stack.headBookmark)}
              >
                <span class="stack-name mono">{stack.headBookmark}</span>
                <span class="stack-title">{stack.title}</span>
                <span class="stack-meta">
                  {stack.changeCount} change{stack.changeCount === 1
                    ? ""
                    : "s"}{stack.isActive ? " · active" : ""}
                </span>
              </button>
            {/each}
          </div>

          {#if landLoading}
            <p class="blurb quiet" data-land-state="planning">
              Working out what landing {landFor} needs…
            </p>
          {:else if land && landOutcome}
            <div
              class="plan"
              data-land-outcome={landOutcome.failed ? "failed" : "done"}
            >
              <p class="plan-head">
                {landOutcome.failed
                  ? "Landing stopped partway — the steps below tell the story."
                  : `Landed ${land.headBookmark}'s round.`}
              </p>
              <ul class="steps">
                {#each landOutcome.steps as step, index (index)}
                  <li class="step" data-land-step={step.status}>
                    <span class="step-glyph {step.status}">
                      {stepGlyph[step.status]}
                    </span>
                    <span class="step-text">
                      {landActionRow(step.action, land.remote).text}
                      {#if step.detail}
                        <span class="step-detail">{step.detail}</span>
                      {/if}
                    </span>
                  </li>
                {/each}
              </ul>
            </div>
          {:else if land}
            <div class="plan" data-land-plan={land.headBookmark}>
              <div class="plan-stack">
                {#each land.segments as segment (segment.bookmark)}
                  {@const chip = segmentChip(segment.status)}
                  <div class="segment" data-land-segment={segment.bookmark}>
                    <span class="seg-bookmark mono">{segment.bookmark}</span>
                    <span class="seg-chip {chip.tone}" data-land-status={segment.status.kind}>
                      {chip.label}
                    </span>
                    <span class="seg-meta">
                      {segment.changeIds.length} change{segment.changeIds
                        .length === 1
                        ? ""
                        : "s"}
                      {#if segment.pr}
                        · #{segment.pr.number} open
                      {/if}
                    </span>
                    {#if segment.status.kind === "merged"}
                      {@const merged = segment.status}
                      <button
                        class="pr-link"
                        onclick={() => openUrl(merged.url)}
                      >
                        #{merged.number} ↗
                      </button>
                    {/if}
                  </div>
                {/each}
              </div>

              {#if land.actions.length === 0 && land.blockers.length === 0}
                <p class="blurb" data-land-state="nothing-to-run">
                  Nothing to run right now — the notes below say where
                  things stand.
                </p>
              {:else}
                <ul class="actions">
                  {#each land.actions as action, index (index)}
                    {@const row = landActionRow(action, land.remote)}
                    <li class="action" data-land-action={action.kind}>
                      <span class="action-glyph {row.tone}">{row.glyph}</span>
                      <span>{row.text}</span>
                    </li>
                  {/each}
                </ul>
              {/if}

              {#each land.blockers as blocker (blocker)}
                <p class="blocker" data-land-blocker>{blocker}</p>
              {/each}
              {#each land.warnings as warning (warning)}
                <p class="warning" data-land-warning>{warning}</p>
              {/each}

              {#if land.actions.length > 0}
                <div class="plan-go" data-land-go>
                  <Button
                    variant="primary"
                    disabled={landing || land.blockers.length > 0}
                    onclick={landStack}
                  >
                    {landing ? "Landing…" : `Land ${land.headBookmark}`}
                  </Button>
                  {#if land.blockers.length > 0}
                    <span class="go-note">fix the blockers first</span>
                  {/if}
                </div>
              {/if}
            </div>
          {/if}
          {#if landError}
            <p class="error" data-land-error>{landError}</p>
          {/if}
        </section>
      {/if}
    {/if}
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

  .stacks {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-bottom: var(--sp-3);
  }

  .stack-row {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    width: 100%;
    text-align: left;
    padding: var(--sp-2) var(--sp-3);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-m);
    cursor: pointer;
    transition: border-color var(--t-fast) var(--ease-out);
  }

  .stack-row:hover {
    border-color: var(--clr-border-1);
  }

  .stack-row.picked {
    border-color: var(--clr-accent-strong);
  }

  .stack-name {
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .stack-title {
    font-size: var(--text-s);
    color: var(--clr-text-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }

  .stack-meta {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    white-space: nowrap;
  }

  .plan {
    padding: var(--sp-3) var(--sp-4);
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-m);
  }

  .plan-head {
    font-size: var(--text-s);
    color: var(--clr-text-1);
    margin-bottom: var(--sp-2);
  }

  .plan-stack {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-bottom: var(--sp-3);
  }

  .segment {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    font-size: var(--text-s);
  }

  .seg-bookmark {
    color: var(--clr-text-1);
    font-weight: 550;
  }

  .seg-arrow,
  .seg-base {
    color: var(--clr-text-3);
  }

  .seg-chip {
    font-size: var(--text-xs);
    font-weight: 600;
    padding: 1px var(--sp-2);
    border-radius: 999px;
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
  }

  .seg-chip.ok {
    color: var(--clr-ok);
    border-color: color-mix(in oklab, var(--clr-ok) 40%, transparent);
  }

  .seg-chip.accent {
    color: var(--clr-accent-strong);
    border-color: color-mix(in oklab, var(--clr-accent-strong) 40%, transparent);
  }

  .seg-chip.warn {
    color: var(--clr-warn);
    border-color: color-mix(in oklab, var(--clr-warn) 40%, transparent);
  }

  .seg-meta {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .actions,
  .steps {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    padding: 0;
    margin: 0 0 var(--sp-2);
  }

  .action,
  .step {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .action-glyph {
    font-family: var(--font-mono);
    width: 1.2em;
    text-align: center;
    flex: none;
  }

  .action-glyph.accent {
    color: var(--clr-accent-strong);
  }

  .action-glyph.ok {
    color: var(--clr-ok);
  }

  .action-glyph.warn {
    color: var(--clr-warn);
  }

  .step-glyph {
    font-family: var(--font-mono);
    width: 1.2em;
    text-align: center;
    flex: none;
  }

  .step-glyph.done {
    color: var(--clr-ok);
  }

  .step-glyph.failed {
    color: var(--clr-danger);
  }

  .step-glyph.skipped {
    color: var(--clr-text-3);
  }

  .step-text {
    flex: 1;
    min-width: 0;
  }

  .step-detail {
    display: block;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .pr-link {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--clr-accent-strong);
    white-space: nowrap;
    padding: 0;
    cursor: pointer;
  }

  .comment-preview {
    margin-bottom: var(--sp-2);
  }

  .comment-preview summary {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    cursor: pointer;
    user-select: none;
  }

  .comment-preview summary:hover {
    color: var(--clr-text-2);
  }

  .comment-preview pre {
    margin-top: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.5;
    color: var(--clr-text-2);
    background: var(--clr-bg-1);
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-s);
    white-space: pre-wrap;
    overflow-x: auto;
  }

  .blocker {
    font-size: var(--text-s);
    color: var(--clr-danger);
    margin-bottom: var(--sp-1);
  }

  .warning {
    font-size: var(--text-s);
    color: var(--clr-warn);
    margin-bottom: var(--sp-1);
  }

  .plan-go {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
  }

  .go-note {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
