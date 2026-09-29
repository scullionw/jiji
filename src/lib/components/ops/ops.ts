// Pure shaping for the operation timeline: day grouping and collapsing
// runs of working-copy snapshots so the log reads like a journal of real
// work instead of an undifferentiated stream.

import dayjs from "dayjs";
import type { OperationItem } from "$lib/bindings/OperationItem";

export type TimelineRow =
  | { kind: "op"; op: OperationItem }
  | { kind: "snapshots"; key: string; ops: OperationItem[] };

export interface OpDayGroup {
  label: string;
  rows: TimelineRow[];
}

/** Runs of at least this many consecutive snapshot ops collapse into one row. */
export const SNAPSHOT_RUN_MIN = 3;

export function dayLabel(
  timestamp: string,
  now: string | number | Date = new Date(),
): string {
  const day = dayjs(timestamp);
  const today = dayjs(now);
  if (day.isSame(today, "day")) return "Today";
  if (day.isSame(today.subtract(1, "day"), "day")) return "Yesterday";
  return day.isSame(today, "year")
    ? day.format("MMMM D")
    : day.format("MMMM D, YYYY");
}

/** Time of day for rows already grouped under a day header. */
export function clockTime(timestamp: string): string {
  return dayjs(timestamp).format("HH:mm");
}

/** The operations a restore to `opId` would unwind: everything newer than
 *  it in the newest-first list. Empty for the current head (a no-op
 *  restore); `null` when the op is not in the list at all. */
export function opsSince(
  ops: OperationItem[],
  opId: string,
): OperationItem[] | null {
  const index = ops.findIndex((op) => op.id === opId);
  return index === -1 ? null : ops.slice(0, index);
}

/** jj's root operation (id is all zeros) is the state before the repo
 *  existed: nothing to revert, and restoring to it would empty the repo —
 *  it gets no time-travel affordances. */
export function isRootOp(op: OperationItem): boolean {
  return /^0+$/.test(op.id);
}

/** Group newest-first operations by calendar day, collapsing snapshot runs
 *  within each day. The current operation never collapses. */
export function groupOperations(
  ops: OperationItem[],
  now: string | number | Date = new Date(),
): OpDayGroup[] {
  const groups: OpDayGroup[] = [];
  let label: string | null = null;
  let bucket: OperationItem[] = [];
  const flush = () => {
    if (bucket.length > 0 && label !== null) {
      groups.push({ label, rows: collapseSnapshotRuns(bucket) });
    }
    bucket = [];
  };
  for (const op of ops) {
    const opLabel = dayLabel(op.timestamp, now);
    if (opLabel !== label) {
      flush();
      label = opLabel;
    }
    bucket.push(op);
  }
  flush();
  return groups;
}

function collapseSnapshotRuns(ops: OperationItem[]): TimelineRow[] {
  const rows: TimelineRow[] = [];
  let i = 0;
  while (i < ops.length) {
    let j = i;
    while (j < ops.length && ops[j].isSnapshot && !ops[j].isCurrent) j++;
    const run = ops.slice(i, j);
    if (run.length >= SNAPSHOT_RUN_MIN) {
      rows.push({ kind: "snapshots", key: run[0].id, ops: run });
    } else {
      for (const op of run) rows.push({ kind: "op", op });
    }
    if (j === i) {
      rows.push({ kind: "op", op: ops[i] });
      j = i + 1;
    }
    i = j;
  }
  return rows;
}
