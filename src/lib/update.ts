import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";

let didCheck = false;

export async function checkForAppUpdate() {
  if (didCheck || !import.meta.env.PROD) return;
  didCheck = true;

  try {
    const update = await check();
    if (!update) return;

    await update.downloadAndInstall();
    await relaunch();
  } catch (error) {
    console.warn("App update check failed", error);
  }
}
