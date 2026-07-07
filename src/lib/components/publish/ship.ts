// The Ship-to-trunk workflow's stack and row models: which stacks can
// ship directly to trunk and how a ship plan's actions read as rows.
// Pure data — no Svelte. The plan itself is Rust-owned (`jiji-forge`'s
// `plan_ship`); this only shapes what the snapshot already knows so the
// section can offer stacks before a plan is fetched.

import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { ShipAction } from "$lib/bindings/ShipAction";

export interface ShippableStack {
  workstreamId: string;
  title: string;
  /** The change that ships: the stack's top, skipping an undescribed
   * working copy sitting on it — a draft, not shippable work (it would
   * only block on "no description"; describing it is how it opts in).
   * The draft stays behind as a mutable child of the new trunk. */
  headChange: string;
  /** First line of the head's description, for the row. */
  headTitle: string;
  isActive: boolean;
  /** Changes from the head down the stack. */
  changeCount: number;
}

/** Stacks the Ship group offers: every workstream, bookmarked or not —
 * shipping needs no bookmark other than trunk's own. Active stack first,
 * like the publish rows. */
export function shippableStacks(snapshot: RepoSnapshot): ShippableStack[] {
  const nodes = new Map(snapshot.nodes.map((n) => [n.id, n]));
  const stacks: ShippableStack[] = [];
  for (const stream of snapshot.workstreams) {
    // node_ids are top-first.
    let headIndex = 0;
    const top = nodes.get(stream.nodeIds[0]);
    if (top && top.kind === "workingCopy" && !top.description.trim()) {
      headIndex = 1;
    }
    if (headIndex >= stream.nodeIds.length) continue; // only a fresh draft
    const head = nodes.get(stream.nodeIds[headIndex]);
    if (!head) continue;
    stacks.push({
      workstreamId: stream.id,
      title: stream.title,
      headChange: head.id,
      headTitle: head.description.split("\n")[0] || "no description",
      isActive: stream.isActive,
      changeCount: stream.nodeIds.length - headIndex,
    });
  }
  stacks.sort((a, b) => Number(b.isActive) - Number(a.isActive));
  return stacks;
}

export interface ShipActionRow {
  glyph: string;
  tone: "accent" | "ok" | "warn";
  text: string;
}

/** One ship action as the plan card renders it. */
export function shipActionRow(action: ShipAction): ShipActionRow {
  switch (action.kind) {
    case "rebaseOntoTrunk":
      return {
        glyph: "⤴",
        tone: "accent",
        text:
          action.moves === 1
            ? `Rebase ${action.rootChange} onto the fetched trunk first`
            : `Rebase ${action.rootChange} and its ${action.moves - 1} descendant${
                action.moves === 2 ? "" : "s"
              } onto the fetched trunk first`,
      };
    case "moveTrunk":
      return {
        glyph: "⚑",
        tone: "accent",
        text: `Point ${action.bookmark} at ${action.to} — the stack becomes trunk history`,
      };
    case "pushTrunk":
      return {
        glyph: "↑",
        tone: "ok",
        text: `Push ${action.bookmark} to ${action.remote} — this is the shipping step`,
      };
    case "newWorkingCopy":
      return {
        glyph: "@",
        tone: "accent",
        text: `Start a fresh working copy on ${action.on} — the current one ships with the stack`,
      };
  }
}
