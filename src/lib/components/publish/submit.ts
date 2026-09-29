// The Publish section's stack model: which stacks are publishable and how
// a submit plan reads as rows. Pure data — no Svelte. The plan itself is
// Rust-owned (`jiji-forge`'s `plan_submit`); this only shapes what the
// snapshot already knows so the section can offer stacks before a plan is
// fetched.

import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SubmitAction } from "$lib/bindings/SubmitAction";
import type { SubmitHead } from "$lib/bindings/SubmitHead";

export type PublishableStack = {
  workstreamId: string;
  title: string;
  isActive: boolean;
  changeCount: number;
} & (
  | {
      /** The bookmark a submit plan publishes up to: the top-most
       * bookmarked change of the stack (work above it stays local — the
       * bookmark is the opt-in, jjpr's model). */
      headBookmark: string;
      headChange?: undefined;
    }
  | {
      /** A stack with no bookmark anywhere: the head change to publish,
       * with the branch name derived by the plan and the bookmark created
       * on confirm. Ship's head posture — an undescribed working-copy
       * draft on top is skipped. */
      headChange: string;
      /** First line of the head's description, for the row. */
      headTitle: string;
      headBookmark?: undefined;
    }
);

/** The submit head a stack row plans with, and the string key that names
 * the row (`data-submit-stack`, picked state). */
export function stackHead(stack: PublishableStack): SubmitHead {
  return stack.headBookmark != null
    ? { kind: "bookmark", name: stack.headBookmark }
    : { kind: "change", change: stack.headChange };
}

export function stackKey(stack: PublishableStack): string {
  return stack.headBookmark ?? stack.headChange;
}

/** Stacks the Publish section offers, active stack first: workstreams
 * whose chain carries a local non-trunk bookmark publish up to the
 * top-most one (when several bookmarks share the head change, the one
 * GitHub already knows — an open PR's head branch — wins, then the first
 * name, mirroring the backend's segment naming so the offered name
 * matches the plan); fully unbookmarked workstreams publish their head
 * change through a plan-derived name. */
export function publishableStacks(
  snapshot: RepoSnapshot,
  prBranches: ReadonlySet<string>,
): PublishableStack[] {
  const local = new Set(
    snapshot.bookmarks
      .filter((b) => b.isLocal && !b.isTrunk)
      .map((b) => b.name),
  );
  const nodes = new Map(snapshot.nodes.map((n) => [n.id, n]));
  const stacks: PublishableStack[] = [];
  for (const stream of snapshot.workstreams) {
    // node_ids are top-first; the first bookmarked one is the publish head.
    const headIndex = stream.nodeIds.findIndex((id) =>
      nodes.get(id)?.bookmarks.some((name) => local.has(name)),
    );
    if (headIndex === -1) {
      // No bookmark anywhere: the derived-name flow, with ship's head
      // posture — skip an undescribed working-copy draft sitting on top.
      let start = 0;
      const top = nodes.get(stream.nodeIds[0]);
      if (top && top.kind === "workingCopy" && !top.description.trim()) {
        start = 1;
      }
      if (start >= stream.nodeIds.length) continue; // only a fresh draft
      const head = nodes.get(stream.nodeIds[start]);
      if (!head) continue;
      stacks.push({
        workstreamId: stream.id,
        title: stream.title,
        headChange: head.id,
        headTitle: head.description.split("\n")[0] || "no description",
        isActive: stream.isActive,
        changeCount: stream.nodeIds.length - start,
      });
      continue;
    }
    const names = nodes
      .get(stream.nodeIds[headIndex])!
      .bookmarks.filter((name) => local.has(name))
      .sort((a, b) => {
        const aKnown = prBranches.has(a) ? 0 : 1;
        const bKnown = prBranches.has(b) ? 0 : 1;
        return aKnown - bKnown || a.localeCompare(b);
      });
    stacks.push({
      workstreamId: stream.id,
      title: stream.title,
      headBookmark: names[0],
      isActive: stream.isActive,
      changeCount: stream.nodeIds.length - headIndex,
    });
  }
  stacks.sort((a, b) => Number(b.isActive) - Number(a.isActive));
  return stacks;
}

/** One plan action as the panel renders it. */
export interface ActionRow {
  glyph: string;
  tone: "accent" | "ok";
  text: string;
}

export function actionRow(action: SubmitAction, remote: string): ActionRow {
  switch (action.kind) {
    case "createBookmark":
      return {
        glyph: "⚑",
        tone: "accent",
        text: `Create bookmark ${action.bookmark} at ${action.change} — named from its description`,
      };
    case "push":
      return action.create
        ? {
            glyph: "↑",
            tone: "accent",
            text: `Publish ${action.bookmark} as a new branch on ${remote}`,
          }
        : {
            glyph: "↑",
            tone: "accent",
            text: `Push ${action.bookmark} to ${remote}`,
          };
    case "createPr":
      return {
        glyph: "+",
        tone: "ok",
        text: `Open a ${action.draft ? "draft " : ""}pull request: ${action.bookmark} → ${action.base} — “${action.title}”`,
      };
    case "retargetPr":
      return {
        glyph: "⇄",
        tone: "accent",
        text: `Retarget #${action.number}: base ${action.fromBase} → ${action.toBase}`,
      };
    case "updatePrText":
      if (action.seed) {
        return {
          glyph: "✎",
          tone: "accent",
          text: `Adopt #${action.number}’s description — it matches ${action.bookmark}’s commit, so Jiji records its fingerprints and keeps it updated from now on`,
        };
      }
      return {
        glyph: "✎",
        tone: "accent",
        text: `Update #${action.number}’s ${
          action.title != null ? "title and description" : "description"
        } from ${action.bookmark}’s commit text`,
      };
    case "syncStackComment":
      return {
        glyph: "☰",
        tone: "ok",
        text: action.create
          ? `Post the stack comment on ${
              action.number != null
                ? `#${action.number}`
                : `${action.bookmark}’s new pull request`
            }`
          : `Update the stack comment on #${action.number}`,
      };
  }
}
