//! Licensing against Polar's public customer-portal license-key endpoints.
//!
//! The app and all core jj functionality are free; a one-time license
//! unlocks the theme system. Every value here is public and safe to ship:
//! validation needs only the organization id — never a secret or private
//! token.

use serde::Deserialize;
use serde_json::json;

use crate::store::{now_ms, LicenseRecord};

/// Polar organization that owns the Jiji products and license-key benefit.
const POLAR_ORG_ID: &str = "a37fb037-8fc7-4c87-982f-e9b4720237a8";
const POLAR_LICENSE_API: &str = "https://api.polar.sh/v1/customer-portal/license-keys";

/// Hosted Polar checkout links for the two one-time products.
pub const SOLO_CHECKOUT_URL: &str =
    "https://buy.polar.sh/polar_cl_5MwwECsz2qK0oUEHHLfeXktj9kSxsjlnRWOA60jKFGl";
pub const PERSONAL_CHECKOUT_URL: &str =
    "https://buy.polar.sh/polar_cl_2vELWWSZMb7KoKqDjsILXonFNpeHyS1JtwZxg2ixOUw";
pub const SOLO_PRICE: &str = "$15";
pub const PERSONAL_PRICE: &str = "$20";

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// The fields of Polar's license-key answer that Jiji reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Validation {
    pub status: String,
    pub limit_activations: Option<u64>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Activation {
    id: String,
}

/// A Polar API failure. `status == 0` means the request never reached
/// Polar.
#[derive(Debug)]
pub struct PolarError {
    pub status: u16,
    /// Polar's own words, for the log.
    pub message: String,
}

impl PolarError {
    /// A failure that says nothing about the key: Polar unreachable, slow,
    /// busy or broken. Asking again later may well work.
    fn is_transient(&self) -> bool {
        matches!(self.status, 0 | 408 | 429) || self.status >= 500
    }
}

/// Polar's license-key endpoints, behind a seam so tests can answer for
/// them.
pub trait Polar {
    fn validate(&self, key: &str, activation_id: Option<&str>) -> Result<Validation, PolarError>;
    /// Register `device_id` against `key`; the new activation's id.
    fn activate(&self, key: &str, device_id: &str) -> Result<String, PolarError>;
}

/// The real thing, over HTTPS.
pub struct PolarApi;

