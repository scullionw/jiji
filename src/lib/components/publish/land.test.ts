import { describe, expect, it } from "vitest";
import type { LandAction } from "$lib/bindings/LandAction";
import { landActionRow, segmentChip } from "./land";

describe("landActionRow", () => {
  const row = (action: LandAction) => landActionRow(action, "origin");

  it("phrases the merge with its method", () => {
    expect(
      row({
        kind: "mergePr",
        number: 1n,
        bookmark: "auth",
        method: "squash",
        expectedHead: "feedface",
      }).text,
    ).toBe("Squash-merge #1 (auth)");
    expect(
      row({
        kind: "mergePr",
        number: 2n,
        bookmark: "b",
        method: "merge",
        expectedHead: "x",
      }).text,
    ).toBe("Merge #2 (b)");
  });

  it("phrases the automation hand-offs", () => {
    expect(
      row({ kind: "enableAutoMerge", number: 1n, bookmark: "auth", method: "squash" })
        .text,
    ).toContain("Enable auto-merge (squash) on #1");
    expect(
      row({ kind: "enqueuePr", number: 1n, bookmark: "auth" }).text,
    ).toBe("Add #1 (auth) to the merge queue");
  });

  it("phrases the reconcile tail", () => {
    expect(row({ kind: "fetchRemote", remote: "origin" }).text).toContain(
      "Fetch from origin",
    );
    expect(
      row({ kind: "rebaseOntoTrunk", rootChange: "b1", moves: 1 }).text,
    ).toBe("Rebase b1 onto the new trunk");
    expect(
      row({ kind: "rebaseOntoTrunk", rootChange: "b1", moves: 2 }).text,
    ).toBe("Rebase b1 and its 1 descendant onto the new trunk");
    expect(
      row({ kind: "rebaseOntoTrunk", rootChange: "b1", moves: 4 }).text,
    ).toBe("Rebase b1 and its 3 descendants onto the new trunk");
    expect(
      row({ kind: "pushStack", bookmarks: ["profile", "extras"] }).text,
    ).toBe("Push the rebased profile, extras to origin");
    expect(
      row({ kind: "retargetPr", number: 7n, bookmark: "profile", toBase: "main" })
        .text,
    ).toBe("Retarget #7 (profile) onto main");
  });

  it("phrases the cleanup as warn-toned rows", () => {
    const cleanup = row({ kind: "cleanupBookmark", bookmark: "auth" });
    expect(cleanup.tone).toBe("warn");
    expect(cleanup.text).toBe(
      "Remove the landed bookmark auth here and on origin",
    );
    const single = row({
      kind: "abandonLanded",
      bookmark: "auth",
      changeIds: ["a1"],
    });
    expect(single.text).toContain("landed change a1");
    const many = row({
      kind: "abandonLanded",
      bookmark: "auth",
      changeIds: ["a2", "a1"],
    });
    expect(many.tone).toBe("warn");
    expect(many.text).toContain("2 landed changes");
  });
});

describe("segmentChip", () => {
  it("maps every landing status to a chip", () => {
    expect(segmentChip({ kind: "merged", number: 1n, url: "u" })).toEqual({
      label: "merged #1",
      tone: "ok",
    });
    expect(segmentChip({ kind: "landing" })).toEqual({
      label: "landing",
      tone: "accent",
    });
    expect(segmentChip({ kind: "waiting" })).toEqual({
      label: "waiting",
      tone: "warn",
    });
    expect(segmentChip({ kind: "stacked" })).toEqual({
      label: "next up",
      tone: "muted",
    });
  });
});
