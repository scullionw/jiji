import { describe, expect, it } from "vitest";
import { stackSummary } from "./stacks";
import { publishFixture } from "./publish.fixtures";

describe("shared stack presentation", () => {
  it("renders Rust-owned statuses without claiming CI alone makes a stack mergeable", () => {
    const { stacks } = publishFixture();
    expect(stackSummary(stacks[0])).toEqual({ label: "Draft", tone: "muted" });
    expect(stackSummary({ ...stacks[0], status: "published" }).label).toBe(
      "Published",
    );
    expect(stackSummary(stacks[1]).label).toBe("Local");
  });
});
