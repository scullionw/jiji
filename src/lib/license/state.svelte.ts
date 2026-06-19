// Licensing state and the supportive registration flow. The app and all core
// jj functionality are free; a one-time Polar license unlocks the theme
// system. Validation runs directly against Polar's public endpoints — no
// backend, no secret. See jiji-logbook/plans/POLAR_LICENSE_PLAN.md.

import {
  activate,
  validate,
  isEntitled,
  PolarError,
  type PolarValidation,
} from "./api";
import {
  loadRecord,
  saveRecord,
  clearRecord,
  getDeviceId,
  readRegisteredMirror,
  setRegisteredMirror,
  type LicenseRecord,
} from "./persist";

export type Plan = "solo" | "personal" | "unknown";

const DAY_MS = 24 * 60 * 60 * 1000;

// `registered` is seeded synchronously from the localStorage mirror, so it is
// already correct on first paint (matching app.html). loadLicenseState() then
// confirms it from license.json and revalidates.
export const license = $state({
  registered: readRegisteredMirror(),
  loaded: false,
  plan: "unknown" as Plan,
  displayKey: "",
});

// Lets the ThemeMenu's locked state open the popover that lives in the top bar.
export const registrationUI = $state({ open: false });
export function openRegistration(): void {
  registrationUI.open = true;
}

export function isRegistered(): boolean {
  return license.registered;
}

export function canUseThemes(): boolean {
  return license.registered;
}

let cached: LicenseRecord | null = null;

function planFromLimit(limit: number | null): Plan {
  if (limit === 1) return "solo";
  if (typeof limit === "number" && limit > 1) return "personal";
  return "unknown";
}

function maskKey(key: string): string {
  return `••••${key.slice(-4)}`;
}

function adopt(record: LicenseRecord): void {
  cached = record;
  license.registered = true;
  license.plan = record.plan;
  license.displayKey = maskKey(record.key);
}

function drop(): void {
  cached = null;
  license.registered = false;
  license.plan = "unknown";
  license.displayKey = "";
  setRegisteredMirror(false);
}

function recordFrom(
  key: string,
  activationId: string,
  deviceId: string,
  v: PolarValidation,
): LicenseRecord {
  return {
    key,
    activationId,
    deviceId,
    status: v.status,
    plan: planFromLimit(v.limit_activations),
    limitActivations: v.limit_activations,
    expiresAt: v.expires_at,
    registeredAt: cached?.registeredAt ?? Date.now(),
    lastValidatedAt: Date.now(),
  };
}

/** Load cached state at startup and revalidate opportunistically (≤ 1×/day). */
export async function loadLicenseState(): Promise<void> {
  try {
    const record = await loadRecord();
    if (record) {
      adopt(record);
      if (Date.now() - record.lastValidatedAt > DAY_MS) void refreshLicense();
    } else {
      drop();
    }
  } catch {
    // Store unreadable — fall back to the mirror we already seeded.
  } finally {
    license.loaded = true;
  }
}

/** Re-validate the cached license. Offline keeps the cache; an explicit
 *  negative response (revoked / not found) drops it. */
export async function refreshLicense(): Promise<void> {
  if (!cached) return;
  try {
    const v = await validate(cached.key, cached.activationId || null);
    if (isEntitled(v)) {
      const record = recordFrom(
        cached.key,
        cached.activationId,
        cached.deviceId,
        v,
      );
      await saveRecord(record);
      adopt(record);
    } else {
      await clearRecord();
      drop();
    }
  } catch (err) {
    // Only a definitive "gone" downgrades; network / server hiccups keep us
    // registered (offline-friendly, per the plan).
    if (err instanceof PolarError && err.status === 404) {
      await clearRecord();
      drop();
    }
  }
}

/** Activate + validate a freshly entered key. Throws a user-facing message. */
export async function activateLicense(rawKey: string): Promise<void> {
  const key = rawKey.trim();
  if (!key) throw new Error("Enter your license key.");

  const deviceId = await getDeviceId();

  // Re-entering the same key on this device: reuse the existing activation
  // rather than burning another slot.
  if (cached && cached.key === key && cached.activationId) {
    const v = await validate(key, cached.activationId);
    if (isEntitled(v)) {
      const record = recordFrom(key, cached.activationId, deviceId, v);
      await saveRecord(record);
      adopt(record);
      return;
    }
  }

  let activationId: string;
  try {
    const act = await activate(key, deviceLabel(deviceId), {
      device_id: deviceId,
    });
    activationId = act.id;
  } catch (err) {
    throw activationError(err);
  }

  const v = await validate(key, activationId);
  if (!isEntitled(v)) {
    throw new Error(
      "That license isn’t active right now. Contact support if this is unexpected.",
    );
  }

  const record = recordFrom(key, activationId, deviceId, v);
  await saveRecord(record);
  adopt(record);
}

function deviceLabel(deviceId: string): string {
  return `Jiji desktop (${deviceId.slice(0, 8)})`;
}

function activationError(err: unknown): Error {
  if (err instanceof PolarError) {
    if (err.status === 404) {
      return new Error(
        "We couldn’t find that license key. Check it and try again.",
      );
    }
    if (err.status === 403) {
      return new Error(
        "This license is already active on its maximum number of devices.",
      );
    }
    if (err.status === 0) {
      return new Error(
        "Couldn’t reach Polar. Check your connection and try again.",
      );
    }
  }
  return new Error("Something went wrong activating your license. Try again.");
}
