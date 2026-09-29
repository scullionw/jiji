import { describe, expect, it } from "vitest";
import type { ChecksRollup } from "$lib/bindings/ChecksRollup";
import type { PrState } from "$lib/bindings/PrState";
import type { PrSummary } from "$lib/bindings/PrSummary";
import type { RepoPrState } from "$lib/bindings/RepoPrState";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { ReviewDecision } from "$lib/bindings/ReviewDecision";
import {
  bookmarkTaken,
  rerunSummary,
  reviewBookmark,
  reviewRow,
  reviewRows,
} from "./review";

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
    byBranch: {},
  };
}

describe("reviewRow", () => {
  it("same-repo PRs read as their branch, case-insensitively", () => {
    const row = reviewRow(pr(), "o");
    expect(row.fork).toBe(false);
    expect(row.headLabel).toBe("watcher-fix");

    const cased = reviewRow(pr({ headOwner: "O" }), "o");
    expect(cased.fork).toBe(false);
  });

  it("fork PRs are flagged and labeled owner:branch", () => {
    const row = reviewRow(pr({ headOwner: "contributor" }), "o");
    expect(row.fork).toBe(true);
    expect(row.headLabel).toBe("contributor:watcher-fix");

    // A deleted fork keeps the plain branch label but still reads fork.
    const ghost = reviewRow(pr({ headOwner: null }), "o");
    expect(ghost.fork).toBe(true);
    expect(ghost.headLabel).toBe("watcher-fix");
  });

  it("re-run offers only on open PRs with failing checks", () => {
    expect(reviewRow(pr({ checks: "failing" }), "o").canRerun).toBe(true);
    expect(reviewRow(pr({ checks: "passing" }), "o").canRerun).toBe(false);
    expect(reviewRow(pr({ checks: "pending" }), "o").canRerun).toBe(false);
    expect(
      reviewRow(pr({ state: "merged", checks: "failing" }), "o").canRerun,
    ).toBe(false);
  });
});

describe("reviewRows", () => {
  it("maps every reported PR and answers empty before state arrives", () => {
    const rows = reviewRows(
      prState(pr(), pr({ number: 7n, headOwner: "fork-owner" })),
      "o",
    );
    expect(rows).toHaveLength(2);
    expect(rows[1].fork).toBe(true);
    expect(reviewRows(null, "o")).toEqual([]);
    expect(reviewRows(prState(pr()), null)).toEqual([]);
  });
});

describe("fetch-for-review defaults", () => {
  it("suggests pr/N and spots taken names", () => {
    expect(reviewBookmark(213n)).toBe("pr/213");
    const snapshot = {
      bookmarks: [
        { name: "pr/213", isLocal: true },
        { name: "remote-only", isLocal: false },
      ],
    } as unknown as RepoSnapshot;
    expect(bookmarkTaken("pr/213", snapshot)).toBe(true);
    expect(bookmarkTaken(" pr/213 ", snapshot)).toBe(true);
    // Remote-only entries do not block a local name.
    expect(bookmarkTaken("remote-only", snapshot)).toBe(false);
    expect(bookmarkTaken("pr/7", snapshot)).toBe(false);
    expect(bookmarkTaken("pr/7", null)).toBe(false);
  });
});

describe("rerunSummary", () => {
  it("tells what re-ran, what was refused, and the honest empty story", () => {
    expect(rerunSummary({ rerun: ["ci", "lint"], refused: [] })).toBe(
      "Re-running failed jobs in ci, lint.",
    );
    expect(
      rerunSummary({ rerun: ["ci"], refused: ["docs: already re-running"] }),
    ).toBe("Re-running failed jobs in ci. GitHub refused: docs: already re-running.");
    expect(rerunSummary({ rerun: [], refused: [] })).toContain(
      "another CI system",
    );
  });
});
