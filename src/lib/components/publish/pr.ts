// PR badge presentation: how an open pull request reads as a compact
// badge on graph rows and in the change header. Pure data — no Svelte.

import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { PrSummary } from "$lib/bindings/PrSummary";
import type { RepoPrState } from "$lib/bindings/RepoPrState";

/** The PR attached to a bookmark, when there is one. Local non-trunk
 * bookmarks only — trunk is what PRs merge into, and the synthetic
 * remote-only trunk entry is not pushable at all — matched by name
 * against the fork-filtered head-branch map (`prs_by_branch`). */
export function prForBookmark(
  bookmark: Pick<BookmarkState, "name" | "isLocal" | "isTrunk">,
  prs: RepoPrState | null,
): PrSummary | null {
  if (!prs || !bookmark.isLocal || bookmark.isTrunk) return null;
  return prs.byBranch[bookmark.name] ?? null;
}

/** Badge tone, which doubles as its plain state word. Draft is a state of
 * its own here: an open-but-draft PR should read muted, not ready. */
export type PrTone = "open" | "draft" | "merged" | "closed";

export interface PrGlyph {
  glyph: string;
  tone: "ok" | "warn" | "danger";
  label: string;
}

export interface PrBadge {
  pr: PrSummary;
  /** "#213" */
  label: string;
  tone: PrTone;
  /** Review state, glyph-sized; null when the repo requires no review or
   * the PR is no longer open (the tone already says everything). */
  review: PrGlyph | null;
  /** Same for the CI rollup; null when no checks are configured. */
  checks: PrGlyph | null;
  /** The full story in plain language, for tooltips. */
  title: string;
}

const REVIEW_GLYPH: Record<string, PrGlyph | null> = {
  approved: { glyph: "✓", tone: "ok", label: "approved" },
  changesRequested: { glyph: "±", tone: "danger", label: "changes requested" },
  reviewRequired: { glyph: "◦", tone: "warn", label: "review required" },
  none: null,
};

const CHECKS_GLYPH: Record<string, PrGlyph | null> = {
  passing: { glyph: "✓", tone: "ok", label: "checks passing" },
  failing: { glyph: "×", tone: "danger", label: "checks failing" },
  pending: { glyph: "…", tone: "warn", label: "checks running" },
  none: null,
};

export function prBadge(pr: PrSummary): PrBadge {
  const tone: PrTone =
    pr.state === "open" ? (pr.isDraft ? "draft" : "open") : pr.state;
  // Review/CI cues matter while the PR can still land; once merged or
  // closed the tone is the whole story.
  const active = pr.state === "open";
  const review = active ? REVIEW_GLYPH[pr.review] : null;
  const checks = active ? CHECKS_GLYPH[pr.checks] : null;
  const facts = [tone as string, review?.label, checks?.label]
    .filter(Boolean)
    .join(" · ");
  return {
    pr,
    label: `#${pr.number}`,
    tone,
    review,
    checks,
    title: `#${pr.number} “${pr.title}” — ${facts} · into ${pr.baseBranch}`,
  };
}