fn post<T: serde::de::DeserializeOwned>(
    endpoint: &str,
    body: serde_json::Value,
) -> Result<T, PolarError> {
    let unreachable = |_| PolarError {
        status: 0,
        message: "Could not reach Polar.".into(),
    };
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(unreachable)?;
    let response = client
        .post(format!("{POLAR_LICENSE_API}/{endpoint}"))
        .json(&body)
        .send()
        .map_err(unreachable)?;
    let status = response.status();
    if !status.is_success() {
        let detail = response
            .json::<serde_json::Value>()
            .ok()
            .and_then(|data| {
                data.get("detail")
                    .and_then(|d| d.as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| status.to_string());
        return Err(PolarError {
            status: status.as_u16(),
            message: detail,
        });
    }
    response.json::<T>().map_err(|err| PolarError {
        status: status.as_u16(),
        message: err.to_string(),
    })
}

impl Polar for PolarApi {
    fn validate(&self, key: &str, activation_id: Option<&str>) -> Result<Validation, PolarError> {
        let mut body = json!({ "key": key, "organization_id": POLAR_ORG_ID });
        if let Some(id) = activation_id.filter(|id| !id.is_empty()) {
            body["activation_id"] = json!(id);
        }
        post("validate", body)
    }

    fn activate(&self, key: &str, device_id: &str) -> Result<String, PolarError> {
        post::<Activation>(
            "activate",
            json!({
                "key": key,
                "organization_id": POLAR_ORG_ID,
                "label": format!("Jiji desktop ({})", &device_id[..device_id.len().min(8)]),
                "meta": { "device_id": device_id },
            }),
        )
        .map(|activation| activation.id)
    }
}

/// A granted, unexpired key entitles the user.
fn is_entitled(validation: &Validation) -> bool {
    if validation.status != "granted" {
        return false;
    }
    match &validation.expires_at {
        Some(expiry) => chrono::DateTime::parse_from_rfc3339(expiry)
            .map(|at| at.timestamp_millis() >= now_ms() as i64)
            .unwrap_or(true),
        None => true,
    }
}

pub fn plan_from_limit(limit: Option<u64>) -> &'static str {
    match limit {
        Some(1) => "solo",
        Some(n) if n > 1 => "personal",
        _ => "unknown",
    }
}

pub fn mask_key(key: &str) -> String {
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("••••{tail}")
}

fn record_from(
    key: &str,
    activation_id: &str,
    device_id: &str,
    validation: &Validation,
    prior: Option<&LicenseRecord>,
) -> LicenseRecord {
    LicenseRecord {
        key: key.to_owned(),
        activation_id: activation_id.to_owned(),
        device_id: device_id.to_owned(),
        status: validation.status.clone(),
        plan: plan_from_limit(validation.limit_activations).to_owned(),
        limit_activations: validation.limit_activations,
        expires_at: validation.expires_at.clone(),
        registered_at: prior.map_or_else(now_ms, |record| record.registered_at),
        last_validated_at: now_ms(),
    }
}

/// Whether a cached record is due its opportunistic revalidation (at most
/// once a day).
pub fn needs_refresh(record: &LicenseRecord) -> bool {
    now_ms().saturating_sub(record.last_validated_at) > DAY_MS
}

/// What a revalidation concluded.
pub enum Refresh {
    /// Still entitled; the record to store.
    Keep(LicenseRecord),
    /// A definitive negative (revoked, expired, not found): drop it.
    Drop,
    /// Polar could not be reached or hiccuped: keep what is cached
    /// (offline-friendly).
    Unknown,
}

/// Re-validate a cached license.
pub fn refresh(polar: &impl Polar, record: &LicenseRecord) -> Refresh {
    match polar.validate(&record.key, Some(&record.activation_id)) {
        Ok(validation) if is_entitled(&validation) => Refresh::Keep(record_from(
            &record.key,
            &record.activation_id,
            &record.device_id,
            &validation,
            Some(record),
        )),
        Ok(_) => Refresh::Drop,
        Err(err) if err.status == 404 => Refresh::Drop,
        Err(_) => Refresh::Unknown,
    }
}

/// Activate and validate a freshly entered key. The error is the message
/// to show.
pub fn activate_license(
    polar: &impl Polar,
    raw_key: &str,
    device_id: &str,
    cached: Option<&LicenseRecord>,
) -> Result<LicenseRecord, String> {
    let key = raw_key.trim();
    if key.is_empty() {
        return Err("Enter your license key.".into());
    }
    let generic = || "Something went wrong activating your license. Try again.".to_owned();
    let unreachable = || "Couldn’t reach Polar. Check your connection and try again.".to_owned();

    // Re-entering the same key on this device: reuse the existing
    // activation rather than burning another slot.
    if let Some(record) = cached.filter(|r| r.key == key && !r.activation_id.is_empty()) {
        match polar.validate(key, Some(&record.activation_id)) {
            Ok(validation) if is_entitled(&validation) => {
                return Ok(record_from(
                    key,
                    &record.activation_id,
                    device_id,
                    &validation,
                    cached,
                ));
            }
            // Polar could not say: a new activation now could spend a
            // device slot, or meet the device limit, for a key this device
            // already holds. Nothing changes; asking again will tell.
            Err(err) if err.is_transient() => {
                log::warn!(
                    "could not validate this device's activation ({}): {}",
                    err.status,
                    err.message
                );
                return Err(match err.status {
                    0 => unreachable(),
                    _ => "Polar couldn’t check your key just now. Nothing changed — try again \
                          in a moment."
                        .into(),
                });
            }
            // Revoked or expired, or an activation Polar no longer knows: a
            // new activation is the way back, and its answer the one to
            // show.
            Ok(_) | Err(_) => {}
        }
    }

    let activation = polar.activate(key, device_id).map_err(|err| {
        log::warn!(
            "Polar refused the activation ({}): {}",
            err.status,
            err.message
        );
        match err.status {
            404 => "We couldn’t find that license key. Check it and try again.".to_owned(),
            403 => "This license is already active on its maximum number of devices.".to_owned(),
            0 => unreachable(),
            _ => generic(),
        }
    })?;
    let validation = polar
        .validate(key, Some(&activation))
        .map_err(|_| generic())?;
    if !is_entitled(&validation) {
        return Err(
            "That license isn’t active right now. Contact support if this is unexpected.".into(),
        );
    }
    Ok(record_from(
        key,
        &activation,
        device_id,
        &validation,
        cached,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_follow_the_activation_limit() {
        assert_eq!(plan_from_limit(Some(1)), "solo");
        assert_eq!(plan_from_limit(Some(3)), "personal");
        assert_eq!(plan_from_limit(None), "unknown");
        assert_eq!(plan_from_limit(Some(0)), "unknown");
    }

    #[test]
    fn entitlement_needs_a_granted_unexpired_key() {
        let granted = |expires_at: Option<&str>| Validation {
            status: "granted".into(),
            limit_activations: None,
            expires_at: expires_at.map(str::to_owned),
        };
        assert!(is_entitled(&granted(None)));
        assert!(is_entitled(&granted(Some("2999-01-01T00:00:00Z"))));
        assert!(!is_entitled(&granted(Some("2001-01-01T00:00:00Z"))));
        assert!(!is_entitled(&Validation {
            status: "revoked".into(),
            ..granted(None)
        }));
    }

    #[test]
    fn keys_display_masked() {
        assert_eq!(mask_key("JIJI-ABCD-1234"), "••••1234");
    }

    /// Polar as a test needs it: validation answers `fails` in turn (a
    /// status per call) and then grants the key; activations are counted.
    struct StubPolar {
        fails: std::cell::RefCell<Vec<u16>>,
        activations: std::cell::Cell<u32>,
    }

    impl StubPolar {
        fn failing(statuses: &[u16]) -> Self {
            Self {
                fails: std::cell::RefCell::new(statuses.to_vec()),
                activations: std::cell::Cell::new(0),
            }
        }
    }

    impl Polar for StubPolar {
        fn validate(&self, _: &str, _: Option<&str>) -> Result<Validation, PolarError> {
            let mut fails = self.fails.borrow_mut();
            if !fails.is_empty() {
                return Err(PolarError {
                    status: fails.remove(0),
                    message: "stubbed".into(),
                });
            }
            Ok(Validation {
                status: "granted".into(),
                limit_activations: Some(3),
                expires_at: None,
            })
        }

        fn activate(&self, _: &str, _: &str) -> Result<String, PolarError> {
            self.activations.set(self.activations.get() + 1);
            Ok("activation-new".into())
        }
    }

    fn registered() -> LicenseRecord {
        LicenseRecord {
            key: "JIJI-ABCD-1234".into(),
            activation_id: "activation-1".into(),
            device_id: "device-1".into(),
            status: "granted".into(),
            plan: "personal".into(),
            limit_activations: Some(3),
            expires_at: None,
            registered_at: 1,
            last_validated_at: 1,
        }
    }

    #[test]
    fn re_entering_a_key_while_polar_hiccups_activates_nothing() {
        let cached = registered();
        for status in [0, 429, 503] {
            let polar = StubPolar::failing(&[status]);
            let refused = activate_license(&polar, "JIJI-ABCD-1234", "device-1", Some(&cached));
            assert!(refused.is_err(), "{status}");
            assert_eq!(polar.activations.get(), 0, "{status}");
            // Asking again, once Polar answers, reuses this device's
            // activation.
            let retried =
                activate_license(&polar, " JIJI-ABCD-1234 ", "device-1", Some(&cached)).unwrap();
            assert_eq!(polar.activations.get(), 0);
            assert_eq!(
                (retried.activation_id.as_str(), retried.device_id.as_str()),
                ("activation-1", "device-1")
            );
            assert_eq!(retried.registered_at, cached.registered_at);
        }
    }

    #[test]
    fn an_activation_polar_no_longer_knows_is_replaced() {
        let polar = StubPolar::failing(&[404]);
        let cached = registered();
        let record = activate_license(&polar, "JIJI-ABCD-1234", "device-1", Some(&cached)).unwrap();
        assert_eq!(polar.activations.get(), 1);
        assert_eq!(record.activation_id, "activation-new");
    }
}
