// Review-helper presentation: how open PRs read as rows in Publish's
// pull-request list, the fetch-for-review defaults, and the re-run-CI
// outcome line. Pure data — no Svelte.

import type { CiRerunReport } from "$lib/bindings/CiRerunReport";
import type { PrSummary } from "$lib/bindings/PrSummary";
import type { RepoPrState } from "$lib/bindings/RepoPrState";
import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import { prBadge, type PrBadge } from "./pr";

export interface ReviewRow {
  pr: PrSummary;
  badge: PrBadge;
  /** True when the head branch lives on a fork (or the fork is gone) —
   * exactly the PRs only `refs/pull/N/head` can reach. */
  fork: boolean;
  /** `owner:branch` for fork rows, the plain branch name otherwise. */
  headLabel: string;
  /** Failed CI worth re-running: open, and the rollup says failing. */
  canRerun: boolean;
}

/** Failed CI worth re-running: the PR is open and the rollup says
 * failing. Shared by the Publish rows and the workbench header chip so
 * the two surfaces cannot disagree. */
export function canRerun(pr: PrSummary): boolean {
  return pr.state === "open" && pr.checks === "failing";
}

export function reviewRow(pr: PrSummary, repoOwner: string): ReviewRow {
  const fork =
    pr.headOwner == null ||
    pr.headOwner.toLowerCase() !== repoOwner.toLowerCase();
  return {
    pr,
    badge: prBadge(pr),
    fork,
    headLabel:
      fork && pr.headOwner ? `${pr.headOwner}:${pr.headBranch}` : pr.headBranch,
    canRerun: canRerun(pr),
  };
}

/** Every open PR as a review row, in the report's newest-updated order. */
export function reviewRows(
  prs: RepoPrState | null,
  repoOwner: string | null,
): ReviewRow[] {
  if (!prs || !repoOwner) return [];
  return prs.report.prs.map((pr) => reviewRow(pr, repoOwner));
}

/** The default local bookmark a fetched PR reviews under. Deliberately
 * `pr/N` rather than the head branch name: it can never collide with the
 * team's own bookmarks, and it reads as review-owned. */
export function reviewBookmark(number: number | bigint): string {
  return `pr/${number}`;
}

/** Whether a local bookmark already answers to `name` — surfaced in the
 * fetch panel before the backend's own refusal would. */
export function bookmarkTaken(
  name: string,
  snapshot: RepoSnapshot | null,
): boolean {
  const trimmed = name.trim();
  return (
    snapshot?.bookmarks.some((b) => b.isLocal && b.name === trimmed) ?? false
  );
}

/** The re-run answer as one plain line. Empty on both sides is the honest
 * "Actions cannot see that failure" story. */
export function rerunSummary(report: CiRerunReport): string {
  const parts: string[] = [];
  if (report.rerun.length > 0) {
    parts.push(`Re-running failed jobs in ${report.rerun.join(", ")}.`);
  }
  if (report.refused.length > 0) {
    parts.push(`GitHub refused: ${report.refused.join("; ")}.`);
  }
  if (parts.length === 0) {
    return (
      "No failed GitHub Actions runs on this PR's head — the failing " +
      "check may come from another CI system."
    );
  }
  return parts.join(" ");
}
