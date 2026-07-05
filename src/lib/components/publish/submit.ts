// The Publish section's stack model: which stacks are publishable and how
// a submit plan reads as rows. Pure data — no Svelte. The plan itself is
// Rust-owned (`jiji-forge`'s `plan_submit`); this only shapes what the
// snapshot already knows so the section can offer stacks before a plan is
// fetched.

import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { SubmitAction } from "$lib/bindings/SubmitAction";

export interface PublishableStack {
  workstreamId: string;
  title: string;
  /** The bookmark a submit plan publishes up to: the top-most bookmarked
   * change of the stack. */
  headBookmark: string;
  isActive: boolean;
  /** Changes from the bookmarked head down the stack — what publishing
   * covers (work above the bookmark stays local). */
  changeCount: number;
}

/** Stacks the Publish section offers: workstreams whose chain carries at
 * least one local non-trunk bookmark, active stack first. When several
 * bookmarks share the head change, the one GitHub already knows (an open
 * PR's head branch) wins, then the first name — mirroring the backend's
 * segment naming so the offered name matches the plan. */
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
    if (headIndex === -1) continue;
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
        text: `Open a pull request: ${action.bookmark} → ${action.base} — “${action.title}”`,
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
