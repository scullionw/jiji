import { describe, expect, it } from "vitest";
import type { AutoLandState } from "$lib/bindings/AutoLandState";
import type { LandPlan } from "$lib/bindings/LandPlan";
import {
  autolandChip,
  autolandStory,
  canQueueAutoLand,
  isTerminalPhase,
} from "./autoland";

function plan(overrides: Partial<LandPlan> = {}): LandPlan {
  return {
    headBookmark: "profile",
    remote: "origin",
    baseBranch: "main",
    segments: [],
    actions: [],
    blockers: [],
    warnings: [],
    ...overrides,
  };
}

function job(overrides: Partial<AutoLandState> = {}): AutoLandState {
  return {
    headBookmark: "profile",
    phase: { kind: "waiting", attention: false, reasons: ["checks running"] },
    rounds: 0,
    merged: [],
    segments: [],
    lastOutcome: null,
    ...overrides,
  };
}

describe("isTerminalPhase", () => {
  it("marks done, failed, and stopped as over", () => {
    expect(isTerminalPhase({ kind: "done" })).toBe(true);
    expect(isTerminalPhase({ kind: "failed", message: "x" })).toBe(true);
    expect(isTerminalPhase({ kind: "stopped" })).toBe(true);
    expect(
      isTerminalPhase({ kind: "waiting", attention: false, reasons: [] }),
    ).toBe(false);
    expect(isTerminalPhase({ kind: "round" })).toBe(false);
  });
});

describe("canQueueAutoLand", () => {
  it("queues a plan with work to run", () => {
    expect(
      canQueueAutoLand(
        plan({
          actions: [{ kind: "fetchRemote", remote: "origin" }],
        }),
      ),
    ).toBe(true);
  });

  it("queues transient blockers — waiting them out is the point", () => {
    expect(
      canQueueAutoLand(
        plan({
          blockers: [{ message: "#1's checks are still running", wait: true }],
        }),
      ),
    ).toBe(true);
  });

  it("queues a pure supervision plan (no actions, no blockers)", () => {
    expect(canQueueAutoLand(plan())).toBe(true);
  });

  it("refuses needs-user blockers, like Land does", () => {
    expect(
      canQueueAutoLand(
        plan({
          blockers: [
            { message: "#1's checks are still running", wait: true },
            { message: "changes were requested on #1", wait: false },
          ],
        }),
      ),
    ).toBe(false);
  });
});

describe("autolandChip", () => {
  it("watches quietly on remote conditions", () => {
    const chip = autolandChip(job());
    expect(chip.label).toBe("Auto-land profile: watching");
    expect(chip.tone).toBe("accent");
    expect(chip.pulse).toBe(true);
    expect(chip.dismissable).toBe(false);
  });

  it("warns when the job needs the user", () => {
    const chip = autolandChip(
      job({
        phase: { kind: "waiting", attention: true, reasons: ["fix CI"] },
      }),
    );
    expect(chip.label).toBe("Auto-land profile: needs you");
    expect(chip.tone).toBe("warn");
    expect(chip.dismissable).toBe(false);
  });

  it("marks the terminal states dismissable with their tones", () => {
    expect(autolandChip(job({ phase: { kind: "done" } }))).toMatchObject({
      tone: "ok",
      dismissable: true,
    });
    expect(
      autolandChip(job({ phase: { kind: "failed", message: "x" } })),
    ).toMatchObject({ tone: "danger", dismissable: true });
    expect(autolandChip(job({ phase: { kind: "stopped" } }))).toMatchObject({
      tone: "muted",
      dismissable: true,
    });
  });

  it("pulses through a running round", () => {
    const chip = autolandChip(job({ phase: { kind: "round" } }));
    expect(chip.label).toContain("landing…");
    expect(chip.pulse).toBe(true);
  });
});

describe("autolandStory", () => {
  it("joins the waiting reasons and names what landed so far", () => {
    const story = autolandStory(
      job({
        phase: {
          kind: "waiting",
          attention: false,
          reasons: ["checks running", "review pending"],
        },
        merged: [
          { number: 1n, url: "https://x/1", bookmark: "auth" },
        ],
      }),
    );
    expect(story).toContain("checks running; review pending");
    expect(story).toContain("Landed so far: #1");
  });

  it("sums up a finished job", () => {
    const story = autolandStory(
      job({
        phase: { kind: "done" },
        rounds: 2,
        merged: [
          { number: 1n, url: "https://x/1", bookmark: "auth" },
          { number: 7n, url: "https://x/7", bookmark: "profile" },
        ],
      }),
    );
    expect(story).toBe("The whole stack landed: #1, #7 in 2 rounds.");
  });

  it("carries the parked job's message", () => {
    expect(
      autolandStory(
        job({ phase: { kind: "failed", message: "GitHub is unreachable" } }),
      ),
    ).toContain("GitHub is unreachable");
  });
});
