// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LandPlan } from "$lib/bindings/LandPlan";
import type { AutoLandStatus } from "$lib/bindings/AutoLandStatus";
import { forge } from "$lib/state/forge.svelte";
import { app } from "$lib/state/app.svelte";
import { autoland } from "$lib/state/autoland.svelte";
import * as api from "$lib/api";
import { publishFixture } from "./publish.fixtures";
import PublishWorkspace from "./PublishWorkspace.svelte";

vi.mock("$lib/api", () => ({
  publishStacks: vi.fn(),
  landPlan: vi.fn(),
  submitPlan: vi.fn(),
  landStack: vi.fn(),
  autolandStart: vi.fn(),
  autolandStop: vi.fn(),
  forgePrs: vi.fn(),
  errorMessage: (error: unknown) => String(error),
}));
vi.mock("$lib/state/actions", () => ({
  applySnapshot: vi.fn(),
  consumeIntent: vi.fn(),
  fetchPr: vi.fn(),
  jumpToChange: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("$lib/motion", () => ({ motionMs: () => 0 }));

function landPlan(): LandPlan {
  const stack = publishFixture().stacks[0];
  return {
    headBookmark: stack.headBookmark!,
    remote: "origin",
    baseBranch: "develop",
    options: { method: null, bypass: false },
    segments: stack.segments.map((segment, index) => ({
      bookmark: segment.bookmark!,
      changeIds: segment.changes.map((node) => node.id),
      pr: segment.pr,
      status: { kind: index ? "stacked" : "landing" },
    })),
    actions: [{ kind: "markReadyPr", number: 753n, bookmark: "ws/tu1" }],
    blockers: [],
    warnings: [],
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  const { snapshot, prs } = publishFixture();
  app.snapshot = snapshot;
  app.intent = null;
  forge.phase = "ready";
  forge.status = {
    repo: {
      provider: "gitHub",
      host: "github.com",
      owner: "org",
      name: "repo",
      remote: "origin",
    },
    auth: { source: "ghCli", login: "dev" },
  };
  forge.prs = prs;
  forge.prsLoading = false;
  forge.prsError = null;
  forge.error = null;
  autoland.jobs = [];
  autoland.stopping = false;
  vi.mocked(api.publishStacks).mockImplementation(
    async () => publishFixture().stacks,
  );
  vi.mocked(api.landPlan).mockResolvedValue(landPlan());
  vi.mocked(api.forgePrs).mockResolvedValue(prs);
  vi.mocked(api.submitPlan).mockResolvedValue({
    headBookmark: "scratch",
    remote: "origin",
    baseBranch: "develop",
    segments: [],
    actions: [],
    blockers: [],
    warnings: [],
    stackCommentPreview: null,
    prTemplatePath: null,
  });
});
afterEach(cleanup);

describe("Publish workspace", () => {
  it("opens the current jjpr stack with all PRs and landing controls visible", async () => {
    const view = render(PublishWorkspace, { snapshot: app.snapshot! });
    await screen.findByText("Auto-land when ready");
    expect(
      screen.getByRole("heading", {
        name: "Resolve trips on their own schedules",
        level: 2,
      }),
    ).toBeTruthy();
    expect(
      view.container.querySelectorAll("[data-publish-segment]"),
    ).toHaveLength(5);
    expect(api.landPlan).toHaveBeenCalledWith("ws/tu3-emission", {
      method: null,
      bypass: false,
    });
    expect(view.container.querySelector("[data-review-pr]")).toBeNull();
    expect(
      view.container.querySelector('[data-stack-select="scratch"]'),
    ).toBeNull();
    expect(
      screen.getByText("Draft PRs will be marked ready as they land."),
    ).toBeTruthy();
    expect(api.landStack).not.toHaveBeenCalled();
  });

  it("reveals local work on demand and switches to its publish plan", async () => {
    const view = render(PublishWorkspace, { snapshot: app.snapshot! });
    await screen.findByText("Auto-land when ready");
    await fireEvent.click(screen.getByRole("button", { name: /Local work/ }));
    await fireEvent.click(
      view.container.querySelector('[data-stack-select="scratch"]')!,
    );
    await screen.findByRole("heading", { name: "All up to date" });
    expect(api.submitPlan).toHaveBeenCalledWith({
      kind: "change",
      change: "scratch",
    });
    expect(
      view.container.querySelectorAll("[data-publish-segment]"),
    ).toHaveLength(1);
  });

  it("keeps the PR browser off the stack page and preserves the selected stack", async () => {
    const view = render(PublishWorkspace, { snapshot: app.snapshot! });
    await screen.findByText("Auto-land when ready");
    await fireEvent.click(
      screen.getByRole("button", { name: /Pull requests/ }),
    );
    expect(view.container.querySelectorAll("[data-review-pr]")).toHaveLength(5);
    await fireEvent.click(screen.getByRole("button", { name: /^Stacks/ }));
    expect(
      view.container.querySelectorAll("[data-publish-segment]"),
    ).toHaveLength(5);
    expect(api.landPlan).toHaveBeenCalledTimes(1);
  });

  it("replans with the actual method and bypass selections", async () => {
    const view = render(PublishWorkspace, { snapshot: app.snapshot! });
    await screen.findByText("Auto-land when ready");
    await fireEvent.change(
      view.container.querySelector("[data-land-method]")!,
      { target: { value: "rebase" } },
    );
    await waitFor(() =>
      expect(api.landPlan).toHaveBeenLastCalledWith("ws/tu3-emission", {
        method: "rebase",
        bypass: false,
      }),
    );
    await screen.findByText("Auto-land when ready");
    await fireEvent.click(view.container.querySelector("[data-land-bypass]")!);
    await waitFor(() =>
      expect(api.landPlan).toHaveBeenLastCalledWith("ws/tu3-emission", {
        method: "rebase",
        bypass: true,
      }),
    );
  });

  it("ignores a late land plan after selecting a different stack", async () => {
    let resolve!: (plan: LandPlan) => void;
    vi.mocked(api.landPlan).mockReturnValue(
      new Promise((answer) => (resolve = answer)),
    );
    const view = render(PublishWorkspace, { snapshot: app.snapshot! });
    await screen.findByText("Checking readiness…");
    await fireEvent.click(screen.getByRole("button", { name: /Local work/ }));
    await fireEvent.click(
      view.container.querySelector('[data-stack-select="scratch"]')!,
    );
    resolve(landPlan());
    await screen.findByRole("heading", { name: "All up to date" });
    expect(view.container.querySelector("[data-land-plan]")).toBeNull();
  });

  it("queues the selected stack and lets the user stop the live job", async () => {
    const plan = landPlan();
    const job: AutoLandStatus = {
      live: true,
      record: {
        repoPath: app.snapshot!.repoPath,
        savedAtMs: 0n,
        state: {
          headBookmark: plan.headBookmark,
          options: plan.options,
          phase: {
            kind: "waiting",
            attention: false,
            reasons: ["Waiting for review"],
          },
          rounds: 0,
          merged: [],
          segments: plan.segments.map((segment, index) =>
            index
              ? segment
              : {
                  ...segment,
                  pr: {
                    ...segment.pr!,
                    isDraft: false,
                    checks: "pending",
                    review: "reviewRequired",
                  },
                },
          ),
          lastOutcome: null,
        },
      },
    };
    vi.mocked(api.autolandStart).mockResolvedValue([job]);
    vi.mocked(api.autolandStop).mockResolvedValue([
      {
        ...job,
        live: false,
        record: {
          ...job.record,
          state: { ...job.record.state, phase: { kind: "stopped" } },
        },
      },
    ]);
    const view = render(PublishWorkspace, { snapshot: app.snapshot! });
    await fireEvent.click(
      await screen.findByRole("button", { name: "Auto-land when ready" }),
    );
    expect(api.autolandStart).toHaveBeenCalledWith("ws/tu3-emission", {
      method: null,
      bypass: false,
    });
    await screen.findByText("Auto-landing");
    expect(view.container.querySelector("[data-autoland-queue]")).toBeNull();
    const firstPr = view.container.querySelector("[data-publish-segment]")!;
    expect(firstPr.textContent).toContain("Checks running");
    expect(firstPr.textContent).not.toContain("Draft pull request");
    await fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await screen.findByText("Stopped");
    expect(api.autolandStop).toHaveBeenCalledOnce();
  });
});
