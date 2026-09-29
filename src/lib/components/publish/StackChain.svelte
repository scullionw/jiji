<script lang="ts">
  import { openUrl } from "@tauri-apps/plugin-opener";
  import Icon from "$lib/components/ui/Icon.svelte";
  import { jumpToChange } from "$lib/state/actions";
  import type { LandSegment } from "$lib/bindings/LandSegment";
  import type { PublishStack } from "./stacks";

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
</script>

<section class="chain" aria-label="Stack in landing order" data-stack-chain>
  <div class="chain-heading">
    <span>Landing order</span><span>Base → head</span>
  </div>
  <div class="base">
    <span class="base-icon"><Icon name="branch" size={14} /></span><span
      class="mono">{stack.base || "trunk"}</span
    ><span class="base-label">target branch</span>
  </div>
  <ol>
    {#each stack.segments as segment, index (segment.bookmark ?? index)}
      {@const key = segment.bookmark ?? String(index)}
      {@const planned = landing.find(
        (item) => item.bookmark === segment.bookmark,
      )}
      {@const status = planned?.status.kind}
      {@const pr = planned?.pr ?? segment.pr}
      {@const merged = status === "merged"}
      {@const next = status === "landing" || status === "waiting"}
      <li
        data-publish-segment={segment.bookmark ?? "local"}
        class:next
        class:merged
      >
        <div class="rail">
          <span class="position"
            >{#if merged}<Icon name="check" size={13} />{:else}{index +
                1}{/if}</span
          >
        </div>
        <article class="segment-card">
          <div class="card-top">
            <span class="pr-kind">
              <Icon name="branch" size={13} />
              {pr
                ? merged
                  ? "Merged"
                  : pr.isDraft
                    ? "Draft pull request"
                    : "Pull request"
                : "Unpublished"}
            </span>
            {#if next}<span class="next-label">Up next</span>{/if}
            {#if pr}
              <button
                class="pr-link mono"
                title="Open pull request #{pr.number} on GitHub"
                onclick={() => openUrl(pr.url)}
                >#{pr.number} <span>↗</span></button
              >
            {/if}
          </div>
          <h3>{pr?.title ?? segment.title}</h3>
          <div class="branch-row">
            <span
              class="branch-name mono"
              title={segment.bookmark ??
                "Branch name is chosen when publishing"}
              >{segment.bookmark ?? "New branch"}</span
            >
            {#if segment.changes.length}
              <button
                class="changes"
                aria-expanded={expanded.has(key)}
                onclick={() => toggle(key)}
                >{segment.changes.length} change{segment.changes.length === 1
                  ? ""
                  : "s"}<Icon
                  name={expanded.has(key) ? "chevronDown" : "chevronRight"}
                  size={12}
                /></button
              >
            {/if}
          </div>
          {#if pr && !merged}
            <div class="signals">
              {#if pr.checks !== "none"}
                <span
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
                    size={12}
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
                  class:ok={pr.review === "approved"}
                  class:danger={pr.review === "changesRequested"}
                >
                  <Icon
                    name={pr.review === "approved" ? "check" : "eye"}
                    size={12}
                  />
                  {pr.review === "approved"
                    ? "Approved"
                    : pr.review === "changesRequested"
                      ? "Changes requested"
                      : "Review needed"}
                </span>
              {/if}
            </div>
          {/if}
          {#if expanded.has(key)}
            <div class="commits">
              {#each segment.changes as change (change.id)}
                <button
                  onclick={() => jumpToChange(change.id)}
                  title="Review this change on the workbench"
                  ><Icon name="commit" size={14} /><span
                    >{change.description.split("\n")[0] ||
                      "Untitled change"}</span
                  ><span class="mono">{change.changeId.slice(0, 8)}</span
                  ></button
                >
              {/each}
            </div>
          {/if}
        </article>
      </li>
    {/each}
  </ol>
</section>

<style>
  .chain {
    min-width: 0;
  }

  .chain-heading {
    display: flex;
    justify-content: space-between;
    padding: 0 0 var(--sp-2);
    color: var(--clr-text-3);
    font-size: var(--text-xs);
  }

  .chain-heading > :first-child {
    font-weight: 600;
  }

  .base {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 28px;
    margin-bottom: var(--sp-2);
    color: var(--clr-text-2);
    font-size: var(--text-s);
  }

  .base-icon {
    width: 24px;
    display: grid;
    place-items: center;
    color: var(--clr-text-3);
  }

  .base-label {
    color: var(--clr-text-3);
    font-size: var(--text-xs);
  }

  ol {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  li {
    position: relative;
    display: grid;
    grid-template-columns: 24px minmax(0, 1fr);
    gap: var(--sp-3);
    padding-bottom: var(--sp-2);
  }

  .rail {
    position: relative;
    display: flex;
    justify-content: center;
    padding-top: 14px;
  }

  .rail::before {
    content: "";
    position: absolute;
    top: -30px;
    bottom: -20px;
    width: 1px;
    background: var(--clr-border-1);
  }

  li:last-child .rail::before {
    bottom: calc(100% - 26px);
  }

  .position {
    position: relative;
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    border: 1px solid var(--clr-border-1);
    border-radius: 50%;
    background: var(--clr-bg-1);
    color: var(--clr-text-3);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }

  .next .position {
    border-color: var(--clr-accent);
    color: var(--clr-accent-contrast);
    background: var(--clr-accent);
  }

  .merged .position {
    color: var(--clr-ok);
    border-color: var(--clr-ok);
  }

  .segment-card {
    min-width: 0;
    padding: 10px 14px;
    border: 1px solid var(--clr-border-2);
    border-radius: var(--radius-l);
  }

  .next .segment-card {
    border-color: color-mix(in srgb, var(--clr-accent) 45%, var(--clr-border-2));
  }

  .card-top {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    margin-bottom: 4px;
  }

  .pr-kind {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }

  .next-label {
    font-size: var(--text-xs);
    color: var(--clr-accent-strong);
    font-weight: 600;
  }

  .pr-link {
    margin-left: auto;
    color: var(--clr-accent-strong);
    font-size: var(--text-s);
    flex-shrink: 0;
  }

  .pr-link span {
    opacity: 0.65;
  }

  .pr-link:hover {
    text-decoration: underline;
  }

  h3 {
    font-size: var(--text-m);
    line-height: 1.45;
    font-weight: 600;
    margin: 0 0 4px;
    color: var(--clr-text-1);
    overflow-wrap: anywhere;
  }

  .branch-row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }

  .branch-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--clr-text-3);
    font-size: var(--text-xs);
  }

  .changes {
    display: flex;
    align-items: center;
    gap: 3px;
    margin-left: auto;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .changes:hover {
    color: var(--clr-text-1);
  }

  .signals {
    display: flex;
    gap: var(--sp-4);
    flex-wrap: wrap;
    margin-top: 6px;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  .signals > span {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .ok {
    color: var(--clr-ok);
  }

  .warn {
    color: var(--clr-warn);
  }

  .danger {
    color: var(--clr-danger);
  }

  .commits {
    border-top: 1px solid var(--clr-border-2);
    margin-top: var(--sp-2);
    padding-top: 2px;
  }

  .commits button {
    display: flex;
    align-items: center;
    text-align: left;
    gap: var(--sp-2);
    width: 100%;
    height: 26px;
    color: var(--clr-text-2);
    font-size: var(--text-s);
  }

  .commits button:hover {
    color: var(--clr-accent-strong);
  }

  .commits button > span:first-of-type {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .commits .mono {
    color: var(--clr-text-3);
    font-size: var(--text-xs);
  }

  .mono {
    font-family: var(--font-mono);
  }
</style>
