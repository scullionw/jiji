<script lang="ts">
  import Icon from "$lib/components/ui/Icon.svelte";
  import { app } from "$lib/state/app.svelte";
  import OperationRow from "./OperationRow.svelte";
  import { groupOperations, opsSince } from "./ops";

  const ops = $derived(app.snapshot?.operations ?? []);
  const groups = $derived(groupOperations(ops));

  // Keys (first op id) of snapshot runs the user opened.
  let expandedRuns = $state<string[]>([]);

  function toggleRun(key: string) {
    expandedRuns = expandedRuns.includes(key)
      ? expandedRuns.filter((k) => k !== key)
      : [...expandedRuns, key];
  }

  // One open plan panel across the whole timeline, like the workbench's
  // single confirm state.
  let confirm = $state<{ opId: string; action: "restore" | "revert" } | null>(
    null,
  );

  function toggleConfirm(opId: string, action: "restore" | "revert" | null) {
    confirm = action === null ? null : { opId, action };
  }

  function rowProps(opId: string) {
    return {
      restoreCount: opsSince(ops, opId)?.length ?? null,
      confirm: confirm?.opId === opId ? confirm.action : null,
      onToggle: (action: "restore" | "revert" | null) =>
        toggleConfirm(opId, action),
    };
  }
</script>

<div class="view">
  <div class="column">
    <header class="head">
      <h2>Operations</h2>
      <p>
        Everything that has happened to this repository, newest first. Every
        entry is a complete repo state: restore the repo to any point, or
        revert a single operation — time travel is just another operation,
        so it can always be undone again.
      </p>
    </header>

    {#each groups as group (group.label)}
      <section class="day">
        <div class="day-head">
          <span class="day-label">{group.label}</span>
        </div>
        {#each group.rows as row, i (row.kind === "op" ? row.op.id : row.key)}
          {@const lineUp = i > 0}
          {@const lineDown = i < group.rows.length - 1}
          {#if row.kind === "op"}
            <OperationRow op={row.op} {lineUp} {lineDown} {...rowProps(row.op.id)} />
          {:else}
            {@const expanded = expandedRuns.includes(row.key)}
            <button
              class="run-toggle"
              class:expanded
              aria-expanded={expanded}
              onclick={() => toggleRun(row.key)}
            >
              <span class="rail" class:line-up={lineUp} class:line-down={lineDown || expanded}>
                <span class="glyph mono">~</span>
              </span>
              <span class="run-label">
                {row.ops.length} working-copy snapshots
              </span>
              <span class="chevron" class:open={expanded}>
                <Icon name="chevronRight" size={11} />
              </span>
            </button>
            {#if expanded}
              {#each row.ops as op, j (op.id)}
                <OperationRow
                  {op}
                  lineUp={true}
                  lineDown={j < row.ops.length - 1 || lineDown}
                  {...rowProps(op.id)}
                />
              {/each}
            {/if}
          {/if}
        {/each}
      </section>
    {/each}

    {#if ops.length >= 50}
      <p class="cap-note">Showing the 50 most recent operations.</p>
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

  .day-head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) 0;
    background: var(--clr-bg-1);
  }

  .day-head::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--clr-border-2);
  }

  .day-label {
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--clr-text-3);
  }

  /* Collapsed snapshot runs read like jj's elided-revisions rows. */
  .run-toggle {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    text-align: left;
    border-radius: var(--radius-s);
    transition: background var(--t-fast) var(--ease-out);
  }

  .run-toggle:hover {
    background: var(--clr-bg-hover);
  }

  .run-toggle .rail {
    position: relative;
    flex-shrink: 0;
    width: 22px;
    align-self: stretch;
    display: flex;
    justify-content: center;
  }

  .run-toggle .rail::before,
  .run-toggle .rail::after {
    content: "";
    position: absolute;
    left: 50%;
    width: 1.5px;
    transform: translateX(-50%);
    background: color-mix(in srgb, var(--clr-text-3) 38%, transparent);
    opacity: 0;
  }

  .run-toggle .rail::before {
    top: 0;
    height: 7px;
  }

  .run-toggle .rail::after {
    top: 21px;
    bottom: 0;
  }

  .run-toggle .rail.line-up::before {
    opacity: 1;
  }

  .run-toggle .rail.line-down::after {
    opacity: 1;
  }

  .run-toggle .glyph {
    position: relative;
    z-index: 1;
    margin-top: 4px;
    font-size: var(--text-s);
    color: var(--clr-text-3);
    background: var(--clr-bg-1);
    line-height: 1.6;
  }

  .run-label {
    padding: 5px 0;
    font-size: var(--text-s);
    color: var(--clr-text-3);
    font-style: italic;
  }

  .chevron {
    display: inline-flex;
    color: var(--clr-text-3);
    transition: transform var(--t-fast) var(--ease-out);
  }

  .chevron.open {
    transform: rotate(90deg);
  }

  .cap-note {
    margin-top: var(--sp-4);
    padding-left: 30px;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
