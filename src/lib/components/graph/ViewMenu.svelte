<script lang="ts">
  // The workbench's lens picker: which slice of the repo the graph draws.
  // Presets are one click; the revset input takes anything jj's `-r` would,
  // with the backend's refusal rendered inline; a typed revset can be saved
  // as a named per-repo view (Tauri store). Same anchored-panel shape as
  // the theme menu.

  import { tick } from "svelte";
  import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
  import Icon from "$lib/components/ui/Icon.svelte";
  import {
    VIEW_PRESETS,
    deleteSavedView,
    renameSavedView,
    sameView,
    upsertSavedView,
    viewLabel,
    type SavedView,
  } from "./views";
  import * as api from "$lib/api";
  import { setSnapshotView } from "$lib/state/actions";
  import { loadSavedViews, persistSavedViews } from "$lib/state/savedviews";

  let { snapshot }: { snapshot: RepoSnapshot } = $props();

  let open = $state(false);
  let anchor: HTMLDivElement | undefined = $state();
  let busy = $state(false);
  let error = $state<string | null>(null);
  let revsetInput = $state("");

  let savedViews = $state<SavedView[]>([]);
  let saving = $state(false);
  let nameInput = $state("");
  let nameEl: HTMLInputElement | undefined = $state();
  let renaming = $state<string | null>(null);
  let renameInput = $state("");
  let renameEl: HTMLInputElement | undefined = $state();

  const current = $derived(snapshot.view);
  const streamCount = $derived(snapshot.workstreams.length);
  // The trigger reads as the pane label: the workstream count on the
  // default lens (what it always said), the lens name on any other — a
  // custom lens wears its saved name when it has one.
  const triggerLabel = $derived(
    current.kind === "workstreams"
      ? streamCount === 0
        ? "No workstreams"
        : `${streamCount} workstream${streamCount === 1 ? "" : "s"}`
      : current.kind === "custom"
        ? (savedViews.find((view) => view.revset === current.revset)?.name ??
          viewLabel(current))
        : viewLabel(current),
  );

  // Loaded outside the panel too, so the trigger can name a saved lens.
  $effect(() => {
    const path = snapshot.repoPath;
    loadSavedViews(path)
      .then((views) => {
        if (path === snapshot.repoPath) savedViews = views;
      })
      .catch(() => {
        savedViews = [];
      });
  });

  $effect(() => {
    if (saving) void tick().then(() => nameEl?.focus());
  });

  $effect(() => {
    if (renaming !== null) void tick().then(() => renameEl?.focus());
  });

  function toggle() {
    open = !open;
    if (open) {
      error = null;
      saving = false;
      renaming = null;
      revsetInput = current.kind === "custom" ? current.revset : "";
    }
  }

  async function apply(view: typeof current) {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await setSnapshotView(view);
      open = false;
    } catch (err) {
      error = api.errorMessage(err);
    } finally {
      busy = false;
    }
  }

  function applyRevset() {
    const revset = revsetInput.trim();
    if (revset === "") return;
    void apply({ kind: "custom", revset });
  }

  async function persistViews(next: SavedView[]) {
    savedViews = next;
    try {
      await persistSavedViews(snapshot.repoPath, next);
    } catch (err) {
      error = api.errorMessage(err);
    }
  }

  function commitSave() {
    const next = upsertSavedView(savedViews, nameInput, revsetInput);
    if (!next) return;
    error = null;
    saving = false;
    nameInput = "";
    void persistViews(next);
  }

  function startRename(view: SavedView) {
    error = null;
    renaming = view.name;
    renameInput = view.name;
  }

  function commitRename() {
    if (renaming === null || renameInput.trim() === "") return;
    error = null;
    const next = renameSavedView(savedViews, renaming, renameInput);
    if (!next) {
      error = `A saved view named “${renameInput.trim()}” already exists.`;
      return;
    }
    renaming = null;
    void persistViews(next);
  }

  function removeView(view: SavedView) {
    if (renaming === view.name) renaming = null;
    void persistViews(deleteSavedView(savedViews, view.name));
  }

  /** Inline editors swallow Escape so it cancels them, not the panel. */
  function escapeCancels(cancel: () => void) {
    return (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        cancel();
      }
    };
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (open && event.key === "Escape") open = false;
  }

  function onWindowMousedown(event: MouseEvent) {
    if (open && anchor && !anchor.contains(event.target as Node)) {
      open = false;
    }
  }
</script>

<svelte:window onkeydown={onWindowKeydown} onmousedown={onWindowMousedown} />

