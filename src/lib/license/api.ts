// Thin client for Polar's public customer-portal license-key endpoints.
//
// These endpoints are CORS-enabled and need no authentication — only the
// public organization id — so we call them straight from the webview with
// fetch(). No secret or private token ever ships in the app.

import { POLAR_LICENSE_API, POLAR_ORG_ID } from "./config";

export type LicenseStatus = "granted" | "revoked" | "disabled";

/** Subset of Polar's LicenseKeyRead that we read. */
export interface PolarLicenseKey {
  id: string;
  organization_id: string;
  benefit_id: string;
  key: string;
  display_key: string;
  status: LicenseStatus;
  limit_activations: number | null;
  usage: number;
  limit_usage: number | null;
  expires_at: string | null;
}

/** Polar's LicenseKeyActivationRead (subset). */
export interface PolarActivation {
  id: string;
  license_key_id: string;
  label: string;
  license_key: PolarLicenseKey;
}

/** validate() returns the key fields plus its bound activation, if any. */
export interface PolarValidation extends PolarLicenseKey {
  activation: { id: string } | null;
}

/** A Polar API error. `status === 0` means the request never reached Polar. */
export class PolarError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "PolarError";
  }
}

async function post<T>(
  endpoint: string,
  body: Record<string, unknown>,
): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`${POLAR_LICENSE_API}/${endpoint}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  } catch {
    throw new PolarError(0, "network", "Could not reach Polar.");
  }
  const data: unknown = await res.json().catch(() => null);
  if (!res.ok) {
    const obj = (data ?? {}) as { error?: string; detail?: string };
    throw new PolarError(
      res.status,
      obj.error ?? String(res.status),
      obj.detail ?? res.statusText,
    );
  }
  return data as T;
}

/** Register this device against a key; the returned `id` is the activation id. */
export function activate(
  key: string,
  label: string,
  meta?: Record<string, string>,
): Promise<PolarActivation> {
  return post("activate", {
    key,
    organization_id: POLAR_ORG_ID,
    label,
    ...(meta ? { meta } : {}),
  });
}

/** Validate a key, optionally bound to a prior activation. */
export function validate(
  key: string,
  activationId?: string | null,
): Promise<PolarValidation> {
  return post("validate", {
    key,
    organization_id: POLAR_ORG_ID,
    ...(activationId ? { activation_id: activationId } : {}),
  });
}

/** A granted, unexpired key entitles the user. */
export function isEntitled(key: {
  status: LicenseStatus;
  expires_at: string | null;
}): boolean {
  if (key.status !== "granted") return false;
  if (key.expires_at && Date.parse(key.expires_at) < Date.now()) return false;
  return true;
}
