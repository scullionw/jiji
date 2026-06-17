// Persistence for license state. The full record lives in license.json (Tauri
// store plugin); a single `registered` bit is also mirrored to localStorage so
// the pre-paint boot script in app.html can gate themes synchronously — with no
// flash of a paid theme for an unregistered copy.

import { load, type Store } from "@tauri-apps/plugin-store";

const STORE_FILE = "license.json";
const STATE_KEY = "state";
const DEVICE_KEY = "deviceId";

/** localStorage key read synchronously by app.html's boot script. */
export const REGISTERED_FLAG = "jiji-registered";

export interface LicenseRecord {
  key: string;
  activationId: string;
  deviceId: string;
  status: string;
  plan: "solo" | "personal" | "unknown";
  limitActivations: number | null;
  expiresAt: string | null;
  registeredAt: number;
  lastValidatedAt: number;
}

let storePromise: Promise<Store> | null = null;
function getStore(): Promise<Store> {
  storePromise ??= load(STORE_FILE, { defaults: {}, autoSave: true });
  return storePromise;
}

export async function loadRecord(): Promise<LicenseRecord | null> {
  const store = await getStore();
  return (await store.get<LicenseRecord>(STATE_KEY)) ?? null;
}

export async function saveRecord(record: LicenseRecord): Promise<void> {
  const store = await getStore();
  await store.set(STATE_KEY, record);
  setRegisteredMirror(true);
}

export async function clearRecord(): Promise<void> {
  const store = await getStore();
  await store.delete(STATE_KEY);
  setRegisteredMirror(false);
}

/** A stable, random per-install id used as the Polar activation device. */
export async function getDeviceId(): Promise<string> {
  const store = await getStore();
  let id = await store.get<string>(DEVICE_KEY);
  if (!id) {
    id = crypto.randomUUID();
    await store.set(DEVICE_KEY, id);
  }
  return id;
}

export function setRegisteredMirror(registered: boolean): void {
  try {
    if (registered) localStorage.setItem(REGISTERED_FLAG, "1");
    else localStorage.removeItem(REGISTERED_FLAG);
  } catch {
    /* localStorage should always exist in the webview */
  }
}

export function readRegisteredMirror(): boolean {
  try {
    return localStorage.getItem(REGISTERED_FLAG) === "1";
  } catch {
    return false;
  }
}
