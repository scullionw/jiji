<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    variant = "secondary",
    disabled = false,
    title,
    onclick,
    children,
  }: {
    variant?: "primary" | "secondary" | "ghost";
    disabled?: boolean;
    title?: string;
    onclick?: (event: MouseEvent) => void;
    children: Snippet;
  } = $props();
</script>

<button class="btn {variant}" {disabled} {title} {onclick}>
  {@render children()}
</button>

<style>
  .btn {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 30px;
    padding: 0 var(--sp-4);
    border-radius: var(--radius-m);
    font-size: var(--text-m);
    font-weight: 500;
    transition:
      background var(--t-fast) var(--ease-out),
      border-color var(--t-fast) var(--ease-out),
      color var(--t-fast) var(--ease-out);
  }

  .btn:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .primary {
    background: var(--clr-accent);
    color: var(--clr-accent-contrast);
  }

  .primary:hover:not(:disabled) {
    background: var(--clr-accent-strong);
  }

  .secondary {
    background: var(--clr-bg-3);
    border: 1px solid var(--clr-border-1);
    color: var(--clr-text-1);
  }

  .secondary:hover:not(:disabled) {
    border-color: var(--clr-border-1);
    /* Mixing toward text-1 lifts in dark and dims in light. */
    background: color-mix(in srgb, var(--clr-bg-3) 94%, var(--clr-text-1));
  }

  .ghost {
    color: var(--clr-text-2);
  }

  .ghost:hover:not(:disabled) {
    background: var(--clr-bg-hover);
    color: var(--clr-text-1);
  }
</style>
