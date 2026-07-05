import { describe, expect, it } from "vitest";
import {
  checkedAgeLabel,
  remotesLabel,
  upstreamChip,
  type UpstreamChipState,
} from "./upstream";

const MIN = 60_000;

function state(overrides: Partial<UpstreamChipState> = {}): UpstreamChipState {
  return { checking: false, lastChecked: null, error: null, ...overrides };
}

describe("checkedAgeLabel", () => {
  it("reads just-now under a minute and clamps clock skew", () => {
    expect(checkedAgeLabel(1_000, 30_000)).toBe("just now");
    // A check that completed "after" the heartbeat's clock never reads
    // negative.
    expect(checkedAgeLabel(90_000, 30_000)).toBe("just now");
  });

  it("reads minutes, then hours", () => {
    expect(checkedAgeLabel(0, 3 * MIN)).toBe("3m ago");
    expect(checkedAgeLabel(0, 59 * MIN)).toBe("59m ago");
    expect(checkedAgeLabel(0, 60 * MIN)).toBe("1h ago");
    expect(checkedAgeLabel(0, 26 * 60 * MIN)).toBe("26h ago");
  });
});

describe("upstreamChip", () => {
  it("renders nothing without remotes — no upstream, no dead affordance", () => {
    expect(upstreamChip(state(), [], 0)).toBeNull();
  });

  it("offers a first fetch before any check completed", () => {
    const chip = upstreamChip(state(), ["origin"], 0);
    expect(chip).toMatchObject({ label: "Fetch", tone: "quiet" });
    expect(chip?.title).toContain("origin");
  });

  it("ages the label after a successful check", () => {
    const chip = upstreamChip(
      state({ lastChecked: 0 }),
      ["origin"],
      5 * MIN,
    );
    expect(chip).toMatchObject({ label: "Checked 5m ago", tone: "quiet" });
    expect(chip?.title).toContain("fetch from origin now");
  });

  it("shows the busy state while a check runs, whatever else is set", () => {
    const chip = upstreamChip(
      state({ checking: true, lastChecked: 0, error: "old failure" }),
      ["origin"],
      MIN,
    );
    expect(chip).toMatchObject({ label: "Checking…", tone: "busy" });
  });

  it("surfaces a failed check quietly, with the story in the tooltip", () => {
    const chip = upstreamChip(
      state({ lastChecked: 0, error: "could not reach origin" }),
      ["origin"],
      MIN,
    );
    expect(chip).toMatchObject({ label: "Fetch failed", tone: "error" });
    expect(chip?.title).toContain("could not reach origin");
    expect(chip?.title).toContain("retry");
  });

  it("names the sole remote and counts several", () => {
    expect(remotesLabel(["origin"])).toBe("origin");
    expect(remotesLabel(["origin", "upstream"])).toBe("2 remotes");
    const chip = upstreamChip(state(), ["origin", "upstream"], 0);
    expect(chip?.title).toContain("2 remotes");
  });
});
