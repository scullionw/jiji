import { describe, expect, it } from "vitest";
import type { ChecksRollup } from "$lib/bindings/ChecksRollup";
import type { PrState } from "$lib/bindings/PrState";
import type { PrSummary } from "$lib/bindings/PrSummary";
import type { RepoPrState } from "$lib/bindings/RepoPrState";
import type { ReviewDecision } from "$lib/bindings/ReviewDecision";
import { prBadge, prForBookmark } from "./pr";

function pr(overrides: Partial<PrSummary> = {}): PrSummary {
  return {
    number: 213n,
    title: "Add the watcher",
    url: "https://github.com/o/r/pull/213",
    state: "open" as PrState,
    isDraft: false,
    headBranch: "watcher-fix",
    headCommit: "a".repeat(40),
    headOwner: "o",
    baseBranch: "main",
    body: null,
    review: "none" as ReviewDecision,
    checks: "none" as ChecksRollup,
    ...overrides,
  };
}

function prState(...prs: PrSummary[]): RepoPrState {
  return {
    report: { prs, truncated: false },
    byBranch: Object.fromEntries(prs.map((p) => [p.headBranch, p])),
  };
}

const localBookmark = { name: "watcher-fix", isLocal: true, isTrunk: false };

describe("prForBookmark", () => {
  const prs = prState(pr());

  it("attaches by bookmark name", () => {
    expect(prForBookmark(localBookmark, prs)?.number).toBe(213n);
  });

  it("ignores trunk, non-local entries, and unmatched names", () => {
    expect(
      prForBookmark({ name: "watcher-fix", isLocal: true, isTrunk: true }, prs),
    ).toBeNull();
    expect(
      prForBookmark(
        { name: "watcher-fix", isLocal: false, isTrunk: false },
        prs,
      ),
    ).toBeNull();
    expect(
      prForBookmark({ name: "other", isLocal: true, isTrunk: false }, prs),
    ).toBeNull();
  });

  it("answers null before any PR state arrives", () => {
    expect(prForBookmark(localBookmark, null)).toBeNull();
  });
});

describe("prBadge", () => {
  it("reads a plain open PR with no review or checks configured", () => {
    const badge = prBadge(pr());
    expect(badge.label).toBe("#213");
    expect(badge.tone).toBe("open");
    expect(badge.review).toBeNull();
    expect(badge.checks).toBeNull();
    expect(badge.title).toBe(
      "#213 “Add the watcher” — open · into main",
    );
  });

  it("carries review and checks glyphs while open", () => {
    const badge = prBadge(
      pr({ review: "approved", checks: "passing" }),
    );
    expect(badge.review).toMatchObject({ glyph: "✓", tone: "ok" });
    expect(badge.checks).toMatchObject({ glyph: "✓", tone: "ok" });
    expect(badge.title).toContain("approved · checks passing");

    const blocked = prBadge(
      pr({ review: "changesRequested", checks: "failing" }),
    );
    expect(blocked.review).toMatchObject({ glyph: "±", tone: "danger" });
    expect(blocked.checks).toMatchObject({ glyph: "×", tone: "danger" });

    const waiting = prBadge(
      pr({ review: "reviewRequired", checks: "pending" }),
    );
    expect(waiting.review).toMatchObject({ tone: "warn" });
    expect(waiting.checks).toMatchObject({ tone: "warn" });
  });

  it("draft is its own tone, still showing the live cues", () => {
    const badge = prBadge(pr({ isDraft: true, checks: "pending" }));
    expect(badge.tone).toBe("draft");
    expect(badge.checks).not.toBeNull();
    expect(badge.title).toContain("draft · checks running");
  });

  it("merged and closed drop the cues — the tone is the story", () => {
    const merged = prBadge(
      pr({ state: "merged", review: "approved", checks: "passing" }),
    );
    expect(merged.tone).toBe("merged");
    expect(merged.review).toBeNull();
    expect(merged.checks).toBeNull();

    // A closed draft reads closed, not draft.
    const closed = prBadge(pr({ state: "closed", isDraft: true }));
    expect(closed.tone).toBe("closed");
  });
});
