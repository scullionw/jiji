<script lang="ts">
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type { AutoLandState } from "$lib/bindings/AutoLandState";
  import type { LandOptions } from "$lib/bindings/LandOptions";
  import type { LandOutcome } from "$lib/bindings/LandOutcome";
  import type { LandPlan } from "$lib/bindings/LandPlan";
  import type { MergeMethod } from "$lib/bindings/MergeMethod";
  import type { PrSummary } from "$lib/bindings/PrSummary";
  import type { ShipOutcome } from "$lib/bindings/ShipOutcome";
  import type { ShipPlan } from "$lib/bindings/ShipPlan";
  import type { SubmitHead } from "$lib/bindings/SubmitHead";
  import type { SubmitOutcome } from "$lib/bindings/SubmitOutcome";
  import type { SubmitPlan } from "$lib/bindings/SubmitPlan";
  import type { TokenSource } from "$lib/bindings/TokenSource";
  import * as api from "$lib/api";
  import { errorMessage } from "$lib/api";
  import Button from "$lib/components/ui/Button.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { untrack } from "svelte";
  import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
  import { stackSummary, type PublishStack } from "./stacks";
  import StackChain from "./StackChain.svelte";
  import { fromNow } from "$lib/time";
  import { app } from "$lib/state/app.svelte";
  import {
    applySnapshot,
    consumeIntent,
    fetchPr,
    jumpToChange,
  } from "$lib/state/actions";
  import {
    autoland,
    dismissAutoLand,
    startAutoLand,
    stopAutoLand,
  } from "$lib/state/autoland.svelte";
  import {
    connectForge,
    disconnectForge,
    forge,
    refreshForgePrs,
  } from "$lib/state/forge.svelte";
  import {
    autolandOptionsStory,
    autolandStory,
    canQueueAutoLand,
    isInterrupted,
    isTerminalPhase,
    liveAutolandJob,
    resumeBlocker,
    visibleAutolandJobs,
  } from "./autoland";
  import { landActionRow, segmentChip } from "./land";
  import { shipActionRow } from "./ship";
  import {
    bookmarkTaken,
    rerunSummary,
    reviewBookmark,
    reviewRows,
  } from "./review";
  import { actionRow } from "./submit";

  let { snapshot }: { snapshot: RepoSnapshot } = $props();

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

  // The review helpers: every open PR as a row with fetch-for-review (any
  // PR — cross-fork included — lands in a local bookmark via
  // refs/pull/N/head) and re-run-failed-CI, plus a by-number lookup for
  // PRs the open list cannot see.
  const MAX_REVIEW_ROWS = 100;
  const allReviewRows = $derived(reviewRows(forge.prs, repo?.owner ?? null));
  const visibleReviewRows = $derived(allReviewRows.slice(0, MAX_REVIEW_ROWS));

  // The fetch-for-review panel: pick a PR (row click or number lookup),
  // name the bookmark, confirm.
  let reviewFor = $state<PrSummary | null>(null);
  let reviewName = $state("");
  let reviewBusy = $state(false);
  let reviewError = $state<string | null>(null);
  let reviewDone = $state<{ bookmark: string; target: string | null } | null>(
    null,
  );
  const reviewNameTaken = $derived(bookmarkTaken(reviewName, snapshot));

  function openReviewPanel(pr: PrSummary) {
    reviewFor = pr;
    reviewName = reviewBookmark(pr.number);
    reviewError = null;
    reviewDone = null;
  }

  function closeReviewPanel() {
    reviewFor = null;
    reviewError = null;
  }

  async function fetchForReview() {
    if (!reviewFor || !repo || reviewBusy) return;
    if (!reviewName.trim() || reviewNameTaken) return;
    reviewBusy = true;
    reviewError = null;
    try {
      const outcome = await fetchPr(
        repo.remote,
        reviewFor.number,
        reviewName.trim(),
      );
      reviewDone = {
        bookmark: reviewName.trim(),
        target: outcome.targetChange,
      };
      reviewFor = null;
    } catch (err) {
      reviewError = errorMessage(err);
    }
    reviewBusy = false;
  }

  // The by-number lookup feeding the same panel.
  let lookupNumber = $state("");
  let lookupBusy = $state(false);
  let lookupError = $state<string | null>(null);

  async function lookupPr() {
    const number = Number.parseInt(lookupNumber, 10);
    if (!Number.isFinite(number) || number <= 0 || lookupBusy) return;
    lookupBusy = true;
    lookupError = null;
    try {
      openReviewPanel(await api.forgePr(number));
    } catch (err) {
      lookupError = errorMessage(err);
    }
    lookupBusy = false;
  }

  // Re-run failed CI, per row; the note under the list tells what
  // happened, and the badge refresh follows GitHub's answer.
  let rerunBusyFor = $state<string | null>(null);
  let rerunNote = $state<string | null>(null);

  async function rerunCi(pr: PrSummary) {
    if (rerunBusyFor !== null) return;
    rerunBusyFor = String(pr.number);
    rerunNote = null;
    try {
      const report = await api.rerunFailedCi(pr.number);
      rerunNote = `#${pr.number}: ${rerunSummary(report)}`;
      await refreshForgePrs();
    } catch (err) {
      rerunNote = `#${pr.number}: ${errorMessage(err)}`;
    }
    rerunBusyFor = null;
  }

  // Selecting a stack answers a read-only plan; executing replaces it with
  // per-step results.
  let stacks = $state<PublishStack[]>([]);
  let stacksLoading = $state(true);
  let stacksError = $state<string | null>(null);
  $effect(() => {
    const current = snapshot;
    const prs = forge.prs;
    let cancelled = false;
    stacksLoading = true;
    stacksError = null;
    api
      .publishStacks(current, prs)
      .then((answer) => {
        if (!cancelled) stacks = answer;
      })
      .catch((error) => {
        if (!cancelled) stacksError = errorMessage(error);
      })
      .finally(() => {
        if (!cancelled) stacksLoading = false;
      });
    return () => {
      cancelled = true;
    };
  });
  const publishedStacks = $derived(stacks.filter((stack) => stack.prCount > 0));
  const localStacks = $derived(stacks.filter((stack) => stack.prCount === 0));
  let selectedKey = $state<string | null>(null);
  let query = $state("");
  let searchInput = $state<HTMLInputElement>();
  let showLocal = $state(false);
  let page = $state<"stacks" | "review">("stacks");
  let connectionOpen = $state(false);
  let mode = $state<"land" | "publish" | "ship">("land");
  const selected = $derived(
    stacks.find((stack) => stack.key === selectedKey) ?? null,
  );
  const matches = (stack: PublishStack) =>
    stack.searchText.includes(query.trim().toLowerCase());
  const filteredPublished = $derived(publishedStacks.filter(matches));
  const filteredLocal = $derived(localStacks.filter(matches));

  function selectStack(stack: PublishStack) {
    if (executing) return;
    selectedKey = stack.key;
    page = "stacks";
    ++planSeq;
    ++landSeq;
    ++shipSeq;
    plan = null;
    land = null;
    ship = null;
    outcome = null;
    landOutcome = null;
    shipOutcome = null;
    planError = null;
    landError = null;
    shipError = null;
    queueError = null;
    planLoading = false;
    landLoading = false;
    shipLoading = false;
    landMethod = null;
    landBypass = false;
    if (stack.prCount && stack.headBookmark && stack.submitHead && connected) {
      void loadLandPlan(stack.headBookmark);
    } else if (stack.submitHead && connected) {
      void loadPlan(stack.submitHead);
    }
  }

  // Open the current published stack immediately. Keep the selection as
  // facts refresh; a PR fetch must not steal the user's place in the list.
  $effect(() => {
    const choices = stacks;
    const ready = phase === "ready" && !forge.prsLoading && !stacksLoading;
    if (!ready || !choices.length) return;
    untrack(() => {
      if (!selectedKey || !choices.some((stack) => stack.key === selectedKey))
        selectStack(choices.find((stack) => stack.isActive) ?? choices[0]);
      else if (
        connected &&
        !plan &&
        !land &&
        !ship &&
        !planLoading &&
        !landLoading &&
        !shipLoading &&
        !planError &&
        !landError &&
        !shipError
      ) {
        const current = choices.find((stack) => stack.key === selectedKey);
        if (current?.submitHead) selectStack(current);
      }
    });
  });

  async function refreshSelected() {
    await refreshForgePrs();
    if (!selected || executing) return;
    if (mode === "land" && selected.headBookmark)
      await loadLandPlan(selected.headBookmark);
    else if (mode === "publish" && selected.submitHead)
      await loadPlan(selected.submitHead);
    else if (mode === "ship" && selected.headChange)
      await loadShipPlan(selected.headChange);
  }
  let planFor = $state<string | null>(null);
  let planHead: SubmitHead | null = null;
  let plan = $state<SubmitPlan | null>(null);
  let planLoading = $state(false);
  let planError = $state<string | null>(null);
  let publishing = $state(false);
  let outcome = $state<SubmitOutcome | null>(null);
  let planSeq = 0;

  async function loadPlan(head: SubmitHead) {
    mode = "publish";
    const seq = ++planSeq;
    planFor = head.kind === "bookmark" ? head.name : head.change;
    planHead = head;
    plan = null;
    outcome = null;
    planError = null;
    planLoading = true;
    try {
      const answer = await api.submitPlan(head);
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
    if (!plan || !planHead || publishing || plan.blockers.length > 0) return;
    publishing = true;
    planError = null;
    try {
      outcome = await api.submitStack(planHead, plan);
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

  // The run's user-steerable rules: the merge method (null lets the plan
  // pick squash → rebase → merge from what the repo allows) and the
  // bypass-protections switch. Changing either re-plans — the options
  // shape the plan itself, and execution re-derives under the plan's own
  // copy of them.
  let landMethod = $state<MergeMethod | null>(null);
  let landBypass = $state(false);

  function landOptions(): LandOptions {
    return { method: landMethod, bypass: landBypass };
  }

  function replanLand() {
    if (landFor && !landing) void loadLandPlan(landFor);
  }

  async function loadLandPlan(headBookmark: string) {
    mode = "land";
    const seq = ++landSeq;
    landFor = headBookmark;
    land = null;
    landOutcome = null;
    landError = null;
    landLoading = true;
    try {
      const answer = await api.landPlan(headBookmark, landOptions());
      if (seq !== landSeq) return;
      land = answer;
    } catch (err) {
      if (seq !== landSeq) return;
      landError = errorMessage(err);
    } finally {
      if (seq === landSeq) landLoading = false;
    }
  }

  // The command palette's "Land <bookmark>…" and "Ship … to trunk…" rows
  // land here: open the stack's plan card, the same as clicking its row.
  $effect(() => {
    const intent = app.intent;
    if (stacksLoading) return;
    if (intent?.kind === "land") {
      consumeIntent();
      selectedKey =
        stacks.find((stack) => stack.headBookmark === intent.bookmark)?.key ??
        null;
      page = "stacks";
      void loadLandPlan(intent.bookmark);
    } else if (intent?.kind === "ship") {
      consumeIntent();
      selectedKey =
        stacks.find((stack) => stack.headChange === intent.head)?.key ?? null;
      page = "stacks";
      void loadShipPlan(intent.head);
    }
  });

  // The ship-to-trunk workflow: the same plan → confirm → execute shape,
  // no PR — fetch (backend, during planning), rebase if trunk moved,
  // point trunk at the stack, push. Offered for every workstream since
  // shipping needs no bookmark; the backend needs only a GitHub remote,
  // not a connection — the push authenticates like any git push.
  let shipFor = $state<string | null>(null);
  let ship = $state<ShipPlan | null>(null);
  let shipLoading = $state(false);
  let shipError = $state<string | null>(null);
  let shipping = $state(false);
  let shipOutcome = $state<ShipOutcome | null>(null);
  let shipSeq = 0;

  async function loadShipPlan(headChange: string) {
    mode = "ship";
    const seq = ++shipSeq;
    shipFor = headChange;
    ship = null;
    shipOutcome = null;
    shipError = null;
    shipLoading = true;
    try {
      const answer = await api.shipPlan(headChange);
      if (seq !== shipSeq) return;
      ship = answer;
    } catch (err) {
      if (seq !== shipSeq) return;
      shipError = errorMessage(err);
    } finally {
      if (seq === shipSeq) shipLoading = false;
    }
  }

  async function shipStack() {
    if (!ship || !shipFor || shipping || ship.blockers.length > 0) return;
    if (ship.actions.length === 0) return;
    shipping = true;
    shipError = null;
    try {
      const repo = snapshot?.repoPath;
      shipOutcome = await api.shipStack(shipFor, ship);
      // The steps republished the snapshot mid-flow; pulling it once more
      // keeps the flow self-contained (runMutation's posture), and the
      // PR refresh follows a shipped stack's PRs turning merged.
      const refreshed = repo ? await api.currentSnapshot(repo) : null;
      if (refreshed) applySnapshot(refreshed);
      await refreshForgePrs();
    } catch (err) {
      shipError = errorMessage(err);
    }
    shipping = false;
  }

  async function landStack() {
    if (!land || !landFor || landing || land.blockers.length > 0) return;
    landing = true;
    landError = null;
    try {
      const repo = snapshot?.repoPath;
      landOutcome = await api.landStack(landFor, land);
      // The land run's mutations republished the snapshot step by step;
      // pulling it once more makes the flow self-contained and
      // deterministic (the same posture as runMutation), and badges and
      // PR counts follow what just landed on GitHub.
      const refreshed = repo ? await api.currentSnapshot(repo) : null;
      if (refreshed) applySnapshot(refreshed);
      await refreshForgePrs();
    } catch (err) {
      landError = errorMessage(err);
    }
    landing = false;
  }

  // Queue the planned stack for auto-land: the supervised job takes over
  // from here — its card replaces the manual plan flow, and re-deriving
  // plans is its whole point, so the confirmed plan object is not sent.
  let queueing = $state(false);
  const executing = $derived(publishing || landing || shipping || queueing);
  let queueError = $state<string | null>(null);

  async function queueAutoLand() {
    if (!land || !landFor || queueing || landing) return;
    if (!canQueueAutoLand(land)) return;
    queueing = true;
    queueError = null;
    try {
      await startAutoLand(landFor, landOptions());
      landOutcome = null;
    } catch (err) {
      queueError = errorMessage(err);
    }
    queueing = false;
  }

  // Job cards render only in the repo their record belongs to — a record
  // restored from an earlier session must not narrate some other
  // project's stack here. The live job leads; interrupted records (they
  // want a decision) come before terminal ones.
  const jobStatuses = $derived(
    visibleAutolandJobs(autoland.jobs, snapshot?.repoPath),
  );

  // A running job owns the queue affordance — repo-wide, since the host
  // runs one live job at a time; terminal and interrupted records free it
  // (resuming an interrupted job goes through the same start call, so
  // either button works).
  const jobActive = $derived(liveAutolandJob(autoland.jobs) != null);

  // Whether the derived plan may be queued: work to run now, or only
  // conditions waiting clears (transient blockers).
  const queueable = $derived(
    land != null && canQueueAutoLand(land) && !jobActive,
  );
  const manualLandLabel = $derived(
    land?.actions.some((action) => action.kind === "mergePr")
      ? "Land next PR"
      : land?.actions.some((action) => action.kind === "enqueuePr")
        ? "Join merge queue"
        : land?.actions.some((action) => action.kind === "enableAutoMerge")
          ? "Enable auto-merge"
          : "Reconcile stack",
  );

  // The land steps' phrasing needs the remote name; the job state doesn't
  // carry one, so the detected repo's remote (the same remote every land
  // plan uses) fills it in.
  const jobRemote = $derived(repo?.remote ?? "origin");

  // Resuming an interrupted job is the same call queueing makes — the
  // backend seeds the engine from the surviving record.
  let resuming = $state(false);
  let resumeError = $state<string | null>(null);

  async function resumeAutoLand(job: AutoLandState) {
    if (resuming) return;
    resuming = true;
    resumeError = null;
    try {
      // Re-send the visibly confirmed saved rules. The backend also
      // preserves them when omitted, but an admin bypass must never be a
      // hidden consequence of a generic resume action.
      await startAutoLand(job.headBookmark, job.options);
    } catch (err) {
      resumeError = errorMessage(err);
    }
    resuming = false;
  }
</script>

{#snippet resultSteps(
  steps: { status: string; detail: string | null; text: string }[],
)}
  <ol class="steps">
    {#each steps as step, index (index)}
      <li data-step={step.status}>
        <span class="step-glyph {step.status}">{stepGlyph[step.status]}</span>
        <div class="step-text">
          {step.text}{#if step.detail}<p>{step.detail}</p>{/if}
        </div>
      </li>
    {/each}
  </ol>
{/snippet}

{#snippet stackRow(stack: PublishStack)}
  {@const summary = stackSummary(stack)}
  <button
    class="stack-row"
    class:selected={selectedKey === stack.key}
    disabled={executing}
    onclick={() => selectStack(stack)}
    data-stack-select={stack.key}
    aria-pressed={selectedKey === stack.key}
  >
    <span class="stack-row-title">{stack.title}</span>
    {#if stack.isActive}<i class="current-dot" title="Current stack"></i>{/if}
    <span class="stack-row-meta"
      ><span class="status-dot {summary.tone}"></span><span
        >{summary.label}</span
      ><span class="meta-sep"></span><span
        >{stack.prCount
          ? `${stack.prCount} PR${stack.prCount === 1 ? "" : "s"}`
          : `${stack.changeCount} change${stack.changeCount === 1 ? "" : "s"}`}</span
      ></span
    >
  </button>
{/snippet}

<svelte:window
  onkeydown={(event) => {
    if (
      event.key === "/" &&
      page === "stacks" &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey &&
      !(
        event.target instanceof HTMLElement &&
        (event.target.matches("input, textarea, select") ||
          event.target.isContentEditable)
      )
    ) {
      event.preventDefault();
      searchInput?.focus();
    }
    if (event.key === "Escape") connectionOpen = false;
  }}
/>

<div class="publish-view">
  <!-- The section's one toolbar, the same height and hairline as the
     workbench's: what this is, which view, and the connection. -->
  <header class="page-bar">
    <h1>Publish</h1>
    <span class="repo mono" data-forge-repo>
      {repo ? `${repo.owner}/${repo.name}` : snapshot.repoName}
    </span>
    <nav class="segmented page-tabs" aria-label="Publish views">
      <button class:active={page === "stacks"} onclick={() => (page = "stacks")}
        >Stacks <span class="count">{publishedStacks.length || localStacks.length}</span
        ></button
      >
      <button
        class:active={page === "review"}
        onclick={() => (page = "review")}
        data-publish-review
        >Pull requests <span class="count">{allReviewRows.length}</span></button
      >
    </nav>
    <div class="header-actions">
      <span class="tabs-note">{repo ? `via ${repo.remote}` : ""}</span>
      <button
        class="icon-btn"
        aria-label="Refresh pull requests and selected plan"
        title="Refresh pull requests and selected plan"
        disabled={forge.prsLoading || executing}
        onclick={refreshSelected}><Icon name="refresh" size={14} /></button
      >
      <details class="connection" bind:open={connectionOpen}>
        <summary class="btn ghost action"
          ><span class="status-dot" class:ok={connected}></span>{connected
            ? `@${auth?.login}`
            : "Connect GitHub"}<Icon name="chevronDown" size={12} /></summary
        >
        <div class="connection-panel popover">
          {#if phase === "loading" || phase === "verifying"}<p
              data-forge-state={phase}
            >
              Checking GitHub…
            </p>
          {:else if connected && auth}
            <div data-forge-state="connected">
              <strong>Connected to GitHub</strong>
              <p>Token {sourceLabel[auth.source!]}.</p>
              {#if auth.source === "keychain"}<Button
                  disabled={busy}
                  onclick={disconnect}>Disconnect</Button
                >{:else}<span class="managed quiet">Managed outside Jiji</span
                >{/if}
            </div>
          {:else}
            <div data-forge-state="disconnected">
              <strong>Connect to GitHub</strong>
              <p>Use your gh CLI login or a personal access token.</p>
              <form
                onsubmit={(event) => {
                  event.preventDefault();
                  connect();
                }}
              >
                <input
                  class="field"
                  type="password"
                  aria-label="GitHub personal access token"
                  placeholder="github_pat_…"
                  autocomplete="off"
                  bind:value={token}
                  disabled={busy}
                  data-forge-token
                /><Button variant="primary" disabled={busy || !token.trim()}
                  >{busy ? "Connecting…" : "Connect"}</Button
                >
              </form>
              <button
                class="text-button"
                onclick={() => openUrl("https://github.com/settings/tokens")}
                >Create a token ↗</button
              >
            </div>
          {/if}
          {#if error}<p class="error" data-forge-error>{error}</p>{/if}
        </div>
      </details>
    </div>
  </header>

  {#if !repo && phase !== "loading"}
    <EmptyState
      icon="publish"
      title="Add a GitHub remote"
      body="Connect this repository to GitHub to publish and land your stacks."
      hint="jj git remote add origin <url>"
    />
  {:else}
    {#if error && !connectionOpen}<div class="notice error" data-forge-error>
        {error}<button
          class="text-button"
          onclick={() => (connectionOpen = true)}>Connection settings</button
        >
      </div>{/if}
    {#if stacksError}<div class="notice error" role="alert">
        {stacksError}<button class="text-button" onclick={refreshSelected}
          >Retry</button
        >
      </div>{/if}
    {#if forge.prsError}<div class="notice error" data-forge-prs="error">
        {forge.prsError}<button class="text-button" onclick={refreshSelected}
          >Retry</button
        >
      </div>{/if}
    {#if forge.prs?.report.truncated}<div class="notice">
        Showing the 100 most recently updated pull requests. Older stacks may be
        incomplete.
      </div>{/if}

    {#if jobStatuses.length > 0}
      <!-- Auto-land jobs: one flat band per record, the live job first.
         The status bar carries the same story as a chip; this is where
         it is acted on. -->
      <div class="jobs">
        {#each jobStatuses as jobStatus (jobStatus.record.state.headBookmark)}
          {@const job = jobStatus.record.state}
          {@const interrupted = isInterrupted(jobStatus)}
          {@const blocker = interrupted
            ? resumeBlocker(jobStatus, snapshot.bookmarks)
            : null}
          {@const tone = interrupted
            ? "warn"
            : job.phase.kind === "done"
              ? "ok"
              : job.phase.kind === "failed"
                ? "danger"
                : job.phase.kind === "stopped"
                  ? "muted"
                  : job.phase.kind === "waiting" && job.phase.attention
                    ? "warn"
                    : "accent"}
          <section
            class="job"
            data-autoland-job={interrupted ? "interrupted" : job.phase.kind}
          >
            <div class="job-head">
              <span
                class="job-dot {tone}"
                class:pulse={jobStatus.live && !isTerminalPhase(job.phase)}
              ></span>
              <strong class="job-title"
                >{interrupted
                  ? "Interrupted"
                  : job.phase.kind === "done"
                    ? "Stack landed"
                    : job.phase.kind === "failed"
                      ? "Needs attention"
                      : job.phase.kind === "stopped"
                        ? "Stopped"
                        : "Auto-landing"}</strong
              >
              <span class="ref job-name"
                ><span class="truncate">{job.headBookmark}</span></span
              >
              <div class="job-buttons">
                {#if interrupted}{#if !blocker}<span data-autoland-resume
                      ><Button
                        disabled={resuming || jobActive}
                        onclick={() => resumeAutoLand(job)}
                        >{job.options.bypass
                          ? "Resume with bypass"
                          : "Resume watching"}</Button
                      ></span
                    >{/if}<Button onclick={() => dismissAutoLand(jobStatus)}
                    >Dismiss</Button
                  >{:else if !isTerminalPhase(job.phase)}<Button
                    disabled={autoland.stopping}
                    onclick={() => stopAutoLand()}
                    >{autoland.stopping ? "Stopping…" : "Stop"}</Button
                  >{:else}<Button onclick={() => dismissAutoLand(jobStatus)}
                    >Dismiss</Button
                  >{/if}
              </div>
            </div>
            <p class="job-story" data-autoland-story>
              {autolandStory(jobStatus)}{#if interrupted}{" "}
                Last update {fromNow(Number(jobStatus.record.savedAtMs))}.{/if}
            </p>
            {#if job.options.bypass}<p
                class="job-story warning"
                data-autoland-options="bypass"
              >
                {autolandOptionsStory(job.options)}
              </p>{/if}
            {#if blocker}<p class="job-story error" data-autoland-resume-blocker>
                {blocker}
              </p>{/if}
            {#if resumeError}<p class="job-story error" data-autoland-resume-error>
                {resumeError}
              </p>{/if}
            {#if job.lastOutcome}<details class="disclosure job-round">
                <summary
                  >Last round <span
                    >{job.lastOutcome.steps.length} step{job.lastOutcome
                      .steps.length === 1
                      ? ""
                      : "s"}</span
                  ></summary
                >{@render resultSteps(
                  job.lastOutcome.steps.map((step) => ({
                    ...step,
                    text: landActionRow(step.action, jobRemote).text,
                  })),
                )}
              </details>{/if}
          </section>
        {/each}
      </div>
    {/if}

    {#if page === "stacks"}
      <div class="workspace">
        <aside class="stack-browser" aria-label="Stacks">
          <label class="stack-search"
            ><Icon name="search" size={13} /><input
              bind:this={searchInput}
              aria-label="Find a stack"
              placeholder="Find a stack…"
              bind:value={query}
            /><kbd class="kbd">/</kbd></label
          >
          <div class="stack-list">
            {#if publishedStacks.length > 0}<div class="list-heading">
                Published <span>{filteredPublished.length}</span>
              </div>
              {#each filteredPublished as stack (stack.key)}{@render stackRow(
                  stack,
                )}{/each}{/if}
            {#if (forge.prsLoading && !forge.prs) || (stacksLoading && !stacks.length)}<div
                class="list-loading"
                data-forge-prs="loading"
              >
                Finding pull requests…
              </div>{/if}
            {#if localStacks.length > 0}
              {@const localOpen = showLocal || !publishedStacks.length || !!query}
              <button
                class="list-heading local-heading"
                aria-expanded={localOpen}
                onclick={() => (showLocal = !showLocal)}
                >Local work <span>{filteredLocal.length}</span><Icon
                  name={localOpen ? "chevronDown" : "chevronRight"}
                  size={11}
                /></button
              >
              {#if localOpen}{#each filteredLocal as stack (stack.key)}{@render stackRow(
                    stack,
                  )}{/each}{/if}
            {/if}
            {#if query && !filteredPublished.length && !filteredLocal.length}<p
                class="list-loading"
              >
                No matching stacks.
              </p>{/if}
          </div>
        </aside>

        <main class="stack-detail">
          {#if selected}
            {@const summary = stackSummary(selected)}
            {@const currentJob = jobStatuses.find(
              (status) =>
                status.record.state.headBookmark === selected.headBookmark &&
                status.live,
            )}
            <!-- The stack's header, two bars like a change's: the title
               and its one verb, then where the stack stands. -->
            <header class="stack-header">
              <div class="bar title-bar">
                <h2 class="stack-title truncate" title={selected.title}>
                  {selected.title}
                </h2>
                {#if selected.headChange}<button
                    class="btn ghost action"
                    title="Open this stack on the workbench"
                    onclick={() =>
                      selected?.headChange && jumpToChange(selected.headChange)}
                    ><Icon name="workbench" size={14} />Workbench</button
                  >{/if}
              </div>
              <div class="bar meta-bar">
                <span class="fact state"
                  ><span class="status-dot {summary.tone}"></span
                  >{summary.label}</span
                >
                <span class="sep"></span>
                <span class="fact"
                  >{selected.prCount} pull request{selected.prCount === 1
                    ? ""
                    : "s"}</span
                >
                {#if selected.changeCount}<span class="fact"
                    >{selected.changeCount} change{selected.changeCount === 1
                      ? ""
                      : "s"}</span
                  >{/if}
                <span class="fact"
                  >into <span class="ref trunk"
                    ><span class="truncate"
                      >{selected.base || snapshot.trunkBookmark}</span
                    ></span
                  ></span
                >
                {#if selected.isActive}<span class="fact"
                    ><i class="current-dot"></i>Current stack</span
                  >{:else if !selected.submitHead}<span class="fact"
                    >On GitHub only</span
                  >{/if}
              </div>
            </header>
            <div class="detail-body">
              <div class="chain-pane">
                <StackChain
                  stack={selected}
                  landing={currentJob?.record.state.segments ??
                    (landFor === selected.headBookmark
                      ? (land?.segments ?? [])
                      : [])}
                />
              </div>
              <aside class="action-column" aria-label="Stack actions">
                <div class="column-head">
                  {#if connected && selected.submitHead}
                    <nav class="segmented" aria-label="Stack action">
                      {#if selected.headBookmark && selected.prCount}<button
                          class:active={mode === "land"}
                          disabled={executing}
                          data-land-stack={selected.headBookmark}
                          onclick={() =>
                            selected?.headBookmark &&
                            loadLandPlan(selected.headBookmark)}>Land</button
                        >{/if}
                      <button
                        class:active={mode === "publish"}
                        disabled={executing}
                        data-submit-stack={selected.key}
                        onclick={() =>
                          selected?.submitHead && loadPlan(selected.submitHead)}
                        >{selected.prCount ? "Update" : "Publish"}</button
                      >
                    </nav>
                    {#if selected.headChange}<details class="more-menu">
                        <summary
                          class="btn ghost action"
                          class:on={mode === "ship"}
                          aria-label="More stack actions"
                          >More<Icon name="chevronDown" size={12} /></summary
                        >
                        <div
                          class="action-menu popover"
                          role="group"
                          aria-label="More stack actions"
                        >
                          <button
                            class="menu-item"
                            disabled={executing}
                            data-ship-stack={selected.headChange}
                            onclick={() =>
                              selected?.headChange &&
                              loadShipPlan(selected.headChange)}
                            ><Icon name="arrowUp" size={14} /><span
                              class="menu-text"
                              >Ship directly to {snapshot.trunkBookmark}</span
                            ></button
                          >
                        </div>
                      </details>{/if}
                  {:else}
                    <span class="column-label">Next step</span>
                  {/if}
                </div>
                <div class="plan">
                  {#if !connected}
                    <h3>Connect to publish</h3>
                    <p>Sign in to see checks, reviews, and landing options.</p>
                    <div class="primary-actions">
                      <Button
                        variant="primary"
                        onclick={() => (connectionOpen = true)}
                        >Connect GitHub</Button
                      >
                    </div>
                  {:else if !selected.submitHead}
                    <h3>Bring this stack home</h3>
                    <p>
                      This stack is on GitHub. Fetch its head to review it
                      locally.
                    </p>
                    <div class="primary-actions">
                      <Button
                        variant="primary"
                        onclick={() => {
                          const pr = selected?.segments.at(-1)?.pr;
                          if (pr) {
                            page = "review";
                            openReviewPanel(pr);
                          }
                        }}>Fetch for review</Button
                      >
                    </div>
                  {:else if mode === "land"}
                    <div class="plan-body" data-land-plan={land?.headBookmark}>
                      {#if currentJob}
                        <h3>Landing in progress</h3>
                        <p>
                          Jiji is watching this stack. Each PR lands when its
                          requirements are met.
                        </p>
                        <p class="footnote">Keep this repository open.</p>
                      {:else if landLoading}<div
                          class="loading-plan"
                          data-land-state="planning"
                        >
                          <h3>Checking readiness…</h3>
                          <p>Reading checks and reviews from GitHub.</p>
                          <span class="skeleton"></span><span
                            class="skeleton short"
                          ></span>
                        </div>
                      {:else if landOutcome}
                        <h3>
                          {landOutcome.failed
                            ? "Round interrupted"
                            : "Round complete"}
                        </h3>
                        <p>
                          {landOutcome.failed
                            ? "Completed steps are preserved. Check the result and try again."
                            : "Check the stack for its next landing step."}
                        </p>
                        <div
                          data-land-outcome={landOutcome.failed
                            ? "failed"
                            : "done"}
                        >
                          {@render resultSteps(
                            landOutcome.steps.map((step) => ({
                              ...step,
                              text: landActionRow(
                                step.action,
                                land?.remote ?? jobRemote,
                              ).text,
                            })),
                          )}
                        </div>
                        <div class="plan-actions">
                          <Button
                            onclick={() => landFor && loadLandPlan(landFor)}
                            >Check again</Button
                          >
                        </div>
                      {:else if land}
                        {@const candidate = land.segments.find(
                          (segment) =>
                            segment.status.kind === "landing" ||
                            segment.status.kind === "waiting",
                        )?.pr}
                        <h3>
                          {land.blockers.some((blocker) => !blocker.wait)
                            ? "Before this can land"
                            : land.blockers.length
                              ? "Waiting on GitHub"
                              : "Land this stack"}
                        </h3>
                        <p>
                          {selected.prCount > 1
                            ? `Land ${selected.prCount} pull requests in order, rebasing the stack as it goes.`
                            : `Merge into ${land.baseBranch} and clean up the local branch.`}
                        </p>
                        {#if candidate?.review === "reviewRequired" && !land.options.bypass && land.blockers.length === 0}
                          <p class="note warn">
                            <Icon name="eye" size={12} />#{candidate.number} is waiting
                            for review
                          </p>
                        {/if}
                        {#if selected.segments.some((segment) => segment.pr?.isDraft)}<p
                            class="note"
                          >
                            Draft PRs will be marked ready as they land.
                          </p>{/if}
                        {#if land.blockers.length}<ul class="blockers">
                            {#each land.blockers as blocker (blocker.message)}<li
                                class="blocker"
                                class:wait={blocker.wait}
                                data-land-blocker={blocker.wait ? "wait" : "user"}
                              >
                                {blocker.message}
                              </li>{/each}
                          </ul>{/if}
                        {#if land.options.bypass}<p class="note warn">
                            Bypass is on. This attempts to merge past checks,
                            reviews, and merge queues.
                          </p>{/if}
                        <div class="primary-actions" data-land-go>
                          {#if queueable}<span data-autoland-queue
                              ><Button
                                variant="primary"
                                disabled={queueing || executing}
                                onclick={queueAutoLand}
                                >{queueing
                                  ? "Starting…"
                                  : "Auto-land when ready"}<Icon
                                  name="arrowRight"
                                  size={14}
                                /></Button
                              ></span
                            >{/if}
                          {#if land.actions.length}<Button
                              variant={queueable ? "secondary" : "primary"}
                              disabled={executing ||
                                land.blockers.length > 0 ||
                                jobActive}
                              onclick={landStack}
                              >{landing ? "Landing…" : manualLandLabel}</Button
                            >{/if}
                        </div>
                        {#if jobActive}<p class="footnote">
                            Another stack is auto-landing. Stop it to start this
                            one.
                          </p>{:else if queueable}<p class="footnote">
                            Runs while Jiji is open. Stop at any time.
                          </p>{/if}
                        <details class="disclosure">
                          <summary
                            >Merge settings <span>{landMethod ?? "Auto"}</span
                            ></summary
                          >
                          <div class="settings-fields">
                            <label
                              >Merge method<select
                                class="field"
                                data-land-method
                                disabled={executing}
                                value={landMethod ?? "auto"}
                                onchange={(event) => {
                                  landMethod =
                                    event.currentTarget.value === "auto"
                                      ? null
                                      : (event.currentTarget
                                          .value as MergeMethod);
                                  replanLand();
                                }}
                                ><option value="auto">Automatic</option><option
                                  value="squash">Squash</option
                                ><option value="rebase">Rebase</option><option
                                  value="merge">Merge commit</option
                                ></select
                              ></label
                            ><label class="checkbox-label"
                              ><input
                                type="checkbox"
                                data-land-bypass
                                disabled={executing}
                                checked={landBypass}
                                onchange={(event) => {
                                  landBypass = event.currentTarget.checked;
                                  replanLand();
                                }}
                              />Bypass checks &amp; reviews</label
                            >
                          </div>
                        </details>
                        {#if land.actions.length}<details class="disclosure">
                            <summary
                              >This round <span
                                >{land.actions.length} step{land.actions
                                  .length === 1
                                  ? ""
                                  : "s"}</span
                              ></summary
                            >
                            <ol class="action-list">
                              {#each land.actions as action, index (index)}<li
                                  data-land-action={action.kind}
                                >
                                  {landActionRow(action, land.remote).text}
                                </li>{/each}
                            </ol>
                          </details>{/if}
                        {#if land.warnings.length}<details class="disclosure">
                            <summary
                              >Notes <span>{land.warnings.length}</span
                              ></summary
                            >{#each land.warnings as warning (warning)}<p
                                class="note warn"
                                data-land-warning
                              >
                                {warning}
                              </p>{/each}
                          </details>{/if}
                      {/if}
                      {#if landError}<p class="error" data-land-error>
                          {landError}
                        </p>
                        <div class="plan-actions">
                          <Button
                            onclick={() => landFor && loadLandPlan(landFor)}
                            >Try again</Button
                          >
                        </div>{/if}
                      {#if queueError}<p class="error" data-autoland-error>
                          {queueError}
                        </p>{/if}
                    </div>
                  {:else if mode === "publish"}
                    <div
                      class="plan-body"
                      data-submit-plan={plan?.headBookmark}
                    >
                      {#if planLoading}<div
                          class="loading-plan"
                          data-submit-state="planning"
                        >
                          <h3>Checking for updates…</h3>
                          <p>Comparing your stack with GitHub.</p>
                          <span class="skeleton"></span><span
                            class="skeleton short"
                          ></span>
                        </div>
                      {:else if outcome}<h3>
                          {outcome.failed
                            ? "Publishing interrupted"
                            : "Stack published"}
                        </h3>
                        <div
                          data-submit-outcome={outcome.failed
                            ? "failed"
                            : "done"}
                        >
                          {@render resultSteps(
                            outcome.steps.map((step) => ({
                              ...step,
                              text: actionRow(
                                step.action,
                                plan?.remote ?? jobRemote,
                              ).text,
                            })),
                          )}
                        </div>
                        <div class="plan-actions">
                          <Button
                            onclick={() =>
                              selected?.submitHead &&
                              loadPlan(selected.submitHead)}>Check again</Button
                          >
                        </div>
                      {:else if plan}
                        {@const idle =
                          !plan.actions.length && !plan.blockers.length}
                        <h3>
                          {idle
                            ? "All up to date"
                            : selected.prCount
                              ? "Update this stack"
                              : "Publish this stack"}
                        </h3>
                        <p data-submit-state={idle ? "up-to-date" : "ready"}>
                          {idle
                            ? "Your local stack and GitHub are in sync."
                            : "Push your changes and keep the pull requests connected."}
                        </p>
                        {#if plan.blockers.length}<ul class="blockers">
                            {#each plan.blockers as blocker (blocker)}<li
                                class="blocker"
                                data-submit-blocker
                              >
                                {blocker}
                              </li>{/each}
                          </ul>{/if}
                        {#if plan.actions.length}<ol class="action-list">
                            {#each plan.actions as action, index (index)}<li
                                data-submit-action={action.kind}
                              >
                                {actionRow(action, plan.remote).text}
                              </li>{/each}
                          </ol>
                          <div class="primary-actions" data-submit-go>
                            <Button
                              variant="primary"
                              disabled={executing ||
                                plan.blockers.length > 0 ||
                                jobActive}
                              onclick={publish}
                              >{publishing
                                ? "Publishing…"
                                : selected.prCount
                                  ? "Publish updates"
                                  : "Publish stack"}</Button
                            >
                          </div>{/if}
                        {#if plan.stackCommentPreview}<details
                            class="disclosure"
                            data-submit-comment-preview
                          >
                            <summary>Stack comment preview</summary>
                            <pre>{plan.stackCommentPreview
                                .split("\n")
                                .filter((line) => !line.startsWith("<!--"))
                                .join("\n")
                                .trim()}</pre>
                          </details>{/if}
                        {#if plan.prTemplatePath}<p
                            class="footnote"
                            data-submit-template
                          >
                            Uses {plan.prTemplatePath}.
                          </p>{/if}
                        {#if plan.warnings.length}<details class="disclosure">
                            <summary
                              >Notes <span>{plan.warnings.length}</span
                              ></summary
                            >{#each plan.warnings as warning (warning)}<p
                                class="note warn"
                                data-submit-warning
                              >
                                {warning}
                              </p>{/each}
                          </details>{/if}
                      {/if}
                      {#if planError}<p class="error" data-submit-error>
                          {planError}
                        </p>
                        <div class="plan-actions">
                          <Button
                            onclick={() =>
                              selected?.submitHead &&
                              loadPlan(selected.submitHead)}>Try again</Button
                          >
                        </div>{/if}
                    </div>
                  {:else}
                    <div class="plan-body" data-ship-plan={ship?.headChange}>
                      {#if shipLoading}<div
                          class="loading-plan"
                          data-ship-state="planning"
                        >
                          <h3>Checking trunk…</h3>
                          <p>Fetching the latest remote state.</p>
                          <span class="skeleton"></span><span
                            class="skeleton short"
                          ></span>
                        </div>
                      {:else if shipOutcome}<h3>
                          {shipOutcome.failed
                            ? "Shipping interrupted"
                            : "Shipped to trunk"}
                        </h3>
                        <div
                          data-ship-outcome={shipOutcome.failed
                            ? "failed"
                            : "done"}
                        >
                          {@render resultSteps(
                            shipOutcome.steps.map((step) => ({
                              ...step,
                              text: shipActionRow(step.action).text,
                            })),
                          )}
                        </div>
                      {:else if ship}<h3>Ship to {ship.trunkBookmark}</h3>
                        <p>
                          Push this stack directly to trunk. This skips pull
                          request review.
                        </p>
                        {#if ship.blockers.length}<ul class="blockers">
                            {#each ship.blockers as blocker (blocker)}<li
                                class="blocker"
                                data-ship-blocker
                              >
                                {blocker}
                              </li>{/each}
                          </ul>{/if}
                        {#if ship.actions.length}<ol class="action-list">
                            {#each ship.actions as action, index (index)}<li
                                data-ship-action={action.kind}
                              >
                                {shipActionRow(action).text}
                              </li>{/each}
                          </ol>{/if}
                        {#each ship.warnings as warning (warning)}<p
                            class="note warn"
                            data-ship-warning
                          >
                            {warning}
                          </p>{/each}
                        {#if ship.actions.length}<div
                            class="primary-actions"
                            data-ship-go
                          >
                            <Button
                              variant="primary"
                              disabled={executing ||
                                ship.blockers.length > 0 ||
                                jobActive}
                              onclick={shipStack}
                              >{shipping
                                ? "Shipping…"
                                : `Ship to ${ship.trunkBookmark}`}</Button
                            >
                          </div>{:else if !ship.blockers.length}<p
                            class="footnote"
                            data-ship-state="nothing-to-run"
                          >
                            Already shipped.
                          </p>{/if}{/if}
                      {#if shipError}<p class="error" data-ship-error>
                          {shipError}
                        </p>{/if}
                    </div>
                  {/if}
                </div>
              </aside>
            </div>
          {:else}<EmptyState
              icon="stack"
              title={stacks.length
                ? "Choose a stack"
                : "A clear path to shipped"}
              body={stacks.length
                ? "Select a stack to see its pull requests and next steps."
                : "Your next change starts on the workbench. Published stacks will appear here, ready to land."}
            />{/if}
        </main>
      </div>
    {:else}
      <section class="review-page">
        <div class="review-bar">
          <span class="review-count"
            >{forge.prsLoading && !forge.prs
              ? "Finding pull requests…"
              : `${allReviewRows.length} open pull request${allReviewRows.length === 1 ? "" : "s"}`}</span
          >
          <form
            class="lookup-row"
            onsubmit={(event) => {
              event.preventDefault();
              lookupPr();
            }}
          >
            <input
              class="field"
              aria-label="Pull request number"
              placeholder="PR number"
              inputmode="numeric"
              bind:value={lookupNumber}
              data-review-lookup
            /><Button
              disabled={lookupBusy || !/^\d+$/.test(lookupNumber.trim())}
              >{lookupBusy ? "Finding…" : "Look up"}</Button
            >
          </form>
        </div>
        {#if lookupError}<p class="notice error" data-review-lookup-error>
            {lookupError}
          </p>{/if}
        {#if reviewFor}{@const pr = reviewFor}
          <div class="review-panel tray" data-review-panel={pr.number}>
            <div class="review-panel-head">
              <h3>Fetch #{pr.number} for review</h3>
              <p class="truncate" title={pr.title}>{pr.title}</p>
            </div>
            <form
              class="review-name-row"
              onsubmit={(event) => {
                event.preventDefault();
                fetchForReview();
              }}
            >
              <label for="review-bookmark">Local bookmark</label><input
                id="review-bookmark"
                class="field mono"
                bind:value={reviewName}
                disabled={reviewBusy}
                data-review-name
              /><Button
                variant="primary"
                disabled={reviewBusy || !reviewName.trim() || reviewNameTaken}
                >{reviewBusy ? "Fetching…" : "Fetch"}</Button
              ><button
                type="button"
                class="btn ghost"
                disabled={reviewBusy}
                onclick={closeReviewPanel}>Cancel</button
              >
            </form>
            {#if reviewNameTaken}<p class="note warn" data-review-taken>
                This bookmark already exists. Choose another name.
              </p>{/if}{#if reviewError}<p class="note error" data-review-error>
                {reviewError}
              </p>{/if}
          </div>{/if}
        {#if reviewDone}<p class="notice" data-review-done>
            Fetched into <span class="ref"
              ><span class="truncate">{reviewDone.bookmark}</span></span
            >.{#if reviewDone.target}{@const target = reviewDone.target}<button
                class="text-button"
                onclick={() => jumpToChange(target)}>Open in workbench ↗</button
              >{/if}
          </p>{/if}
        {#if rerunNote}<p class="notice" data-review-rerun-note>
            {rerunNote}
          </p>{/if}
        <div
          class="review-list"
          data-forge-prs={forge.prsLoading ? "loading" : "ready"}
        >
          {#each visibleReviewRows as row (row.pr.number)}<article
              class="review-row"
              data-review-pr={row.pr.number}
            >
              <span class="review-num mono">#{row.pr.number}</span>
              <div class="review-main">
                <button
                  class="review-title"
                  title="Open #{row.pr.number} on GitHub"
                  onclick={() => openUrl(row.pr.url)}>{row.pr.title}</button
                >
                <span class="review-sub"
                  ><span>{row.pr.isDraft ? "Draft" : "Open"}</span><span
                    class="sep"
                  ></span><span class="mono truncate"
                    >{row.headLabel} → {row.pr.baseBranch}</span
                  ></span
                >
              </div>
              {#if row.badge.checks}<span
                  class="review-check {row.badge.checks.tone}"
                  title={row.badge.checks.label}
                  >{row.badge.checks.glyph}</span
                >{/if}
              <div class="review-actions">
                {#if row.canRerun}<button
                    class="btn ghost"
                    data-review-rerun={row.pr.number}
                    disabled={rerunBusyFor !== null}
                    onclick={() => rerunCi(row.pr)}>Re-run CI</button
                  >{/if}<button
                  class="btn"
                  data-review-fetch={row.pr.number}
                  onclick={() => openReviewPanel(row.pr)}>Fetch for review</button
                >
              </div>
            </article>{/each}
          {#if !visibleReviewRows.length}<p class="list-loading">
              {forge.prsLoading
                ? "Finding pull requests…"
                : "No open pull requests."}
            </p>{/if}
        </div>
      </section>
    {/if}
  {/if}
</div>

<style>
  .publish-view {
    height: 100%;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--clr-bg-1);
    color: var(--clr-text-1);
    container-type: inline-size;
  }

  /* ── Page bar ── the section's one toolbar: same height and hairline as
     the workbench's, so switching sections never moves the frame. */
  .page-bar {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: var(--toolbar-h);
    padding: 0 var(--sp-2) 0 var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
  }

  h1 {
    font-size: var(--text-l);
    font-weight: 600;
    margin: 0;
  }

  .repo {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .page-tabs {
    margin-left: var(--sp-2);
  }

  .page-tabs .count {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    margin-left: auto;
  }

  .tabs-note {
    margin-right: var(--sp-1);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  /* A toolbar verb: the change header's ghost button, 26px tall. */
  .action {
    height: 26px;
    padding: 0 8px;
    gap: 5px;
    font-size: var(--text-s);
  }

  .action.on {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .connection {
    position: relative;
    z-index: 5;
  }

  .connection > summary {
    list-style: none;
    cursor: pointer;
  }

  .connection > summary::-webkit-details-marker,
  .more-menu > summary::-webkit-details-marker,
  .disclosure > summary::-webkit-details-marker {
    display: none;
  }

  .connection[open] > summary {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .connection-panel {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    width: 330px;
    padding: var(--sp-4);
    font-size: var(--text-s);
  }

  .connection-panel strong {
    font-size: var(--text-m);
    font-weight: 600;
  }

  .connection-panel p {
    margin-top: 2px;
    line-height: 1.5;
    color: var(--clr-text-2);
  }

  .connection-panel form {
    display: flex;
    gap: var(--sp-2);
    margin: var(--sp-3) 0;
  }

  .connection-panel .field {
    min-width: 0;
    flex: 1;
  }

  /* ── Notices ── flat bands under the toolbar, never floating. */
  .notice {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 32px;
    padding: var(--sp-1) var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-2);
    background: var(--clr-bg-well);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .notice.error {
    color: var(--clr-danger);
  }

  /* ── Auto-land jobs ── one band per record, the live job first. */
  .jobs {
    flex-shrink: 0;
    max-height: 36%;
    overflow: auto;
    background: var(--clr-bg-well);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .job {
    padding: var(--sp-2) var(--sp-4) var(--sp-3);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .job:last-child {
    border-bottom: 0;
  }

  .job-head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 28px;
  }

  .job-dot {
    flex-shrink: 0;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
    color: var(--clr-text-3);
  }

  .job-dot.accent {
    color: var(--clr-accent-strong);
  }

  .job-dot.ok {
    color: var(--clr-ok);
  }

  .job-dot.warn {
    color: var(--clr-warn);
  }

  .job-dot.danger {
    color: var(--clr-danger);
  }

  .job-dot.pulse {
    animation: job-pulse 1.6s ease-in-out infinite;
  }

  @keyframes job-pulse {
    50% {
      opacity: 0.3;
    }
  }

  .job-title {
    font-size: var(--text-m);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .job-name {
    max-width: 24em;
  }

  .job-buttons {
    margin-left: auto;
    display: flex;
    gap: var(--sp-2);
    flex-shrink: 0;
  }

  /* Story lines hang from the title, past the dot. */
  .job-story {
    max-width: 80em;
    margin: 0 0 0 15px;
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-2);
  }

  .job-story + .job-story {
    margin-top: 2px;
  }

  .job-round {
    margin-left: 15px;
    border-top: 0;
    padding-top: 2px;
  }

  /* In a full-width band the count stays beside its label. */
  .job .job-round > summary > span {
    margin-left: var(--sp-1);
  }

  /* ── Workspace ── the stack browser beside the stack's detail. */
  .workspace {
    display: grid;
    grid-template-columns: 240px minmax(0, 1fr);
    flex: 1;
    min-height: 0;
  }

  .stack-browser {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--clr-border-2);
  }

  /* The search sits in a toolbar-height bar, so its hairline meets the
     stack header's title bar on the right. */
  .stack-search {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--toolbar-h);
    padding: 0 var(--sp-3) 0 var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
    cursor: text;
  }

  .stack-search:focus-within {
    color: var(--clr-text-2);
  }

  .stack-search input {
    flex: 1;
    min-width: 0;
    height: 100%;
    padding: 0;
    border: 0;
    background: transparent;
    font-size: var(--text-s);
    color: var(--clr-text-1);
  }

  .stack-search input:focus {
    outline: none;
  }

  .stack-search input::placeholder {
    color: var(--clr-text-3);
  }

  .stack-list {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding-bottom: var(--sp-5);
  }

  .list-heading {
    display: flex;
    align-items: center;
    gap: 5px;
    width: 100%;
    height: 28px;
    margin-top: var(--sp-2);
    padding: 0 var(--sp-3) 0 var(--sp-4);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--clr-text-3);
    text-align: left;
  }

  .list-heading > span {
    margin-left: auto;
    font-family: var(--font-mono);
    font-weight: 400;
  }

  .local-heading {
    cursor: pointer;
    transition: color var(--t-fast) var(--ease-out);
  }

  .local-heading:hover {
    color: var(--clr-text-1);
  }

  /* A stack row: the title, where it stands, and the current-stack mark
     in the working-copy color. Selection fills the row and marks its
     leading edge, like a graph row. */
  .stack-row {
    position: relative;
    display: block;
    width: 100%;
    padding: 7px 26px 7px var(--sp-4);
    text-align: left;
    transition: background var(--t-fast) var(--ease-out);
  }

  .stack-row:hover:not(:disabled) {
    background: var(--clr-bg-hover);
  }

  .stack-row.selected {
    background: color-mix(in srgb, var(--clr-accent) 12%, transparent);
    box-shadow: inset 2px 0 0 var(--clr-accent);
  }

  .stack-row:disabled {
    opacity: 0.5;
  }

  .stack-row-title {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    font-size: var(--text-m);
    line-height: 1.4;
    color: var(--clr-text-1);
  }

  .stack-row .current-dot {
    position: absolute;
    top: 13px;
    right: var(--sp-3);
  }

  .stack-row-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 3px;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .status-dot {
    display: inline-block;
    flex-shrink: 0;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
    color: var(--clr-text-3);
  }

  .status-dot.ok {
    color: var(--clr-ok);
  }

  .status-dot.warn {
    color: var(--clr-warn);
  }

  .status-dot.danger {
    color: var(--clr-danger);
  }

  .current-dot {
    display: inline-block;
    flex-shrink: 0;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--clr-working-copy);
  }

  .meta-sep {
    flex-shrink: 0;
    width: 1px;
    height: 10px;
    background: var(--clr-border-1);
  }

  .list-loading {
    padding: var(--sp-3) var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  /* ── Stack detail ── a two-bar header, then the ledger and the action
     column scrolling on their own. */
  .stack-detail {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }

  .stack-header {
    flex-shrink: 0;
    border-bottom: 1px solid var(--clr-border-2);
  }

  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    padding: 0 var(--sp-2) 0 var(--sp-4);
  }

  .title-bar {
    height: var(--toolbar-h);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .stack-title {
    flex: 1;
    min-width: 0;
    margin: 0;
    font-size: var(--text-l);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .meta-bar {
    min-height: 30px;
    padding-top: 3px;
    padding-bottom: 3px;
    flex-wrap: wrap;
    gap: 4px var(--sp-3);
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .fact {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
    min-width: 0;
  }

  .fact.state {
    font-weight: 500;
    color: var(--clr-text-2);
  }

  .sep {
    flex-shrink: 0;
    width: 1px;
    height: 12px;
    background: var(--clr-border-1);
  }

  .ref.trunk {
    max-width: 20em;
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
  }

  .detail-body {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    flex: 1;
    min-height: 0;
  }

  .chain-pane {
    min-width: 0;
    min-height: 0;
    overflow: auto;
  }

  .action-column {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    overflow: auto;
    border-left: 1px solid var(--clr-border-2);
  }

  /* The column's head: the mode switch at the ledger head's height, so
     their hairlines meet. */
  .column-head {
    position: sticky;
    top: 0;
    z-index: 2;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--toolbar-h);
    padding: 0 var(--sp-2) 0 var(--sp-3);
    border-bottom: 1px solid var(--clr-border-2);
    background: var(--clr-bg-1);
  }

  .column-label {
    padding-left: var(--sp-1);
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-2);
  }

  .more-menu {
    position: relative;
    margin-left: auto;
  }

  .more-menu > summary {
    list-style: none;
    cursor: pointer;
  }

  .more-menu[open] > summary {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .action-menu {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 8;
    display: flex;
    flex-direction: column;
    min-width: 220px;
    padding: var(--sp-1);
  }

  .menu-item {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 28px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-s);
    font-size: var(--text-m);
    color: var(--clr-text-1);
    text-align: left;
    white-space: nowrap;
    transition: background var(--t-fast) var(--ease-out);
  }

  .menu-item :global(svg) {
    color: var(--clr-text-3);
  }

  .menu-item:hover:not(:disabled) {
    background: var(--clr-bg-hover);
  }

  .menu-item:disabled {
    opacity: 0.5;
  }

  /* ── The plan ── what the next step will do, stated plainly: a heading,
     a sentence, the blockers, the verb. Sections below fold on hairlines. */
  .plan {
    padding: var(--sp-4) var(--sp-4) var(--sp-6);
  }

  h3 {
    margin: 0 0 4px;
    font-size: var(--text-m);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .plan p {
    margin: 0 0 var(--sp-3);
    font-size: var(--text-s);
    line-height: 1.55;
    color: var(--clr-text-2);
    overflow-wrap: anywhere;
  }

  .plan p.note,
  .plan p.footnote,
  .review-panel p.note {
    display: flex;
    gap: 6px;
    align-items: flex-start;
    margin-bottom: var(--sp-2);
    font-size: var(--text-xs);
    line-height: 1.5;
    color: var(--clr-text-3);
  }

  .plan p.note :global(svg) {
    flex-shrink: 0;
    margin-top: 2px;
  }

  .plan p.note.warn,
  .review-panel p.note.warn {
    color: var(--clr-warn);
  }

  .review-panel p.note.error {
    color: var(--clr-danger);
  }

  .plan p.footnote {
    margin: 0 0 var(--sp-3);
  }

  /* Blockers: a list, not alerts. The dot carries the tone — needs you
     (danger) or clears by itself (warn). */
  .blockers {
    list-style: none;
    margin: 0 0 var(--sp-3);
    padding: 0;
  }

  .blocker {
    position: relative;
    padding: 4px 0 4px 14px;
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-1);
    overflow-wrap: anywhere;
  }

  .blocker::before {
    content: "";
    position: absolute;
    left: 0;
    top: 10px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--clr-danger);
  }

  .blocker.wait::before {
    background: var(--clr-warn);
  }

  .primary-actions {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: var(--sp-4) 0 var(--sp-3);
  }

  .primary-actions :global(.btn) {
    width: 100%;
    justify-content: center;
    height: 30px;
    font-size: var(--text-m);
  }

  .plan-actions {
    margin: var(--sp-2) 0 var(--sp-4);
  }

  .disclosure {
    padding: var(--sp-1) 0;
    border-top: 1px solid var(--clr-border-2);
    font-size: var(--text-s);
  }

  .disclosure > summary {
    display: flex;
    gap: 6px;
    align-items: center;
    height: 28px;
    list-style: none;
    cursor: pointer;
    color: var(--clr-text-2);
    transition: color var(--t-fast) var(--ease-out);
  }

  .disclosure > summary:hover {
    color: var(--clr-text-1);
  }

  .disclosure > summary::before {
    content: "›";
    display: inline-block;
    width: 8px;
    color: var(--clr-text-3);
    transition: transform var(--t-fast) var(--ease-out);
  }

  .disclosure[open] > summary::before {
    transform: rotate(90deg);
  }

  .disclosure > summary > span {
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .settings-fields {
    padding: var(--sp-1) 0 var(--sp-2);
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .settings-fields label {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin-bottom: var(--sp-3);
  }

  .settings-fields .checkbox-label {
    flex-direction: row;
    align-items: center;
    gap: var(--sp-2);
    margin-bottom: var(--sp-1);
  }

  .settings-fields input[type="checkbox"] {
    margin: 0;
    accent-color: var(--clr-accent);
  }

  select.field {
    width: 100%;
  }

  /* Numbered plan steps, the numbers in a mono column. */
  .action-list {
    list-style: none;
    counter-reset: step;
    margin: var(--sp-2) 0 var(--sp-3);
    padding: 0;
    font-size: var(--text-s);
    line-height: 1.5;
    color: var(--clr-text-2);
  }

  .action-list li {
    position: relative;
    padding: 3px 0 3px 22px;
    counter-increment: step;
    overflow-wrap: anywhere;
  }

  .action-list li::before {
    content: counter(step);
    position: absolute;
    left: 0;
    top: 3px;
    width: 14px;
    text-align: right;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  /* Executed steps: the glyph says what happened to each. */
  .steps {
    list-style: none;
    margin: var(--sp-2) 0 var(--sp-3);
    padding: 0;
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .steps li {
    display: flex;
    gap: var(--sp-2);
    padding: 3px 0;
    line-height: 1.5;
  }

  .step-glyph {
    flex-shrink: 0;
    width: 14px;
    text-align: center;
    font-family: var(--font-mono);
    color: var(--clr-text-3);
  }

  .step-glyph.done {
    color: var(--clr-ok);
  }

  .step-glyph.failed {
    color: var(--clr-danger);
  }

  .step-text {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .steps p {
    margin: 1px 0 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .plan p.error,
  .error {
    color: var(--clr-danger);
  }

  .warning {
    color: var(--clr-warn);
  }

  .quiet {
    color: var(--clr-text-3);
  }

  .text-button {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--clr-accent-strong);
    font-size: var(--text-s);
  }

  .text-button :global(svg) {
    color: inherit;
  }

  .text-button:hover {
    text-decoration: underline;
  }

  pre {
    margin: var(--sp-1) 0 var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-m);
    background: var(--clr-bg-well);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font: var(--text-xs) / 1.6 var(--font-mono);
    color: var(--clr-text-2);
  }

  .skeleton {
    display: block;
    width: 100%;
    height: 10px;
    margin: var(--sp-2) 0;
    border-radius: var(--radius-s);
    background: color-mix(in srgb, var(--clr-text-1) 8%, transparent);
    animation: pulse 1.3s ease-in-out infinite alternate;
  }

  .skeleton.short {
    width: 62%;
  }

  @keyframes pulse {
    to {
      opacity: 0.35;
    }
  }

  /* ── Pull requests ── a toolbar, then one row per open PR. */
  .review-page {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .review-bar {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: var(--toolbar-h);
    padding: 0 var(--sp-2) 0 var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
  }

  .review-count {
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-2);
  }

  .lookup-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-left: auto;
  }

  .lookup-row .field {
    width: 110px;
    height: 26px;
  }

  .review-list {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }

  .review-row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 44px;
    padding: 5px var(--sp-3) 5px var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    transition: background var(--t-fast) var(--ease-out);
  }

  .review-row:hover {
    background: var(--clr-bg-hover);
  }

  .review-num {
    flex-shrink: 0;
    width: 44px;
    font-size: var(--text-s);
    color: var(--clr-text-3);
  }

  .review-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .review-title {
    display: block;
    max-width: 100%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    text-align: left;
    font-size: var(--text-m);
    color: var(--clr-text-1);
    transition: color var(--t-fast) var(--ease-out);
  }

  .review-title:hover {
    color: var(--clr-accent-strong);
  }

  .review-sub {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    white-space: nowrap;
  }

  .review-check {
    flex-shrink: 0;
    width: 16px;
    text-align: center;
    font-size: var(--text-s);
  }

  .review-check.ok {
    color: var(--clr-ok);
  }

  .review-check.warn {
    color: var(--clr-warn);
  }

  .review-check.danger {
    color: var(--clr-danger);
  }

  .review-actions {
    flex-shrink: 0;
    display: flex;
    gap: var(--sp-1);
  }

  /* The fetch panel: a tray under the toolbar, like a change's panels. */
  .review-panel {
    flex-shrink: 0;
    padding: var(--sp-3) var(--sp-4);
    border-top: 0;
    border-bottom: 1px solid var(--clr-border-2);
    background: var(--clr-bg-well);
  }

  .review-panel-head {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    margin-bottom: var(--sp-2);
  }

  .review-panel-head h3 {
    flex-shrink: 0;
    margin: 0;
  }

  .review-panel-head p {
    min-width: 0;
    margin: 0;
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .review-name-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-s);
    color: var(--clr-text-2);
  }

  .review-name-row .field {
    width: 220px;
  }

  .review-panel p.note {
    margin: var(--sp-2) 0 0;
  }

  @container (max-width: 1000px) {
    .workspace {
      grid-template-columns: 210px minmax(0, 1fr);
    }

    .detail-body {
      grid-template-columns: minmax(0, 1fr) 264px;
    }
  }

  @container (max-width: 780px) {
    .workspace {
      grid-template-columns: 180px minmax(0, 1fr);
    }

    /* Narrow: the action column folds above the ledger and the detail
       scrolls as one. */
    .detail-body {
      display: flex;
      flex-direction: column;
      overflow: auto;
    }

    .chain-pane,
    .action-column {
      flex-shrink: 0;
      min-height: auto;
      overflow: visible;
    }

    .action-column {
      order: -1;
      border-left: 0;
      border-bottom: 1px solid var(--clr-border-2);
    }

    .column-head,
    .chain-pane :global(.chain-head) {
      position: static;
    }

    .tabs-note,
    .repo {
      display: none;
    }

    .job-head {
      flex-wrap: wrap;
    }

    .review-name-row {
      flex-wrap: wrap;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .skeleton,
    .job-dot.pulse {
      animation: none;
    }
  }
</style>
