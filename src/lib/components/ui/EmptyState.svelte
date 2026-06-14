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

<div class="empty">
  <div class="glyph"><Icon name={icon} size={22} /></div>
  <h3>{title}</h3>
  <p>{body}</p>
  {#if hint}
    <span class="hint">{hint}</span>
  {/if}
</div>

<style>
  .empty {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-3);
    text-align: center;
    padding: var(--sp-8);
  }

  .glyph {
    width: 54px;
    height: 54px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: var(--clr-bg-2);
    border: 1px solid var(--clr-border-2);
    color: var(--clr-text-3);
    margin-bottom: var(--sp-1);
  }

  h3 {
    font-size: var(--text-l);
    font-weight: 550;
    color: var(--clr-text-1);
  }

  p {
    font-size: var(--text-m);
    color: var(--clr-text-2);
    max-width: 380px;
  }

  .hint {
    margin-top: var(--sp-2);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--clr-text-3);
    border: 1px solid var(--clr-border-2);
    border-radius: 999px;
    padding: 3px 10px;
  }
</style>
