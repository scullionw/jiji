import { describe, expect, it } from "vitest";
import { rowOffsets, visibleRange } from "./viewport";

describe("viewport windowing", () => {
  it("includes only intersecting rows at exact boundaries", () => {
    const offsets = rowOffsets([30, 18, 52, 30], (height) => height);
    expect(offsets).toEqual([0, 30, 48, 100, 130]);
    expect(visibleRange(offsets, 30, 18)).toEqual({ start: 1, end: 2 });
    expect(visibleRange(offsets, 31, 18)).toEqual({ start: 1, end: 3 });
    expect(visibleRange(offsets, 30, 18, 1)).toEqual({ start: 0, end: 3 });
  });

  it("handles empty content and viewports entirely outside a file", () => {
    expect(visibleRange([0], 0, 800)).toEqual({ start: 0, end: 0 });
    expect(visibleRange([0, 30, 60], -800, 600)).toEqual({ start: 0, end: 0 });
    expect(visibleRange([0, 30, 60], 60, 600)).toEqual({ start: 2, end: 2 });
  });

  it("agrees with intersection for mixed graph and diff block heights", () => {
    const heights = Array.from({ length: 500 }, (_, i) => i % 7 === 0 ? 18 : 64 * 20.8 + 10.4);
    const offsets = rowOffsets(heights, (height) => height);
    for (let top = -1200; top < offsets[500] + 1200; top += 1379) {
      const { start, end } = visibleRange(offsets, top, 820, 1200);
      const actual = heights.flatMap((_, i) =>
        offsets[i + 1] > top - 1200 && offsets[i] < top + 820 + 1200 ? [i] : [],
      );
      expect(Array.from({ length: end - start }, (_, i) => start + i)).toEqual(actual);
    }
  });

  it("keeps a thousand-change graph to fewer than 50 mounted rows", () => {
    const offsets = rowOffsets(Array.from({ length: 1000 }), () => 34);
    for (const top of [0, 8500, 17000, 33100]) {
      const range = visibleRange(offsets, top, 820, 300);
      expect(range.end - range.start).toBeLessThan(50);
      expect(range.end).toBeGreaterThan(range.start);
    }
  });
});
