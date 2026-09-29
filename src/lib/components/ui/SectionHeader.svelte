<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { IconName } from "./icons";

  let {
    icon,
    title,
    description,
  }: { icon: IconName; title: string; description: string } = $props();
</script>

<!-- A full-page section's opening: its name and one paragraph of what the
     surface does. The icon echoes the rail, small and quiet. -->
<header class="sec-head">
  <h2>
    <span class="icon"><Icon name={icon} size={16} /></span>
    {title}
  </h2>
  <p>{description}</p>
</header>

<style>
  .sec-head {
    margin-bottom: var(--sp-5);
  }

  h2 {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-title);
    font-weight: 600;
    line-height: 1.3;
    color: var(--clr-text-1);
  }

  .icon {
    display: grid;
    place-items: center;
    color: var(--clr-text-3);
  }

  p {
    margin-top: 4px;
    font-size: var(--text-s);
    line-height: 1.55;
    color: var(--clr-text-2);
    max-width: 64em;
  }
</style>
