<script lang="ts">
  import EmptyState from "$lib/components/ui/EmptyState.svelte";
  import type { IconName } from "$lib/components/ui/icons";
  import type { Section } from "$lib/state/app.svelte";

  let { section }: { section: Section } = $props();

  const copy: Record<
    string,
    { icon: IconName; title: string; body: string; hint: string }
  > = {
    workspaces: {
      icon: "workspaces",
      title: "Workspaces",
      body: "Workspace switching and stale-workspace recovery will live here.",
      hint: "arrives in a later milestone",
    },
  };

  const content = $derived(copy[section] ?? copy.workspaces);
</script>

<EmptyState
  icon={content.icon}
  title={content.title}
  body={content.body}
  hint={content.hint}
/>
