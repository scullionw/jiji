import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const updater = vi.hoisted(() => ({
  check: vi.fn(),
  relaunch: vi.fn(async () => {}),
}));

vi.mock("@tauri-apps/plugin-updater", () => ({ check: updater.check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: updater.relaunch }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  ask: vi.fn(async () => false),
  message: vi.fn(async () => {}),
}));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: async () => "0.4.0" }));

function fakeUpdate(version: string) {
  return {
    version,
    currentVersion: "0.4.0",
    download: vi.fn(async () => {}),
    install: vi.fn(async () => {}),
  };
}

beforeEach(() => {
  vi.resetModules();
  vi.stubEnv("PROD", true);
  updater.check.mockReset();
  updater.relaunch.mockClear();
});

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("app updates", () => {
  it("downloads a found update but waits for the user to restart", async () => {
    const update = fakeUpdate("0.4.1");
    updater.check.mockResolvedValue(update);
    const { appUpdate, startAppUpdateChecks } = await import("./update.svelte");

    const stop = startAppUpdateChecks();
    await vi.waitFor(() => expect(appUpdate.version).toBe("0.4.1"));
    stop();

    expect(update.download).toHaveBeenCalledOnce();
    expect(update.install).not.toHaveBeenCalled();
    expect(updater.relaunch).not.toHaveBeenCalled();
  });

  it("installs and relaunches only on restartToUpdate", async () => {
    const update = fakeUpdate("0.4.1");
    updater.check.mockResolvedValue(update);
    const { appUpdate, restartToUpdate, startAppUpdateChecks } = await import(
      "./update.svelte"
    );

    const stop = startAppUpdateChecks();
    await vi.waitFor(() => expect(appUpdate.version).toBe("0.4.1"));
    stop();
    await restartToUpdate();

    expect(update.install).toHaveBeenCalledOnce();
    expect(updater.relaunch).toHaveBeenCalledOnce();
  });

  it("stays quiet when there is nothing new", async () => {
    updater.check.mockResolvedValue(null);
    const { appUpdate, startAppUpdateChecks } = await import("./update.svelte");

    const stop = startAppUpdateChecks();
    await vi.waitFor(() => expect(updater.check).toHaveBeenCalled());
    stop();

    expect(appUpdate.version).toBeNull();
  });
});
