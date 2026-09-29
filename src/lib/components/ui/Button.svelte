<script lang="ts">
  import type { Snippet } from "svelte";

  // The material lives in global.css (.btn and its variants), shared with
  // the plain <button class="btn"> controls elsewhere in the app.
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
