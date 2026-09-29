// App updates. A found update downloads in the background and then waits:
// the status bar offers "Restart to update", and nothing restarts until the
// user asks, so a release never interrupts a describe or a split mid-edit.
// The menu bar's "Check for Updates…" reports every outcome instead.

import { ask, message } from "@tauri-apps/plugin-dialog";
import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";

// Windows stay open for days; look again now and then.
const RECHECK_MS = 6 * 60 * 60 * 1000;

/** The downloaded update waiting for a restart, if any. */
export const appUpdate = $state({
  version: null as string | null,
  installing: false,
});

let staged: Update | null = null;
let checking = false;

async function stage(update: Update): Promise<void> {
  await update.download();
  staged = update;
  appUpdate.version = update.version;
}

async function checkQuietly(): Promise<void> {
  if (staged || checking) return;
  checking = true;
  try {
    const update = await check();
    if (update) await stage(update);
  } catch (error) {
    console.warn("App update check failed", error);
  } finally {
    checking = false;
  }
}

/** Checks at startup and every few hours; release builds only. */
export function startAppUpdateChecks(): () => void {
  if (!import.meta.env.PROD) return () => {};
  void checkQuietly();
  const timer = setInterval(() => void checkQuietly(), RECHECK_MS);
  return () => clearInterval(timer);
}

/** Installs the downloaded update and relaunches into it. */
export async function restartToUpdate(): Promise<void> {
  if (!staged || appUpdate.installing) return;
  appUpdate.installing = true;
  try {
    await staged.install();
    await relaunch();
  } catch (error) {
    appUpdate.installing = false;
    console.warn("App update install failed", error);
    await message(`The update could not be installed: ${error}`, {
      title: "Update failed",
      kind: "warning",
    });
  }
}

// The menu bar's "Check for Updates…": every outcome gets told to the
// user's face — found (with a choice), already current, or failed.
export async function checkForAppUpdateNow(): Promise<void> {
  try {
    if (staged) {
      const restart = await ask(
        `Jiji ${staged.version} is downloaded. Restart now to update?`,
        { title: "Update ready", okLabel: "Restart", cancelLabel: "Later" },
      );
      if (restart) await restartToUpdate();
      return;
    }
    const update = await check();
    if (!update) {
      await message(`Jiji ${await getVersion()} is the latest version.`, {
        title: "You're up to date",
      });
      return;
    }
    const install = await ask(
      `Jiji ${update.version} is available — you have ${update.currentVersion}. Update now?`,
      { title: "Update available", okLabel: "Update", cancelLabel: "Later" },
    );
    if (!install) {
      // Keep it for later: the status bar offers the restart once it lands.
      void stage(update).catch((error) =>
        console.warn("App update download failed", error),
      );
      return;
    }
    await stage(update);
    await restartToUpdate();
  } catch (error) {
    console.warn("App update check failed", error);
    await message(`The update check failed: ${error}`, {
      title: "Check for Updates",
      kind: "warning",
    });
  }
}
