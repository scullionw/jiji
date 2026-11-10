//! App updates, against the same release feed and signing key the
//! previous shell used: a `latest.json` manifest on the GitHub release,
//! one signed `.app.tar.gz` per architecture.
//!
//! A found update downloads in the background and then waits — nothing
//! restarts until the user asks, so a release never interrupts a describe
//! or a split mid-edit. The archive is only ever installed after its
//! minisign signature verifies against the public key below.

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use serde::Deserialize;

const ENDPOINT: &str = "https://github.com/scullionw/jiji/releases/latest/download/latest.json";

/// The release signing key (minisign, base64 of the key file's text).
const PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDM2OEQ2QkI3QTIwNTQ0NkUKUldSdVJBV2l0MnVOTmtEeVgvVVFOdEhvM0JUWVdlNHpMd0V0Tm1uKzdhQXMrTE9UTGFGYndtelEK";

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    platforms: std::collections::HashMap<String, PlatformEntry>,
}

#[derive(Debug, Deserialize)]
struct PlatformEntry {
    signature: String,
    url: String,
}

/// A release newer than this build, not downloaded yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Available {
    pub version: String,
    url: String,
    signature: String,
}

/// A downloaded, signature-verified update waiting for a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct Staged {
    pub version: String,
    archive: PathBuf,
}

fn platform_key() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "darwin-aarch64"
    } else {
        "darwin-x86_64"
    }
}

/// The `.app` bundle this process runs from; `None` for an unbundled
/// binary (a dev build), which has nothing to replace.
pub fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
}

/// Whether this build can update itself at all.
pub fn supported() -> bool {
    cfg!(target_os = "macos") && !cfg!(debug_assertions) && bundle_path().is_some()
}

fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Whether `candidate` is a strictly newer release than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent("jiji-updater")
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|err| err.to_string())
}

/// Ask the release feed whether a newer version exists for this machine.
pub fn check() -> Result<Option<Available>, String> {
    let manifest: Manifest = client()?
        .get(ENDPOINT)
        .header("accept", "application/json")
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|err| err.to_string())?
        .json()
        .map_err(|err| err.to_string())?;
    if !is_newer(&manifest.version, CURRENT_VERSION) {
        return Ok(None);
    }
    let Some(entry) = manifest.platforms.get(platform_key()) else {
        return Ok(None);
    };
    Ok(Some(Available {
        version: manifest.version,
        url: entry.url.clone(),
        signature: entry.signature.clone(),
    }))
}

fn decode_text(encoded: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|err| err.to_string())?;
    String::from_utf8(bytes).map_err(|err| err.to_string())
}

fn verify(data: &[u8], signature: &str) -> Result<(), String> {
    let key =
        minisign_verify::PublicKey::decode(&decode_text(PUBKEY)?).map_err(|err| err.to_string())?;
    let signature = minisign_verify::Signature::decode(&decode_text(signature)?)
        .map_err(|err| err.to_string())?;
    key.verify(data, &signature, true)
        .map_err(|err| format!("the update's signature does not verify: {err}"))
}

/// Download an update and verify it; nothing is installed yet.
pub fn download(update: &Available, cache_dir: &Path) -> Result<Staged, String> {
    let bytes = client()?
        .get(&update.url)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.bytes())
        .map_err(|err| err.to_string())?;
    verify(&bytes, &update.signature)?;
    let dir = cache_dir.join("updates");
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let archive = dir.join(format!("Jiji-{}.app.tar.gz", update.version));
    std::fs::write(&archive, &bytes).map_err(|err| err.to_string())?;
    Ok(Staged {
        version: update.version.clone(),
        archive,
    })
}

/// Swap the running bundle for the staged one and relaunch into it. On
/// success the new instance is starting and the caller should quit.
pub fn install_and_relaunch(staged: &Staged) -> Result<(), String> {
    let bundle = bundle_path().ok_or("this build is not running from an app bundle")?;
    let parent = bundle
        .parent()
        .ok_or("the app bundle has no parent folder")?;
    let stage = staged.archive.with_extension("extracted");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|err| err.to_string())?;
    let status = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(&staged.archive)
        .arg("-C")
        .arg(&stage)
        .status()
        .map_err(|err| err.to_string())?;
    if !status.success() {
        return Err("the update archive could not be unpacked".into());
    }
    let fresh = std::fs::read_dir(&stage)
        .map_err(|err| err.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .ok_or("the update archive holds no app bundle")?;

    let backup = parent.join(format!(".Jiji-{}.old.app", std::process::id()));
    let swapped = std::fs::rename(&bundle, &backup).and_then(|()| {
        std::fs::rename(&fresh, &bundle)
            .or_else(|_| copy_bundle(&fresh, &bundle))
            .inspect_err(|_| {
                // Put the running version back rather than leave no app.
                let _ = std::fs::rename(&backup, &bundle);
            })
    });
    match swapped {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&backup);
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            privileged_swap(&fresh, &bundle)?;
        }
        Err(err) => return Err(format!("could not replace {}: {err}", bundle.display())),
    }
    let _ = std::fs::remove_dir_all(&stage);
    let _ = std::fs::remove_file(&staged.archive);
    Command::new("/usr/bin/open")
        .arg("-n")
        .arg(&bundle)
        .spawn()
        .map_err(|err| format!("the update installed, but relaunching failed: {err}"))?;
    Ok(())
}

/// A rename across volumes fails; `ditto` copies a bundle faithfully
/// (resource forks, signatures, symlinks).
fn copy_bundle(from: &Path, to: &Path) -> std::io::Result<()> {
    let status = Command::new("/usr/bin/ditto").arg(from).arg(to).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("ditto failed"))
    }
}

/// The app sits somewhere this user cannot write (a shared /Applications):
/// do the swap behind the native administrator prompt.
fn privileged_swap(fresh: &Path, bundle: &Path) -> Result<(), String> {
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"));
    let shell = format!(
        "rm -rf {bundle} && /usr/bin/ditto {fresh} {bundle}",
        bundle = quote(bundle),
        fresh = quote(fresh),
    );
    let apple = format!(
        r#"do shell script "{}" with administrator privileges"#,
        shell.replace('\\', "\\\\").replace('"', "\\\""),
    );
    let output = Command::new("osascript")
        .args(["-e", &apple])
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_strictly_newer_releases_count() {
        assert!(is_newer("0.4.2", "0.4.1"));
        assert!(is_newer("v0.5.0", "0.4.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.4.1", "0.4.1"));
        assert!(!is_newer("0.4.0", "0.4.1"));
        assert!(!is_newer("nonsense", "0.4.1"));
        // 0.10 is newer than 0.9: numeric, not lexical.
        assert!(is_newer("0.10.0", "0.9.0"));
    }

    #[test]
    fn the_signing_key_decodes() {
        let text = decode_text(PUBKEY).unwrap();
        assert!(text.starts_with("untrusted comment: minisign public key"));
        assert!(minisign_verify::PublicKey::decode(&text).is_ok());
    }

    #[test]
    fn a_bad_signature_is_refused() {
        assert!(verify(b"payload", "bm90IGEgc2lnbmF0dXJl").is_err());
    }

    #[test]
    fn manifests_parse() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"version":"0.5.0","notes":"x","pub_date":"2026-01-01T00:00:00Z",
                "platforms":{"darwin-aarch64":{"signature":"c2ln","url":"https://example.invalid/a.tar.gz"}}}"#,
        )
        .unwrap();
        assert_eq!(manifest.version, "0.5.0");
        assert_eq!(manifest.platforms["darwin-aarch64"].signature, "c2ln");
    }
}
