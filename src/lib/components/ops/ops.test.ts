import { describe, expect, it } from "vitest";
import type { OperationItem } from "$lib/bindings/OperationItem";
import {
  dayLabel,
  groupOperations,
  isRootOp,
  opsSince,
  type TimelineRow,
} from "./ops";

const NOW = "2026-06-10T13:00:00";

let nextId = 0;
function op(overrides: Partial<OperationItem> = {}): OperationItem {
  nextId += 1;
  return {
    id: `op${String(nextId).padStart(10, "0")}`,
    description: "describe commit abc",
    timestamp: "2026-06-10T12:00:00",
    isCurrent: false,
    user: "lauf@mbp",
    isSnapshot: false,
    effects: [],
    moreEffects: 0,
    ...overrides,
  };
}

function snapshot(overrides: Partial<OperationItem> = {}): OperationItem {
  return op({ description: "snapshot working copy", isSnapshot: true, ...overrides });
}

function kinds(rows: TimelineRow[]): string[] {
  return rows.map((row) => row.kind);
}

describe("dayLabel", () => {
  it("labels today and yesterday in words", () => {
    expect(dayLabel("2026-06-10T01:00:00", NOW)).toBe("Today");
    expect(dayLabel("2026-06-09T23:59:00", NOW)).toBe("Yesterday");
  });

  it("labels same-year days without the year", () => {
    expect(dayLabel("2026-06-01T12:00:00", NOW)).toBe("June 1");
  });

  it("labels other years fully", () => {
    expect(dayLabel("2025-12-31T12:00:00", NOW)).toBe("December 31, 2025");
  });
});

describe("groupOperations", () => {
  it("groups newest-first operations by calendar day", () => {
    const groups = groupOperations(
      [
        op({ timestamp: "2026-06-10T12:00:00" }),
        op({ timestamp: "2026-06-10T09:00:00" }),
        op({ timestamp: "2026-06-09T18:00:00" }),
        op({ timestamp: "2026-06-01T18:00:00" }),
      ],
      NOW,
    );
    expect(groups.map((g) => g.label)).toEqual(["Today", "Yesterday", "June 1"]);
    expect(groups[0].rows).toHaveLength(2);
  });

  it("collapses runs of three or more snapshots", () => {
    const groups = groupOperations(
      [op(), snapshot(), snapshot(), snapshot(), op()],
      NOW,
    );
    const rows = groups[0].rows;
    expect(kinds(rows)).toEqual(["op", "snapshots", "op"]);
    const run = rows[1];
    if (run.kind !== "snapshots") throw new Error("expected snapshots row");
    expect(run.ops).toHaveLength(3);
    expect(run.key).toBe(run.ops[0].id);
  });

  it("keeps short snapshot runs as individual rows", () => {
    const groups = groupOperations([snapshot(), snapshot(), op()], NOW);
    expect(kinds(groups[0].rows)).toEqual(["op", "op", "op"]);
  });

  it("never collapses the current operation", () => {
    const groups = groupOperations(
      [snapshot(), snapshot(), snapshot({ isCurrent: true }), snapshot(), snapshot(), snapshot()],
      NOW,
    );
    // The current snapshot splits the run; only the long tail collapses.
    expect(kinds(groups[0].rows)).toEqual(["op", "op", "op", "snapshots"]);
  });

  it("does not collapse runs across day boundaries", () => {
    const groups = groupOperations(
      [
        snapshot({ timestamp: "2026-06-10T01:00:00" }),
        snapshot({ timestamp: "2026-06-10T00:30:00" }),
        snapshot({ timestamp: "2026-06-09T23:59:00" }),
        snapshot({ timestamp: "2026-06-09T23:00:00" }),
      ],
      NOW,
    );
    expect(groups.map((g) => g.label)).toEqual(["Today", "Yesterday"]);
    expect(kinds(groups[0].rows)).toEqual(["op", "op"]);
    expect(kinds(groups[1].rows)).toEqual(["op", "op"]);
  });
});

describe("opsSince", () => {
  it("returns the operations a restore would unwind, newest first", () => {
    const ops = [op(), op(), op()];
    expect(opsSince(ops, ops[2].id)).toEqual([ops[0], ops[1]]);
    expect(opsSince(ops, ops[1].id)).toEqual([ops[0]]);
  });

  it("is empty for the current head and null for unknown ids", () => {
    const ops = [op({ isCurrent: true }), op()];
    expect(opsSince(ops, ops[0].id)).toEqual([]);
    expect(opsSince(ops, "feedfacefeed")).toBeNull();
  });
});

describe("isRootOp", () => {
  it("recognizes the all-zero root operation id only", () => {
    expect(isRootOp(op({ id: "000000000000" }))).toBe(true);
    expect(isRootOp(op({ id: "0a1f6e83d527" }))).toBe(false);
    expect(isRootOp(op())).toBe(false);
  });
});
