import { describe, expect, it } from "vitest";
import type { AutoLandState } from "$lib/bindings/AutoLandState";
import type { AutoLandStatus } from "$lib/bindings/AutoLandStatus";
import type { BookmarkState } from "$lib/bindings/BookmarkState";
import type { LandPlan } from "$lib/bindings/LandPlan";
import {
  autolandChip,
  autolandStory,
  autolandVisible,
  canQueueAutoLand,
  isInterrupted,
  isTerminalPhase,
  resumeBlocker,
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

function status(
  state: AutoLandState,
  overrides: Partial<Omit<AutoLandStatus, "record">> & {
    repoPath?: string;
  } = {},
): AutoLandStatus {
  return {
    record: {
      version: 1,
      repoPath: overrides.repoPath ?? "/tmp/repo",
      state,
      savedAtMs: 1751500000000n,
    },
    live: overrides.live ?? true,
  };
}

function bookmark(name: string, isLocal = true): BookmarkState {
  return {
    name,
    target: "abc",
    remote: isLocal ? null : "origin",
    sync: "synced",
    isTrunk: false,
    isLocal,
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

describe("isInterrupted", () => {
  it("reads a non-live, non-terminal record as interrupted", () => {
    expect(isInterrupted(status(job(), { live: false }))).toBe(true);
    expect(
      isInterrupted(status(job({ phase: { kind: "round" } }), { live: false })),
    ).toBe(true);
  });

  it("never marks live jobs or terminal records", () => {
    expect(isInterrupted(status(job()))).toBe(false);
    expect(
      isInterrupted(status(job({ phase: { kind: "done" } }), { live: false })),
    ).toBe(false);
    expect(
      isInterrupted(
        status(job({ phase: { kind: "failed", message: "x" } }), {
          live: false,
        }),
      ),
    ).toBe(false);
  });
});

describe("autolandVisible", () => {
  it("renders a record only in the repo it belongs to", () => {
    const s = status(job(), { repoPath: "/tmp/repo" });
    expect(autolandVisible(s, "/tmp/repo")).toBe(true);
    expect(autolandVisible(s, "/somewhere/else")).toBe(false);
    expect(autolandVisible(s, null)).toBe(false);
    expect(autolandVisible(s, undefined)).toBe(false);
  });
});

describe("resumeBlocker", () => {
  it("allows resume while the head bookmark is still local", () => {
    const s = status(job(), { live: false });
    expect(resumeBlocker(s, [bookmark("profile"), bookmark("main")])).toBe(
      null,
    );
  });

  it("names a vanished or remote-only bookmark instead of offering a doomed resume", () => {
    const s = status(job(), { live: false });
    expect(resumeBlocker(s, [bookmark("main")])).toContain(
      "no longer a bookmark here",
    );
    expect(resumeBlocker(s, [bookmark("profile", false)])).toContain(
      "no longer a bookmark here",
    );
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
    const chip = autolandChip(status(job()));
    expect(chip.label).toBe("Auto-land profile: watching");
    expect(chip.tone).toBe("accent");
    expect(chip.pulse).toBe(true);
    expect(chip.dismissable).toBe(false);
  });

  it("warns when the job needs the user", () => {
    const chip = autolandChip(
      status(
        job({
          phase: { kind: "waiting", attention: true, reasons: ["fix CI"] },
        }),
      ),
    );
    expect(chip.label).toBe("Auto-land profile: needs you");
    expect(chip.tone).toBe("warn");
    expect(chip.dismissable).toBe(false);
  });

  it("marks the terminal states dismissable with their tones", () => {
    expect(
      autolandChip(status(job({ phase: { kind: "done" } }))),
    ).toMatchObject({
      tone: "ok",
      dismissable: true,
    });
    expect(
      autolandChip(status(job({ phase: { kind: "failed", message: "x" } }))),
    ).toMatchObject({ tone: "danger", dismissable: true });
    expect(
      autolandChip(status(job({ phase: { kind: "stopped" } }))),
    ).toMatchObject({
      tone: "muted",
      dismissable: true,
    });
  });

  it("pulses through a running round", () => {
    const chip = autolandChip(status(job({ phase: { kind: "round" } })));
    expect(chip.label).toContain("landing…");
    expect(chip.pulse).toBe(true);
  });

  it("reads an interrupted record as warn-toned and dismissable, no pulse", () => {
    const chip = autolandChip(status(job(), { live: false }));
    expect(chip.label).toBe("Auto-land profile: interrupted");
    expect(chip.tone).toBe("warn");
    expect(chip.pulse).toBe(false);
    expect(chip.dismissable).toBe(true);
  });
});

describe("autolandStory", () => {
  it("joins the waiting reasons and names what landed so far", () => {
    const story = autolandStory(
      status(
        job({
          phase: {
            kind: "waiting",
            attention: false,
            reasons: ["checks running", "review pending"],
          },
          merged: [{ number: 1n, url: "https://x/1", bookmark: "auth" }],
        }),
      ),
    );
    expect(story).toContain("checks running; review pending");
    expect(story).toContain("Landed so far: #1");
  });

  it("sums up a finished job", () => {
    const story = autolandStory(
      status(
        job({
          phase: { kind: "done" },
          rounds: 2,
          merged: [
            { number: 1n, url: "https://x/1", bookmark: "auth" },
            { number: 7n, url: "https://x/7", bookmark: "profile" },
          ],
        }),
      ),
    );
    expect(story).toBe("The whole stack landed: #1, #7 in 2 rounds.");
  });

  it("carries the parked job's message", () => {
    expect(
      autolandStory(
        status(job({ phase: { kind: "failed", message: "GitHub is unreachable" } })),
      ),
    ).toContain("GitHub is unreachable");
  });

  it("tells the interrupted story with prior progress intact", () => {
    const story = autolandStory(
      status(
        job({
          merged: [{ number: 1n, url: "https://x/1", bookmark: "auth" }],
        }),
        { live: false },
      ),
    );
    expect(story).toContain("The app closed while this job was watching");
    expect(story).toContain("Landed so far: #1");
    const midRound = autolandStory(
      status(job({ phase: { kind: "round" } }), { live: false }),
    );
    expect(midRound).toContain("mid-round");
  });
});
