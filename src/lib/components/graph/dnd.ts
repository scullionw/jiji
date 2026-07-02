// Drag-and-drop policy for the graph: what dropping one change onto
// another means, decided in one place before anything runs. The rows, the
// floating plan card, the status bar, and the drop handler all render this
// shared plan instead of inventing their own rules (GG's centralized
// drag/drop-mutator lesson). Dropping onto a row makes that row the new
// parent — `jj rebase -s` by default, `jj rebase -r` when the change moves
// alone — and the preview is the plan step: releasing is the confirmation,
// the operation log the undo. The backend re-checks everything; this is
// the affordance rule, not the enforcement. Pure data — no Svelte.

import type { GraphNode } from "$lib/bindings/GraphNode";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import {
  descendantsOf,
  findNode,
} from "$lib/components/inspector/inspect";

/** What releasing the pointer here would do. `op` names the backend call
 * (rebase = with descendants, move = only this change); `forcedAlone`
 * marks gestures that had to degrade to a lone move because the target
 * sits inside the dragged stack — the card says so before the drop. */
export type DropPlan =
  | {
      allowed: true;
      op: "rebase" | "move";
      forcedAlone: boolean;
      summary: string;
      consequences: string[];
    }
  | { allowed: false; reason: string };

/** Immutable rows stay put — same rule as the actions row. Divergent
 * copies drag fine: their node ids are commit-keyed, which is exactly how
 * a mutation picks one side deliberately. */
export function canDrag(node: GraphNode): boolean {
  return node.kind !== "immutable";
}

function shortId(node: GraphNode): string {
  return node.id.slice(0, 4);
}

function plural(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? "" : "s"}`;
}

export function planDrop(
  snapshot: RepoSnapshot,
  sourceId: string,
  targetId: string,
  alone: boolean,
): DropPlan {
  const source = findNode(snapshot, sourceId);
  const target = findNode(snapshot, targetId);
  if (!source || !canDrag(source)) {
    return { allowed: false, reason: "Immutable changes cannot be rebased" };
  }
  if (!target) {
    // A refresh can land mid-drag and take the row with it.
    return { allowed: false, reason: "That change left the snapshot" };
  }
  if (target.id === source.id) {
    return { allowed: false, reason: "A change cannot become its own parent" };
  }

  const descendants = descendantsOf(snapshot, sourceId);
  const targetInStack = descendants.some((d) => d.id === target.id);
  // Dropping below its own descendants has exactly one legal meaning — the
  // lone move that reorders the stack — so the gesture degrades to it and
  // the plan says so instead of refusing.
  const forcedAlone = targetInStack && !alone;
  const op: "rebase" | "move" =
    targetInStack || (alone && descendants.length > 0) ? "move" : "rebase";

  const soleParent =
    source.parents.length === 1 && source.parents[0] === target.id;
  if (soleParent && op === "rebase") {
    return {
      allowed: false,
      reason: `Already the parent of ${shortId(source)} — nothing would move`,
    };
  }

  const dest = shortId(target);
  const parentsLabel = source.parents
    .map((id) => id.slice(0, 4))
    .join(", ");
  const isChildTarget = target.parents.includes(source.id);

  let summary: string;
  if (op === "rebase") {
    summary =
      descendants.length > 0
        ? `Rebase onto ${dest} — with ${plural(descendants.length, "descendant")}`
        : `Rebase onto ${dest}`;
  } else if (soleParent) {
    summary = `Extract ${shortId(source)} — its descendants skip past it`;
  } else if (isChildTarget) {
    summary = `Move onto ${dest} — swapping their order`;
  } else if (targetInStack) {
    summary = `Move onto ${dest} — reordering the stack`;
  } else {
    summary = `Move onto ${dest} — only this change`;
  }

  const consequences: string[] = [];
  if (forcedAlone) {
    consequences.push(
      `${dest} is inside the dragged stack, so this change moves alone.`,
    );
  }
  if (op === "move" && descendants.length > 0) {
    consequences.push(
      soleParent
        ? `It stays on ${parentsLabel}; ${plural(descendants.length, "descendant")} reparent onto ${parentsLabel}.`
        : `${plural(descendants.length, "descendant")} stay${descendants.length === 1 ? "s" : ""} behind, reparented onto ${parentsLabel}.`,
    );
  }
  if (source.parents.length > 1) {
    consequences.push(
      `Its ${source.parents.length} parents are replaced by the destination — the merge is dissolved.`,
    );
  }
  const wc = snapshot.workingCopy;
  if (source.id === wc) {
    consequences.push("The working copy moves with it.");
  } else if (descendants.some((d) => d.id === wc)) {
    consequences.push(
      op === "rebase"
        ? "The working copy follows the rebase."
        : "The working copy stays behind.",
    );
  }

  return { allowed: true, op, forcedAlone, summary, consequences };
}
