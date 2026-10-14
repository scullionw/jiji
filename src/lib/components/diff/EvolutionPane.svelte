<script lang="ts">
  // A change's life over time (`jj evolog`) as a first-class surface: a
  // scrubbable timeline of every version the change was rewritten
  // through, with the selected step's diff rendered by the real diff
  // pipeline — syntax highlighting, unified/split, virtualized cards.
  // Built for the `jj edit` workflow, where "what did I just change" is
  // the newest rewrite step, not the change's whole diff — the pane
  // opens there. ←/→ scrub through time; ⇧-click (or ⇧←/⇧→) widens the
  // span to read several rewrites as one diff; Restore returns the
  // change to the version under the cursor.
  //
  // Versions are addressed by commit id — content-stable, and hidden ids
  // still resolve — so fetched step diffs never go stale; new operations
  // only grow the chain. The timeline wears each step's weight (+/−)
  // as the step diffs arrive, so the big rewrites stand out at a glance.

  import { onMount, tick, untrack } from "svelte";
  import { tinykeys } from "tinykeys";
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import Icon from "$lib/components/ui/Icon.svelte";
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import type { ChangeDiff } from "$lib/bindings/ChangeDiff";
  import type { ChangeEvolog } from "$lib/bindings/ChangeEvolog";
  import type { GraphNode } from "$lib/bindings/GraphNode";
  import * as api from "$lib/api";
  import { restoreChangeVersion } from "$lib/state/actions";
  import { app } from "$lib/state/app.svelte";
  import { panelIn } from "$lib/motion";
  import { fromNow, shortAge } from "$lib/time";
  import FileDiffCard from "./FileDiffCard.svelte";
  import { currentDiff } from "./current.svelte";
  import { totalStats, type DiffLayout } from "./diff";
  import {
    defaultSpan,
    extendTo,
    grow,
    scrub,
    stepTo,
    timelineVersions,
    type Span,
  } from "./evolution";

  let {
    node,
    opId,
    layout,
    onclose,
  }: {
    node: GraphNode;
    /// Current operation id; a new operation refetches the chain (a
    /// restore or working-copy snapshot stacks a new version on top).
    opId: string;
    layout: DiffLayout;
    onclose: () => void;
  } = $props();

  // Raw: replaced per fetch, never edited.
  let evolog = $state.raw<ChangeEvolog | null>(null);
  let error = $state<string | null>(null);
  let requestSeq = 0;

  $effect(() => {
    const id = node.id;
    void opId; // refetch when a new operation lands
    const token = ++requestSeq;
    api.changeEvolog(id).then(
      (result) => {
        if (token !== requestSeq) return;
        evolog = result;
        error = null;
      },
      (err) => {
        if (token !== requestSeq) return;
        evolog = null;
        error = api.errorMessage(err);
      },
    );
  });

  // Only an answer for the shown change renders; a stale one is dropped
  // by the token check on arrival and by this guard while in flight.
  const view = $derived(evolog && evolog.id === node.id ? evolog : null);
  const versions = $derived(view ? timelineVersions(view) : []);

  // The selected run of steps. Chain growth (a snapshot, a restore)
  // snaps back to the newest step — the "what just changed" the pane
  // exists to answer; scrubbing within one chain keeps the selection.
  let span = $state<Span | null>(null);
  let spanFor = "";

  $effect(() => {
    const head = versions[versions.length - 1]?.commitId ?? "";
    const key = `${node.id} ${head} ${versions.length}`;
    if (key === spanFor) return;
    spanFor = key;
    untrack(() => (span = defaultSpan(versions.length)));
  });

  // Step and span diffs keyed older→newer commit id. Content-stable, so
  // entries never go stale — the map only grows while the pane is up.
  const diffs = new SvelteMap<
    string,
    ChangeDiff | "loading" | { error: string }
  >();

  function keyBetween(from: number, to: number): string {
    return `${versions[from].commitId}→${versions[to].commitId}`;
  }

  function fetchBetween(from: number, to: number): void {
    const key = keyBetween(from, to);
    if (diffs.has(key)) return;
    diffs.set(key, "loading");
    api.compareDiff(versions[from].commitId, versions[to].commitId).then(
      (diff) => diffs.set(key, diff),
      (err) => diffs.set(key, { error: api.errorMessage(err) }),
    );
  }

  // Guarded on the chain's length: a snapshot can shrink the chain in the
  // same flush that resets the span, and derived order is not promised.
  const spanKey = $derived(
    span && span.to < versions.length ? keyBetween(span.from, span.to) : null,
  );

  $effect(() => {
    const s = span;
    if (!s || s.to >= versions.length) return;
    untrack(() => fetchBetween(s.from, s.to));
  });

  // Warm every step's diff once the chain is known, one request at a
  // time: the timeline gets its per-step weights and scrubbing lands on
  // cache hits. Sequential so a long chain never floods the backend, and
  // capped so a pathological one stops asking; steps beyond the cap
  // still fetch on demand when selected.
  const PREFETCH_CAP = 32;
  let prefetchSeq = 0;

  $effect(() => {
    const vs = versions;
    if (vs.length < 2 || vs.length > PREFETCH_CAP) return;
    const token = ++prefetchSeq;
    untrack(() => {
      void (async () => {
        for (let to = vs.length - 1; to >= 1; to--) {
          if (token !== prefetchSeq) return;
          const key = `${vs[to - 1].commitId}→${vs[to].commitId}`;
          if (diffs.has(key)) continue;
          diffs.set(key, "loading");
          try {
            diffs.set(
              key,
              await api.compareDiff(vs[to - 1].commitId, vs[to].commitId),
            );
          } catch (err) {
            diffs.set(key, { error: api.errorMessage(err) });
          }
        }
      })();
    });
    return () => {
      prefetchSeq++;
    };
  });

  const held = $derived(spanKey ? diffs.get(spanKey) : undefined);
  const spanDiff = $derived(
    held !== undefined && held !== "loading" && !("error" in held)
      ? held
      : null,
  );

  // What a step did, for the timeline's weight labels: null while the
  // diff is unfetched or failed.
  function stepDiff(to: number): ChangeDiff | null {
    const entry = diffs.get(keyBetween(to - 1, to));
    return entry !== undefined && entry !== "loading" && !("error" in entry)
      ? entry
      : null;
  }

  const focal = $derived(span ? (versions[span.to] ?? null) : null);
  const focalIsCurrent = $derived(
    !span || span.to === versions.length - 1,
  );
  const canRestore = $derived(
    focal !== null && !focalIsCurrent && node.kind !== "immutable",
  );

  // ── Selection ──

  function pickVersion(i: number, event: MouseEvent) {
    span =
      event.shiftKey && span
        ? extendTo(span, i, versions.length)
        : stepTo(i, versions.length);
  }

  function pickStep(to: number) {
    span = { from: to - 1, to };
  }

  function isEditable(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    return (
      !!el &&
      (el.tagName === "INPUT" ||
        el.tagName === "TEXTAREA" ||
        el.isContentEditable)
    );
  }

  function onArrow(
    event: KeyboardEvent,
    apply: (s: Span) => Span | null,
  ): void {
    if (app.paletteOpen || isEditable(event.target)) return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    if (!span) return;
    event.preventDefault();
    span = apply(span) ?? span;
  }

  // ←/→ scrub; ⇧←/⇧→ widen. The workbench leaves these keys free (↑/↓
  // walk the graph — and close the pane with the selection move). Both
  // handlers branch on shiftKey themselves, so the plain and ⇧ bindings
  // stay in agreement no matter how tinykeys treats extra modifiers.
  function onScrubKey(event: KeyboardEvent, delta: -1 | 1): void {
    onArrow(event, (s) =>
      event.shiftKey
        ? grow(s, delta === -1 ? "older" : "newer", versions.length)
        : scrub(s, delta, versions.length),
    );
  }

  onMount(() =>
    tinykeys(window, {
      ArrowLeft: (e) => onScrubKey(e, -1),
      ArrowRight: (e) => onScrubKey(e, 1),
      "Shift+ArrowLeft": (e) => onScrubKey(e, -1),
      "Shift+ArrowRight": (e) => onScrubKey(e, 1),
    }),
  );

  // Keep the focal dot in view while scrubbing a chain wider than the
  // strip.
  let timelineEl = $state<HTMLDivElement | undefined>();

  $effect(() => {
    const s = span;
    if (!s) return;
    tick().then(() => {
      timelineEl
        ?.querySelector(`[data-version-index="${s.to}"]`)
        ?.scrollIntoView({ block: "nearest", inline: "nearest" });
    });
  });

  // ── Restore (the pane's one mutation) ──

  let restoreArmed = $state(false);
  let acting = $state(false);
  let actionError = $state<string | null>(null);

  $effect(() => {
    void spanKey;
    untrack(() => {
      restoreArmed = false;
      actionError = null;
    });
  });

  async function restore() {
    const target = focal;
    if (!target || acting) return;
    acting = true;
    actionError = null;
    try {
      await restoreChangeVersion(node.id, target.commitId);
      restoreArmed = false;
    } catch (err) {
      actionError = api.errorMessage(err);
    } finally {
      acting = false;
    }
  }

  // ── Body rendering ──

  // Per-file collapse, reset when the span moves — another comparison is
  // another read.
  const collapsedFiles = new SvelteSet<string>();

  $effect(() => {
    void spanKey;
    untrack(() => collapsedFiles.clear());
    scroller?.scrollTo({ top: 0 });
  });

  let scroller = $state<HTMLDivElement | undefined>();

  // Collapsing moves everything below; the cards window their rows from
  // scroll geometry, so tell them it changed (same beat as DiffView).
  function toggleFile(path: string) {
    if (!collapsedFiles.delete(path)) collapsedFiles.add(path);
    void tick().then(() => scroller?.dispatchEvent(new Event("scroll")));
  }

  // Publish the on-screen step diff for the context menu's patch copy;
  // the root's data-diff-readonly keeps the menu's mutations off (these
  // are old versions — discard/split would act on current content with
  // stale coordinates). DiffView takes the slot back when the pane
  // closes.
  $effect(() => {
    currentDiff.diff = spanDiff;
  });

  const spanStats = $derived(spanDiff ? totalStats(spanDiff.files) : null);
  const descriptionChanged = $derived(
    span !== null &&
      versions.length > 0 &&
      versions[span.from].description !== versions[span.to].description,
  );

  function dotTitle(i: number): string {
    const v = versions[i];
    const lines = [
      `${v.commitId.slice(0, 8)} · version ${i + 1} of ${versions.length}`,
      v.description.split("\n")[0] || "no description",
    ];
    if (v.opDescription) lines.push(`Recorded by: ${v.opDescription}`);
    lines.push(fromNow(v.opTimestamp ?? v.timestamp));
    lines.push("Click to see this version's rewrite · ⇧-click to widen");
    return lines.join("\n");
  }

  function stepTitle(to: number): string {
    return `What the rewrite from ${versions[to - 1].commitId.slice(0, 8)} to ${versions[to].commitId.slice(0, 8)} changed`;
  }
