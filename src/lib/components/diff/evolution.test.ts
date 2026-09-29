import { describe, expect, it } from "vitest";
import type { ChangeEvolog } from "$lib/bindings/ChangeEvolog";
import {
  defaultSpan,
  extendTo,
  grow,
  scrub,
  stepTo,
  timelineVersions,
} from "./evolution";

function evolog(ids: string[]): ChangeEvolog {
  return {
    id: "zzzzzzzz",
    entries: ids.map((commitId) => ({
      commitId,
      description: "",
      author: "a",
      timestamp: "2026-01-01T00:00:00Z",
      opId: null,
      opDescription: null,
      opTimestamp: null,
    })),
    truncated: false,
  };
}

describe("timelineVersions", () => {
  it("reverses the backend's newest-first order into reading order", () => {
    const versions = timelineVersions(evolog(["c3", "c2", "c1"]));
    expect(versions.map((v) => v.commitId)).toEqual(["c1", "c2", "c3"]);
  });

  it("leaves the evolog's own entries untouched", () => {
    const log = evolog(["c2", "c1"]);
    timelineVersions(log);
    expect(log.entries[0].commitId).toBe("c2");
  });
});

describe("defaultSpan", () => {
  it("opens on the newest step", () => {
    expect(defaultSpan(5)).toEqual({ from: 3, to: 4 });
  });

  it("has nothing to show for a never-rewritten change", () => {
    expect(defaultSpan(1)).toBeNull();
    expect(defaultSpan(0)).toBeNull();
  });
});

describe("stepTo", () => {
  it("selects the step that produced the clicked version", () => {
    expect(stepTo(2, 5)).toEqual({ from: 1, to: 2 });
  });

  it("lands the oldest version on the first step", () => {
    expect(stepTo(0, 5)).toEqual({ from: 0, to: 1 });
  });

  it("clamps past the newest version", () => {
    expect(stepTo(9, 5)).toEqual({ from: 3, to: 4 });
  });

  it("refuses on a single-version chain", () => {
    expect(stepTo(0, 1)).toBeNull();
  });
});

describe("extendTo", () => {
  it("widens toward an older version", () => {
    expect(extendTo({ from: 3, to: 4 }, 1, 5)).toEqual({ from: 1, to: 4 });
  });

  it("widens toward a newer version", () => {
    expect(extendTo({ from: 1, to: 2 }, 4, 5)).toEqual({ from: 1, to: 4 });
  });

  it("narrows from the far end when the click lands inside", () => {
    expect(extendTo({ from: 0, to: 4 }, 3, 5)).toEqual({ from: 0, to: 3 });
    expect(extendTo({ from: 0, to: 4 }, 1, 5)).toEqual({ from: 1, to: 4 });
  });

  it("never collapses to a zero-width span", () => {
    expect(extendTo({ from: 2, to: 3 }, 2, 5)).toEqual({ from: 2, to: 3 });
    expect(extendTo({ from: 2, to: 3 }, 3, 5)).toEqual({ from: 2, to: 3 });
  });
});

describe("scrub", () => {
  it("slides a single step along the chain", () => {
    expect(scrub({ from: 2, to: 3 }, -1, 5)).toEqual({ from: 1, to: 2 });
    expect(scrub({ from: 1, to: 2 }, 1, 5)).toEqual({ from: 2, to: 3 });
  });

  it("stops at both ends", () => {
    expect(scrub({ from: 0, to: 1 }, -1, 5)).toEqual({ from: 0, to: 1 });
    expect(scrub({ from: 3, to: 4 }, 1, 5)).toEqual({ from: 3, to: 4 });
  });

  it("collapses a wide span to its newest step before sliding", () => {
    expect(scrub({ from: 0, to: 3 }, -1, 5)).toEqual({ from: 2, to: 3 });
  });
});

describe("grow", () => {
  it("stretches one edge a step at a time", () => {
    expect(grow({ from: 2, to: 3 }, "older", 5)).toEqual({ from: 1, to: 3 });
    expect(grow({ from: 2, to: 3 }, "newer", 5)).toEqual({ from: 2, to: 4 });
  });

  it("holds at the chain's ends", () => {
    expect(grow({ from: 0, to: 1 }, "older", 5)).toEqual({ from: 0, to: 1 });
    expect(grow({ from: 3, to: 4 }, "newer", 5)).toEqual({ from: 3, to: 4 });
  });
});
