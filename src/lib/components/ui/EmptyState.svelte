<script lang="ts">
  import { fade } from "svelte/transition";
  import Icon from "./Icon.svelte";
  import type { IconName } from "./icons";
  import { motionMs } from "$lib/motion";

  let {
    icon,
    title,
    body,
    hint,
  }: { icon: IconName; title: string; body: string; hint?: string } = $props();
</script>

<!-- Empty states often replace a skeleton or a list that just drained;
     the soft arrival keeps that swap from reading as a flash. -->
<div class="empty" in:fade={{ duration: motionMs(140) }}>
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
    width: 56px;
    height: 56px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-xl);
    background: var(--clr-accent-dim);
    color: var(--clr-accent-strong);
    margin-bottom: var(--sp-1);
  }

  h3 {
    font-size: var(--text-l);
    font-weight: 650;
    letter-spacing: -0.01em;
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
