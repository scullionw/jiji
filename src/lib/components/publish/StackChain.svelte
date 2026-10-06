<script lang="ts">
  import { openUrl } from "@tauri-apps/plugin-opener";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { jumpToChange } from "$lib/state/actions";
  import type { LandSegment } from "$lib/bindings/LandSegment";
  import type { PublishStack } from "./stacks";

  // The stack as a ledger: one row per segment in landing order, the
  // base branch at the top, a hairline rail threading the positions.
  // Rows carry their PR's facts (number, checks, review) and open their
  // changes inline; the land plan or job overlays each row's landing
  // status (merged, next, stacked) when one is live.
  let {
    stack,
    landing = [],
  }: { stack: PublishStack; landing?: LandSegment[] } = $props();
  let expanded = $state<Set<string>>(new Set());
  function toggle(key: string) {
    const next = new Set(expanded);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expanded = next;
  }

  const total = $derived(stack.segments.length);
  const mergedCount = $derived(
    landing.filter((segment) => segment.status.kind === "merged").length,
  );
</script>

<section class="chain" aria-label="Stack in landing order" data-stack-chain>
  <div class="chain-head">
    <span class="chain-title">Landing order</span>
    <span class="chain-progress">
      {#if stack.prCount}
        {mergedCount} of {total} merged
      {:else}
        {total} segment{total === 1 ? "" : "s"}
      {/if}
    </span>
  </div>
  <ol class="ledger">
    <li class="base">
      <span class="rail"
        ><span class="pos base-pos"><Icon name="branch" size={11} /></span
        ></span
      >
      <div class="base-body">
        <span class="ref trunk"
          ><span class="truncate">{stack.base || "trunk"}</span></span
        >
        <span class="base-label">target branch</span>
      </div>
    </li>
    {#each stack.segments as segment, index (segment.bookmark ?? index)}
      {@const key = segment.bookmark ?? String(index)}
      {@const planned = landing.find(
        (item) => item.bookmark === segment.bookmark,
      )}
      {@const status = planned?.status}
      {@const mergedPr = status?.kind === "merged" ? status : null}
      {@const pr = planned?.pr ?? segment.pr}
      {@const merged = mergedPr != null}
      {@const next = status?.kind === "landing" || status?.kind === "waiting"}
      {@const open = expanded.has(key)}
      <li
        class="seg"
        class:next
        class:merged
        data-publish-segment={segment.bookmark ?? "local"}
      >
        <span class="rail">
          <span class="pos"
            >{#if merged}<Icon name="check" size={11} />{:else}{index +
                1}{/if}</span
          >
        </span>
        <div class="seg-body">
          <div class="seg-line">
            {#if mergedPr}
              <button
                class="pr-num mono"
                title="Open pull request #{mergedPr.number} on GitHub"
                onclick={() => openUrl(mergedPr.url)}>#{mergedPr.number}</button
              >
            {:else if pr}
              <button
                class="pr-num mono"
                title="Open pull request #{pr.number} on GitHub"
                onclick={() => openUrl(pr.url)}>#{pr.number}</button
              >
            {/if}
            <span class="seg-title">{pr?.title ?? segment.title}</span>
            {#if merged}
              <span class="seg-state ok">Merged</span>
            {:else if next}
              <span class="seg-state up-next">Next to land</span>
            {/if}
          </div>
          <div class="seg-meta">
            {#if segment.bookmark}
              <span class="ref" title={segment.bookmark}
                ><span class="truncate">{segment.bookmark}</span></span
              >
            {:else}
              <span
                class="new-branch"
                title="Branch name is chosen when publishing">new branch</span
              >
            {/if}
            {#if pr && !merged}
              {#if pr.isDraft}
                <span class="signal muted"
                  ><Icon name="edit" size={11} />Draft</span
                >
              {/if}
              {#if pr.checks !== "none"}
                <span
                  class="signal"
                  class:ok={pr.checks === "passing"}
                  class:danger={pr.checks === "failing"}
                  class:warn={pr.checks === "pending"}
                >
                  <Icon
                    name={pr.checks === "passing"
                      ? "check"
                      : pr.checks === "failing"
                        ? "close"
                        : "operations"}
                    size={11}
                  />
                  {pr.checks === "passing"
                    ? "Checks passed"
                    : pr.checks === "failing"
                      ? "Checks failed"
                      : "Checks running"}
                </span>
              {/if}
              {#if pr.review !== "none"}
                <span
                  class="signal"
                  class:ok={pr.review === "approved"}
                  class:danger={pr.review === "changesRequested"}
                >
                  <Icon
                    name={pr.review === "approved" ? "badgeCheck" : "eye"}
                    size={11}
                  />
                  {pr.review === "approved"
                    ? "Approved"
                    : pr.review === "changesRequested"
                      ? "Changes requested"
                      : "Review needed"}
                </span>
              {/if}
            {:else if !pr && !merged}
              <span class="signal muted">Not published</span>
            {/if}
            {#if segment.changes.length}
              <button
                class="changes-toggle"
                aria-expanded={open}
                onclick={() => toggle(key)}
                >{segment.changes.length} change{segment.changes.length === 1
                  ? ""
                  : "s"}<Icon
                  name={open ? "chevronDown" : "chevronRight"}
                  size={11}
                /></button
              >
            {/if}
          </div>
          {#if open}
            <ul class="changes">
              {#each segment.changes as change (change.id)}
                <li>
                  <button
                    onclick={() => jumpToChange(change.id)}
                    title="Review this change on the workbench"
                    ><Icon name="commit" size={13} /><span class="truncate"
                      >{change.description.split("\n")[0] ||
                        "Untitled change"}</span
                    ><span class="change-id mono"
                      >{change.changeId.slice(0, 8)}</span
                    ></button
                  >
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      </li>
    {/each}
  </ol>
</section>

<style>
  .chain {
    min-width: 0;
  }

  /* The ledger's own toolbar: same height and hairline as the action
     column's head beside it, so one line runs across the detail. */
  .chain-head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--toolbar-h);
    padding: 0 var(--sp-4);
    border-bottom: 1px solid var(--clr-border-2);
    background: var(--clr-bg-1);
    font-size: var(--text-s);
  }

  .chain-title {
    font-weight: 600;
    color: var(--clr-text-2);
  }

  .chain-progress {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .ledger {
    list-style: none;
    margin: 0;
    padding: var(--sp-1) var(--sp-4) var(--sp-6);
  }

  .base,
  .seg {
    position: relative;
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr);
    column-gap: var(--sp-3);
  }

  /* The rail: one hairline threading every position marker, from the
     base branch to the last segment. */
  .rail {
    position: relative;
    display: flex;
    justify-content: center;
    align-items: flex-start;
  }

  .rail::before {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    left: 50%;
    width: 1px;
    background: var(--clr-border-1);
  }

  .base .rail::before {
    top: 50%;
  }

  .seg:last-child .rail::before {
    bottom: calc(100% - 20px);
  }

  .pos {
    position: relative;
    z-index: 1;
    width: 18px;
    height: 18px;
    display: grid;
    place-items: center;
    border: 1px solid var(--clr-border-1);
    border-radius: 50%;
    background: var(--clr-bg-1);
    color: var(--clr-text-3);
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 500;
    line-height: 1;
  }

  .base {
    height: 34px;
    align-items: center;
  }

  .base .rail {
    align-items: center;
    height: 100%;
  }

  .base-body {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .ref.trunk {
    max-width: 24em;
    color: var(--clr-accent-strong);
    background: var(--clr-accent-dim);
  }

  .seg .rail {
    padding-top: 11px;
  }

  .seg-body {
    min-width: 0;
    padding: 10px 0;
    border-top: 1px solid var(--clr-border-2);
  }

  .seg-line {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-height: 20px;
    line-height: 20px;
  }

  .pr-num {
    flex-shrink: 0;
    font-size: var(--text-s);
    color: var(--clr-text-2);
    transition: color var(--t-fast) var(--ease-out);
  }

  .pr-num:hover {
    color: var(--clr-accent-strong);
    text-decoration: underline;
  }

  .seg-title {
    flex: 1;
    min-width: 0;
    font-size: var(--text-m);
    font-weight: 500;
    color: var(--clr-text-1);
    overflow-wrap: anywhere;
  }

  .next .seg-title {
    font-weight: 600;
  }

  .merged .seg-title {
    font-weight: 400;
    color: var(--clr-text-2);
  }

  .seg-state {
    flex-shrink: 0;
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .seg-state.up-next {
    color: var(--clr-accent-strong);
  }

  .seg-state.ok {
    color: var(--clr-ok);
  }

  .next .pos {
    border-color: var(--clr-accent);
    background: var(--clr-accent);
    color: var(--clr-accent-contrast);
  }

  .merged .pos {
    border-color: color-mix(in srgb, var(--clr-ok) 55%, transparent);
    color: var(--clr-ok);
  }

  .seg-meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px var(--sp-3);
    min-height: 18px;
    margin-top: 3px;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .seg-meta .ref {
    max-width: 22em;
  }

  .new-branch {
    font-style: italic;
  }

  .signal {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }

  .signal :global(svg) {
    flex-shrink: 0;
  }

  .signal.ok {
    color: var(--clr-ok);
  }

  .signal.warn {
    color: var(--clr-warn);
  }

  .signal.danger {
    color: var(--clr-danger);
  }

  .signal.muted {
    color: var(--clr-text-3);
  }

  .changes-toggle {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    margin-left: auto;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    transition: color var(--t-fast) var(--ease-out);
  }

  .changes-toggle:hover,
  .changes-toggle[aria-expanded="true"] {
    color: var(--clr-text-1);
  }

  .changes {
    list-style: none;
    margin: var(--sp-2) 0 0;
    padding: var(--sp-1) 0 0;
    border-top: 1px solid var(--clr-border-2);
  }

  .changes button {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: 24px;
    padding: 0 var(--sp-1);
    border-radius: var(--radius-s);
    text-align: left;
    font-size: var(--text-s);
    color: var(--clr-text-2);
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .changes button:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .changes button :global(svg) {
    flex-shrink: 0;
    color: var(--clr-text-3);
  }

  .changes .truncate {
    flex: 1;
    min-width: 0;
  }

  .change-id {
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