<div class="anchor" bind:this={anchor}>
  <button
    class="trigger"
    class:open
    class:lensed={current.kind !== "workstreams"}
    title="Change what the graph shows"
    aria-expanded={open}
    onclick={toggle}
  >
    <span class="truncate">{triggerLabel}</span>
    <Icon name="chevronDown" size={12} />
  </button>

  {#if open}
    <div class="panel popover" role="dialog" aria-label="Graph view">
      <div class="menu-label">Show</div>
      {#each VIEW_PRESETS as preset (preset.view.kind)}
        <button
          class="preset"
          class:selected={sameView(current, preset.view)}
          disabled={busy}
          onclick={() => void apply(preset.view)}
        >
          <span class="preset-text">
            <span class="preset-label">{preset.label}</span>
            <span class="preset-desc">{preset.description}</span>
          </span>
          {#if sameView(current, preset.view)}
            <span class="check"><Icon name="check" size={13} /></span>
          {/if}
        </button>
      {/each}

      {#if savedViews.length > 0}
        <div class="menu-label">Saved</div>
        {#each savedViews as view (view.name)}
          {#if renaming === view.name}
            <form
              class="inline-form"
              onsubmit={(event) => {
                event.preventDefault();
                commitRename();
              }}
            >
              <input
                class="field"
                type="text"
                placeholder="View name"
                spellcheck="false"
                bind:value={renameInput}
                bind:this={renameEl}
                onkeydown={escapeCancels(() => (renaming = null))}
              />
              <button
                type="submit"
                class="btn apply"
                disabled={renameInput.trim() === ""}
              >
                Rename
              </button>
            </form>
          {:else}
            <div class="saved-row">
              <button
                class="preset saved"
                class:selected={sameView(current, {
                  kind: "custom",
                  revset: view.revset,
                })}
                disabled={busy}
                onclick={() =>
                  void apply({ kind: "custom", revset: view.revset })}
              >
                <span class="preset-text">
                  <span class="preset-label">{view.name}</span>
                  <span class="preset-desc revset-desc">{view.revset}</span>
                </span>
                {#if sameView(current, { kind: "custom", revset: view.revset })}
                  <span class="check"><Icon name="check" size={13} /></span>
                {/if}
              </button>
              <button
                class="row-action"
                title="Rename view"
                aria-label={`Rename view ${view.name}`}
                onclick={() => startRename(view)}
              >
                <Icon name="edit" size={12} />
              </button>
              <button
                class="row-action danger"
                title="Delete view"
                aria-label={`Delete view ${view.name}`}
                onclick={() => removeView(view)}
              >
                <Icon name="trash" size={12} />
              </button>
            </div>
          {/if}
        {/each}
      {/if}

      <div class="menu-label">Revset</div>
      <form
        class="revset"
        onsubmit={(event) => {
          event.preventDefault();
          applyRevset();
        }}
      >
        <input
          class="field mono"
          type="text"
          placeholder={'e.g. mine() & description("wip")'}
          spellcheck="false"
          disabled={busy}
          bind:value={revsetInput}
        />
        <button
          type="submit"
          class="btn apply"
          disabled={busy || revsetInput.trim() === ""}
        >
          Show
        </button>
      </form>
      {#if saving}
        <form
          class="inline-form"
          onsubmit={(event) => {
            event.preventDefault();
            commitSave();
          }}
        >
          <input
            class="field"
            type="text"
            placeholder="Name this view"
            spellcheck="false"
            bind:value={nameInput}
            bind:this={nameEl}
            onkeydown={escapeCancels(() => (saving = false))}
          />
          <button
            type="submit"
            class="btn apply"
            disabled={nameInput.trim() === "" || revsetInput.trim() === ""}
          >
            Save
          </button>
        </form>
      {:else if revsetInput.trim() !== ""}
        <button class="save-as" onclick={() => (saving = true)}>
          Save as view…
        </button>
      {/if}
      {#if error}
        <p class="error">{error}</p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .anchor {
    position: relative;
    flex: 1;
    min-width: 0;
  }

  .trigger {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 100%;
    height: 24px;
    padding: 0 6px;
    margin-left: -6px;
    border-radius: var(--radius-m);
    font-size: var(--text-m);
    font-weight: 600;
    color: var(--clr-text-1);
    transition: background var(--t-fast) var(--ease-out);
  }

  .trigger :global(svg) {
    flex-shrink: 0;
    color: var(--clr-text-3);
  }

  .trigger:hover,
  .trigger.open {
    background: var(--clr-bg-hover);
  }

  /* A non-default lens is a mode the user should notice they are in. */
  .trigger.lensed,
  .trigger.lensed :global(svg) {
    color: var(--clr-accent-strong);
  }

  .panel {
    position: absolute;
    top: calc(100% + 6px);
    left: -6px;
    z-index: 50;
    width: 296px;
    padding: var(--sp-1);
  }

  .preset {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    padding: 4px var(--sp-2);
    border-radius: var(--radius-s);
    text-align: left;
    transition: background var(--t-fast) var(--ease-out);
  }

  .preset:hover:not(:disabled) {
    background: var(--clr-bg-hover);
  }

  .preset:disabled {
    opacity: 0.6;
  }

  .preset-text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .preset-label {
    font-size: var(--text-m);
    color: var(--clr-text-1);
  }

  .preset-desc {
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }

  /* A saved view's one-liner is its revset, not prose. */
  .revset-desc {
    font-family: var(--font-mono);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .check {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    color: var(--clr-accent);
  }

  .saved-row {
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .saved-row .preset {
    flex: 1;
    min-width: 0;
  }

  .row-action {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-s);
    color: var(--clr-text-3);
    opacity: 0;
    transition:
      background var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out),
      opacity var(--t-fast) var(--ease-out);
  }

  .saved-row:hover .row-action,
  .saved-row:focus-within .row-action {
    opacity: 1;
  }

  .row-action:hover {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }

  .row-action.danger:hover {
    color: var(--clr-danger);
  }

  .revset,
  .inline-form {
    display: flex;
    gap: var(--sp-1);
    padding: 0 var(--sp-2) var(--sp-1);
  }

  .inline-form {
    padding-top: var(--sp-1);
  }

  .revset input,
  .inline-form input {
    flex: 1;
    min-width: 0;
    height: 26px;
  }

  .apply {
    height: 26px;
  }

  .save-as {
    display: block;
    margin: 0 var(--sp-2) var(--sp-1);
    padding: 2px 0;
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    transition: color var(--t-fast) var(--ease-out);
  }

  .save-as:hover {
    color: var(--clr-accent-strong);
  }

  .error {
    margin: var(--sp-1) var(--sp-2) var(--sp-1);
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--clr-danger);
    overflow-wrap: anywhere;
    /* Revset parse errors arrive as jj's multi-line caret diagram. */
    white-space: pre-wrap;
    font-family: var(--font-mono);
    max-height: 120px;
    overflow-y: auto;
  }
</style>