</script>

<section class="evolution" aria-label="Change evolution" data-diff-readonly>
  <header class="head">
    <Icon name="operations" size={13} />
    <span class="title">Evolution</span>
    <span class="chip mono" title="Every version this change was rewritten through, oldest to newest">
      of <b>{node.id.slice(0, 2)}</b>{node.id.slice(2, 8)}
    </span>
    <span class="hint truncate">
      ←/→ scrub through time · ⇧ widens the span · click any dot
    </span>
    <button class="head-close" title="Back to the diff (esc)" onclick={onclose}>
      <Icon name="close" size={13} />
    </button>
  </header>

  {#if view && versions.length > 0}
    <div class="timeline-strip">
      <div class="timeline" bind:this={timelineEl} role="group" aria-label="Version timeline">
        {#if view.truncated}
          <span
            class="older-note mono"
            title="Older versions exist beyond this list — the walk stopped at its cap"
          >⋯</span>
        {/if}
        {#each versions as v, i (v.commitId)}
          {#if i > 0}
            {@const step = stepDiff(i)}
            {@const inSpan = span !== null && span.from <= i - 1 && i <= span.to}
            <button
              class="segment"
              class:selected={inSpan}
              data-action="evolution-step"
              title={stepTitle(i)}
              onclick={() => pickStep(i)}
            >
              <span class="seg-stats mono">
                {#if step && step.files.length > 0}
                  {@const stats = totalStats(step.files)}
                  <ins>+{stats.added}</ins><del>−{stats.removed}</del>
                {/if}
              </span>
              <span
                class="seg-track"
                class:hollow={step !== null && step.files.length === 0}
              ></span>
            </button>
          {/if}
          {@const endpoint = span !== null && (i === span.from || i === span.to)}
          <button
            class="dot-col"
            class:current={i === versions.length - 1}
            class:endpoint
            class:focal={span !== null && i === span.to}
            data-version-index={i}
            data-action="evolution-version"
            title={dotTitle(i)}
            onclick={(event) => pickVersion(i, event)}
          >
            <span class="dot-cap"></span>
            <span class="dot"></span>
            <span class="dot-label mono">
              {endpoint
                ? v.commitId.slice(0, 8)
                : shortAge(v.opTimestamp ?? v.timestamp)}
            </span>
          </button>
        {/each}
      </div>

      {#if span && focal}
        <div class="caption">
          <span class="cap-position mono">
            v{span.to + 1}<span class="cap-of">/{versions.length}</span>
          </span>
          <span class="cap-span mono">
            {versions[span.from].commitId.slice(0, 8)}
            <span class="cap-arrow">→</span>
            {focal.commitId.slice(0, 8)}
          </span>
          {#if span.to - span.from > 1}
            <span class="cap-note">{span.to - span.from} rewrites as one diff</span>
          {:else if focal.opDescription}
            <span
              class="cap-note truncate"
              title="Recorded by operation {focal.opId}: {focal.opDescription}"
            >{focal.opDescription}</span>
          {/if}
          <span class="cap-age" title={focal.opTimestamp ?? focal.timestamp}>
            {fromNow(focal.opTimestamp ?? focal.timestamp)}
          </span>
          {#if spanStats && spanDiff && spanDiff.files.length > 0}
            <span class="cap-stats mono">
              {spanDiff.files.length}
              {spanDiff.files.length === 1 ? "file" : "files"}
              <ins>+{spanStats.added}</ins>
              <del>−{spanStats.removed}</del>
            </span>
          {/if}
          {#if actionError}
            <span class="cap-error truncate" role="alert" title={actionError}>
              {actionError}
            </span>
          {/if}
          {#if canRestore}
            {#if restoreArmed}
              <span class="restore-confirm" in:panelIn>
                <span class="restore-text">
                  Files and description return to
                  <span class="mono">{focal.commitId.slice(0, 8)}</span>;
                  descendants rebase. Undoable from the op log.
                </span>
                <button
                  class="restore-go"
                  data-action="evolution-restore-confirm"
                  disabled={acting}
                  onclick={restore}
                >
                  {acting ? "Restoring…" : "Restore"}
                </button>
                <button
                  class="restore-cancel"
                  disabled={acting}
                  onclick={() => (restoreArmed = false)}
                >
                  Cancel
                </button>
              </span>
            {:else}
              <button
                class="restore-arm"
                data-action="evolution-restore"
                disabled={acting}
                title="Restore {node.id.slice(0, 4)} to version {focal.commitId.slice(0, 8)}"
                onclick={() => (restoreArmed = true)}
              >
                <Icon name="restore" size={11} />
                Restore this version
              </button>
            {/if}
          {/if}
        </div>
      {/if}
    </div>
  {/if}

  <div class="scroller" bind:this={scroller}>
    {#if error}
      <p class="error" role="alert">{error}</p>
    {:else if !view}
      <div class="skeleton" aria-hidden="true">
        <i></i><i></i><i></i><i></i><i></i>
      </div>
    {:else if versions.length === 1}
      <div class="empty">
        <EmptyState
          icon="operations"
          title="No earlier versions"
          body="This change has never been rewritten — its whole life is the diff you were just reading."
        />
      </div>
    {:else if held === "loading" || held === undefined}
      <div class="skeleton" aria-hidden="true">
        <i></i><i></i><i></i><i></i><i></i>
      </div>
    {:else if "error" in held}
      <p class="error" role="alert">{held.error}</p>
    {:else if spanDiff}
      {#if spanDiff.files.length === 0}
        {#if descriptionChanged && span}
          <div class="desc-delta">
            <p class="note">
              This rewrite changed no files — the description moved:
            </p>
            <div class="desc-pair">
              <div class="desc-side removed">
                <span class="desc-tag">before</span>
                <pre>{versions[span.from].description || "(no description)"}</pre>
              </div>
              <div class="desc-side added">
                <span class="desc-tag">after</span>
                <pre>{versions[span.to].description || "(no description)"}</pre>
              </div>
            </div>
          </div>
        {:else}
          <div class="empty">
            <EmptyState
              icon="commit"
              title="No file changes"
              body="Nothing differs between these versions' trees or descriptions — this rewrite only moved the change (a rebase, or a snapshot that touched nothing here)."
            />
          </div>
        {/if}
      {:else}
        {#each spanDiff.files as file, index (`${spanKey} ${file.path}`)}
          <FileDiffCard
            {file}
            {index}
            {layout}
            collapsed={collapsedFiles.has(file.path)}
            ontoggle={() => toggleFile(file.path)}
          />
        {/each}
        {#if spanDiff.truncated}
          <p class="trailing-note">
            File list truncated — this rewrite touched more files than Jiji
            renders
          </p>
        {/if}
      {/if}
    {/if}
  </div>
</section>

<style>
  .evolution {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .head {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: 34px;
    padding: 0 var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
  }

  .title {
    flex-shrink: 0;
    font-size: var(--text-s);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  .chip {
    flex-shrink: 0;
    height: 17px;
    line-height: 17px;
    font-size: var(--text-xs);
    padding: 0 5px;
    border-radius: var(--radius-s);
    background: color-mix(in srgb, var(--clr-text-1) 7%, transparent);
    color: var(--clr-text-2);
  }

  .chip b {
    color: var(--clr-text-1);
    font-weight: 600;
  }

  .hint {
    flex: 1;
    min-width: 0;
    text-align: right;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    opacity: 0.8;
  }

  .head-close {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-m);
    color: var(--clr-text-3);
    transition: all var(--t-fast) var(--ease-out);
  }

  .head-close:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  /* ── The timeline strip ── */

  .timeline-strip {
    flex-shrink: 0;
    border-bottom: 1px solid var(--clr-border-2);
    background: color-mix(in srgb, var(--clr-bg-0) 40%, var(--clr-bg-1));
  }

  .timeline {
    display: flex;
    align-items: stretch;
    overflow-x: auto;
    padding: var(--sp-3) var(--sp-5) var(--sp-1);
  }

  .older-note {
    align-self: center;
    flex-shrink: 0;
    padding: 0 var(--sp-2);
    margin-bottom: 14px;
    color: var(--clr-text-3);
    cursor: default;
  }

  /* Segments and dots share a three-row grid — stats above, the marker
     line, labels below — so the track always runs through dot centers. */
  .segment,
  .dot-col {
    display: grid;
    grid-template-rows: 15px 14px 16px;
    align-items: center;
    justify-items: center;
  }

  .segment {
    flex: 1 1 0;
    min-width: 36px;
    padding: 0;
    cursor: pointer;
  }

  .seg-stats {
    display: inline-flex;
    gap: 5px;
    font-size: var(--text-xs);
    opacity: 0.85;
  }

  .seg-stats ins {
    text-decoration: none;
    color: var(--diff-add-fg);
  }

  .seg-stats del {
    text-decoration: none;
    color: var(--diff-del-fg);
  }

  .seg-track {
    width: 100%;
    height: 2px;
    border-radius: 1px;
    background: var(--clr-border-1);
    transition: background var(--t-fast) var(--ease-out);
  }

  /* A rewrite that touched no files (a describe, a rebase): a dashed
     whisper of a step. */
  .seg-track.hollow {
    height: 0;
    background: none;
    border-top: 2px dashed var(--clr-border-1);
  }

  .segment:hover .seg-track {
    background: var(--clr-text-3);
  }

  .segment:hover .seg-track.hollow {
    background: none;
    border-top-color: var(--clr-text-3);
  }

  .segment.selected .seg-track {
    height: 3px;
    background: var(--clr-accent);
  }

  .segment.selected .seg-track.hollow {
    height: 0;
    background: none;
    border-top: 3px dashed var(--clr-accent);
  }

  .dot-col {
    flex-shrink: 0;
    padding: 0 2px;
    cursor: pointer;
  }

  .dot-cap {
    /* Row-1 spacer so the grid rows stay uniform. */
    width: 1px;
  }

  .dot {
    grid-row: 2;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--clr-bg-1);
    border: 2px solid var(--clr-border-1);
    transition: all var(--t-fast) var(--ease-out);
  }

  .dot-col:hover .dot {
    border-color: var(--clr-text-3);
  }

  .dot-col.current .dot {
    background: var(--clr-accent);
    border-color: var(--clr-accent);
  }

  .dot-col.endpoint .dot {
    border-color: var(--clr-accent);
    box-shadow: 0 0 0 3px var(--clr-accent-dim);
  }

  .dot-col.focal .dot {
    background: var(--clr-accent-strong);
    border-color: var(--clr-accent-strong);
  }

  .dot-label {
    grid-row: 3;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--clr-text-3);
    white-space: nowrap;
  }

  .dot-col.endpoint .dot-label {
    color: var(--clr-accent-strong);
  }

  /* ── The caption row: what span is on screen, and Restore ── */

  .caption {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 30px;
    padding: 0 var(--sp-4) var(--sp-2);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .cap-position {
    flex-shrink: 0;
    color: var(--clr-text-2);
    font-weight: 600;
  }

  .cap-of {
    color: var(--clr-text-3);
    font-weight: 400;
  }

  .cap-span {
    flex-shrink: 0;
    color: var(--clr-text-2);
  }

  .cap-arrow {
    color: var(--clr-text-3);
    padding: 0 1px;
  }

  .cap-note {
    flex: 0 3 auto;
    min-width: 4ch;
  }

  .cap-age {
    flex-shrink: 0;
  }

  .cap-stats {
    flex-shrink: 0;
    display: inline-flex;
    gap: 5px;
  }

  .cap-stats ins {
    text-decoration: none;
    color: var(--diff-add-fg);
  }

  .cap-stats del {
    text-decoration: none;
    color: var(--diff-del-fg);
  }

  .cap-error {
    flex: 1;
    min-width: 0;
    text-align: right;
    color: var(--clr-danger);
  }

  .restore-arm {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    margin-left: auto;
    height: 22px;
    font-size: var(--text-xs);
    font-weight: 500;
    border-radius: var(--radius-s);
    padding: 0 8px;
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
    transition: background var(--t-fast) var(--ease-out);
  }

  .restore-arm:hover:not(:disabled) {
    background: color-mix(in srgb, var(--clr-accent-strong) 24%, transparent);
  }

  .restore-arm:disabled {
    opacity: 0.6;
  }

  .restore-confirm {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-left: auto;
    min-width: 0;
  }

  .restore-text {
    min-width: 0;
    color: var(--clr-text-2);
  }

  .restore-go {
    flex-shrink: 0;
    height: 22px;
    font-size: var(--text-xs);
    font-weight: 500;
    border-radius: var(--radius-s);
    padding: 0 8px;
    color: var(--clr-accent-contrast);
    background: var(--clr-accent);
  }

  .restore-go:hover:not(:disabled) {
    background: color-mix(in srgb, var(--clr-accent) 88%, var(--clr-text-1));
  }

  .restore-go:disabled,
  .restore-cancel:disabled {
    opacity: 0.6;
  }

  .restore-cancel {
    flex-shrink: 0;
    height: 22px;
    font-size: var(--text-xs);
    border-radius: var(--radius-s);
    padding: 0 8px;
    color: var(--clr-text-1);
    background: var(--clr-bg-3);
    border: 1px solid var(--clr-border-1);
  }

  .restore-cancel:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  /* ── The diff body ── */

  /* overflow-anchor: the cards are windowed; see DiffView's scroller. */
  .scroller {
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow-y: auto;
    overflow-anchor: none;
  }

  .empty {
    height: 100%;
    display: grid;
    place-items: center;
  }

  .desc-delta {
    padding: var(--sp-4);
  }

  .note {
    font-size: var(--text-s);
    color: var(--clr-text-3);
    margin-bottom: var(--sp-3);
  }

  .desc-pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-3);
    max-width: 920px;
  }

  .desc-side {
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-m);
    padding: var(--sp-3);
    min-width: 0;
  }

  .desc-side.removed {
    background: color-mix(in srgb, var(--clr-danger) 5%, transparent);
  }

  .desc-side.added {
    background: color-mix(in srgb, var(--clr-ok) 5%, transparent);
  }

  .desc-tag {
    display: inline-block;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--clr-text-3);
    margin-bottom: var(--sp-2);
  }

  .desc-side pre {
    font: inherit;
    font-size: var(--text-s);
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--clr-text-1);
    margin: 0;
  }

  .trailing-note {
    padding: var(--sp-3) var(--sp-4) var(--sp-6);
    font-size: var(--text-s);
    color: var(--clr-text-3);
    font-style: italic;
  }

  .error {
    padding: var(--sp-4);
    font-size: var(--text-s);
    color: var(--clr-warn);
  }

  .skeleton {
    display: flex;
    flex-direction: column;
    gap: 9px;
    padding: var(--sp-4);
  }

  .skeleton i {
    height: 12px;
    border-radius: 4px;
    background: var(--clr-bg-3);
    animation: pulse 1.1s ease-in-out infinite alternate;
  }

  .skeleton i:nth-child(2) {
    width: 86%;
    animation-delay: 100ms;
  }

  .skeleton i:nth-child(3) {
    width: 64%;
    animation-delay: 200ms;
  }

  .skeleton i:nth-child(4) {
    width: 78%;
    animation-delay: 300ms;
  }

  .skeleton i:nth-child(5) {
    width: 42%;
    animation-delay: 400ms;
  }

  @keyframes pulse {
    from {
      opacity: 0.45;
    }
    to {
      opacity: 1;
    }
  }
</style>
