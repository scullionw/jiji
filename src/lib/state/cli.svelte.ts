// The `jiji` terminal launcher: install state and the welcome-screen
// offer. Status loads once at bootstrap; install/uninstall replace it with
// what the backend re-read after mutating, so the offer, the palette rows,
// and the confirmation all follow the launcher's actual on-disk state.

import * as api from "$lib/api";

// Declining the offer is a durable choice — the card never nags again;
// the command palette stays the route back in.
const DISMISSED_KEY = "jiji.cli.offer-dismissed";

export const cli = $state({
  // null until loaded — and forever on a platform without a launcher
  // (status refuses there), which keeps every affordance off screen.
  status: null as api.CliStatus | null,
  dismissed: localStorage.getItem(DISMISSED_KEY) === "1",
  installing: false,
  // Keeps the welcome card up after a successful install to confirm
  // where the command landed (the not-installed condition just vanished).
  justInstalled: false,
});

export async function syncCliStatus(): Promise<void> {
  try {
    cli.status = await api.cliStatus();
  } catch {
    cli.status = null;
  }
}

// Errors propagate (like every action) so callers route them to their
// error slot — except declining the admin prompt, which is a decision,
// not a failure.
export async function installCli(): Promise<void> {
  cli.installing = true;
  try {
    cli.status = await api.installCli();
    cli.justInstalled = true;
  } catch (error) {
    if ((error as api.CommandError | null)?.code === "install_canceled") return;
    throw error;
  } finally {
    cli.installing = false;
  }
}

export async function uninstallCli(): Promise<void> {
  cli.status = await api.uninstallCli();
  cli.justInstalled = false;
}

export function dismissCliOffer(): void {
  cli.dismissed = true;
  localStorage.setItem(DISMISSED_KEY, "1");
}
