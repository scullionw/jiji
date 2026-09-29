import type { PublishStack } from "$lib/bindings/PublishStack";
export type { PublishStack } from "$lib/bindings/PublishStack";
export type { StackSegment } from "$lib/bindings/StackSegment";

// Discovery and status classification live in jiji-forge for both hosts.
export function stackSummary(stack: PublishStack): {
  label: string;
  tone: string;
} {
  return {
    local: { label: "Local", tone: "muted" },
    conflicts: { label: "Conflicts", tone: "danger" },
    needsAttention: { label: "Needs attention", tone: "danger" },
    draft: { label: "Draft", tone: "muted" },
    checksRunning: { label: "Checks running", tone: "warn" },
    inReview: { label: "In review", tone: "warn" },
    published: { label: "Published", tone: "ok" },
  }[stack.status];
}
