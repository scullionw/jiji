// Which terminal apps this machine has, and which one "Open in Terminal"
// launches. Detected once at bootstrap (stat calls, cheap); the choice is
// pinned in Settings and sticks per device. Empty detection (non-macOS,
// stubbed IPC) keeps the picker off screen and the backend's platform
// default in charge.

import * as api from "$lib/api";

const PREFERRED_TERMINAL_KEY = "jiji.terminal.preferred";

export const terminalState = $state({
  detected: [] as api.TerminalInfo[],
  // Terminal id pinned in Settings; null means the platform default.
  preferred: localStorage.getItem(PREFERRED_TERMINAL_KEY),
});

export async function syncTerminals(): Promise<void> {
  try {
    terminalState.detected = await api.detectTerminals();
  } catch (error) {
    console.warn("terminal detection failed", error);
  }
}

export function setPreferredTerminal(id: string | null): void {
  terminalState.preferred = id;
  if (id) localStorage.setItem(PREFERRED_TERMINAL_KEY, id);
  else localStorage.removeItem(PREFERRED_TERMINAL_KEY);
}

/// Open a directory in the pinned terminal while it is still installed,
/// else the platform default.
export function openInTerminal(path: string): Promise<void> {
  const { detected, preferred } = terminalState;
  const choice = detected.find((t) => t.id === preferred)?.id;
  return api.openInTerminal(path, choice);
}
