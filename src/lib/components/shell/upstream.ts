// Pure model for the shell's upstream chip — what the TopBar renders for
// the background upstream checks. Kept out of the component so the states
// (idle age, checking, failed, never-checked) are unit-testable.

export interface UpstreamChipState {
  checking: boolean;
  lastChecked: number | null;
  error: string | null;
}

export type UpstreamTone = "quiet" | "busy" | "error";

export interface UpstreamChip {
  label: string;
  tone: UpstreamTone;
  /** Full story for the tooltip; the label stays terse. */
  title: string;
}

/** "just now", "3m ago", "2h ago" — minutes-granular like the chip's
 * heartbeat; clamped so clock skew never reads negative. */
export function checkedAgeLabel(lastChecked: number, now: number): string {
  const minutes = Math.floor(Math.max(0, now - lastChecked) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ago`;
}

/** How the remotes read in the tooltip: the sole remote by name, several
 * by count. */
export function remotesLabel(remoteNames: string[]): string {
  if (remoteNames.length === 1) return remoteNames[0];
  return `${remoteNames.length} remotes`;
}

/** The chip for the current check state; null when the repo has no
 * remotes — no upstream, nothing to check, no dead affordance. */
export function upstreamChip(
  state: UpstreamChipState,
  remoteNames: string[],
  now: number,
): UpstreamChip | null {
  if (remoteNames.length === 0) return null;
  const remotes = remotesLabel(remoteNames);
  if (state.checking) {
    return {
      label: "Checking…",
      tone: "busy",
      title: `Checking ${remotes} for new changes`,
    };
  }
  if (state.error !== null) {
    return {
      label: "Fetch failed",
      tone: "error",
      title: `Could not fetch from ${remotes}: ${state.error} — click to retry`,
    };
  }
  if (state.lastChecked === null) {
    return {
      label: "Fetch",
      tone: "quiet",
      title: `Fetch from ${remotes}`,
    };
  }
  const age = checkedAgeLabel(state.lastChecked, now);
  return {
    label: `Checked ${age}`,
    tone: "quiet",
    title: `Upstream last checked ${age} — click to fetch from ${remotes} now`,
  };
}
