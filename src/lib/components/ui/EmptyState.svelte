<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { IconName } from "./icons";

  let {
    icon,
    title,
    body,
    hint,
  }: { icon: IconName; title: string; body: string; hint?: string } = $props();
</script>

<!-- A quiet placeholder for a surface with nothing on it: the glyph in
     tertiary ink, one line of what, one of why, and the jj command that
     would change it. -->
<div class="empty">
  <span class="glyph"><Icon name={icon} size={20} /></span>
  <h3>{title}</h3>
  <p>{body}</p>
  {#if hint}
    <span class="hint mono">{hint}</span>
  {/if}
</div>

<style>
  .empty {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    text-align: center;
    padding: var(--sp-8);
  }

  .glyph {
    display: grid;
    place-items: center;
    margin-bottom: var(--sp-2);
    color: var(--clr-text-3);
  }

  h3 {
    font-size: var(--text-m);
    font-weight: 600;
    color: var(--clr-text-1);
  }

  p {
    font-size: var(--text-s);
    line-height: 1.55;
    color: var(--clr-text-2);
    max-width: 380px;
  }

  .hint {
    margin-top: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
  }
</style>
