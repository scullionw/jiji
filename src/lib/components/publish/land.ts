// The Land workflow's row models: how a land plan's actions and segment
// statuses read as rows and chips. Pure data — no Svelte. The plan itself
// is Rust-owned (`jiji-forge`'s `plan_land`); this only phrases it.

import type { LandAction } from "$lib/bindings/LandAction";
import type { LandSegmentStatus } from "$lib/bindings/LandSegmentStatus";
import type { MergeMethod } from "$lib/bindings/MergeMethod";

export interface LandActionRow {
  glyph: string;
  tone: "accent" | "ok" | "warn";
  text: string;
}

const MethodLabel: Record<MergeMethod, string> = {
  squash: "Squash-merge",
  rebase: "Rebase-merge",
  merge: "Merge",
};

/** One land action as the plan card renders it. */
export function landActionRow(action: LandAction, remote: string): LandActionRow {
  switch (action.kind) {
    case "mergePr":
      return {
        glyph: "✓",
        tone: "ok",
        text: `${MethodLabel[action.method]} #${action.number} (${action.bookmark})`,
      };
    case "enableAutoMerge":
      return {
        glyph: "◷",
        tone: "accent",
        text: `Enable auto-merge (${action.method}) on #${action.number} — GitHub merges it once its requirements are met`,
      };
    case "enqueuePr":
      return {
        glyph: "⇥",
        tone: "accent",
        text: `Add #${action.number} (${action.bookmark}) to the merge queue`,
      };
    case "fetchRemote":
      return {
        glyph: "↓",
        tone: "accent",
        text: `Fetch from ${action.remote} so the merged trunk arrives locally`,
      };
    case "rebaseOntoTrunk":
      return {
        glyph: "⤴",
        tone: "accent",
        text:
          action.moves === 1
            ? `Rebase ${action.rootChange} onto the new trunk`
            : `Rebase ${action.rootChange} and its ${action.moves - 1} descendant${
                action.moves === 2 ? "" : "s"
              } onto the new trunk`,
      };
    case "pushStack":
      return {
        glyph: "↑",
        tone: "accent",
        text: `Push the rebased ${action.bookmarks.join(", ")} to ${remote}`,
      };
    case "retargetPr":
      return {
        glyph: "⇄",
        tone: "accent",
        text: `Retarget #${action.number} (${action.bookmark}) onto ${action.toBase}`,
      };
    case "cleanupBookmark":
      return {
        glyph: "–",
        tone: "warn",
        text: `Remove the landed bookmark ${action.bookmark} here and on ${remote}`,
      };
    case "abandonLanded":
      return {
        glyph: "⌫",
        tone: "warn",
        text:
          action.changeIds.length === 1
            ? `Sweep ${action.bookmark}'s landed change ${action.changeIds[0]} — its content lives on trunk now`
            : `Sweep ${action.bookmark}'s ${action.changeIds.length} landed changes — their content lives on trunk now`,
      };
  }
}

export interface SegmentChip {
  label: string;
  tone: "ok" | "accent" | "warn" | "muted";
}

/** The status chip a land segment row wears. */
export function segmentChip(status: LandSegmentStatus): SegmentChip {
  switch (status.kind) {
    case "merged":
      return { label: `merged #${status.number}`, tone: "ok" };
    case "landing":
      return { label: "landing", tone: "accent" };
    case "waiting":
      return { label: "waiting", tone: "warn" };
    case "stacked":
      return { label: "next up", tone: "muted" };
  }
}
