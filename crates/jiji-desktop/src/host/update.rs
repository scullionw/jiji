//! App updates from the release feed: a `latest.json` manifest on the
//! newest GitHub release, naming one signed `.app.tar.gz` per architecture
//! (the release workflow publishes a universal archive under both).
//!
//! A found update downloads in the background and then waits — nothing
//! restarts until the user asks, so a release never interrupts a describe
//! or a split mid-edit. The archive is only ever installed after its
//! minisign signature verifies against the public key below, and it is
//! verified twice: once downloaded, and again from the bytes that unpack
//! at the restart, so a file changed in between never installs.
//!
//! The manifest, the archive layout and the signature encoding are the
//! Tauri updater's, which the first releases shipped with: installs from
//! then read this same feed, so its format is fixed.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// What the archive verified against, to verify it again at install.
    signature: String,
}

/// Why a staged update did not install.
#[derive(Debug, PartialEq)]
pub enum InstallError {
    /// The archive on disk is no longer the one that verified. It is
    /// deleted, and the update has to download again.
    Unverified,
    /// Anything else; the staged update can be tried again.
    Failed(String),
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::Unverified => f.write_str(
                "the downloaded update was changed or removed after it was verified. Your \
                 installed Jiji is unchanged, and the update will download again.",
            ),
            InstallError::Failed(why) => f.write_str(why),
        }
    }
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

/// Whether this build updates itself: a release build running from an app
/// bundle. Anything else has no bundle to replace.
pub fn supported() -> bool {
    cfg!(target_os = "macos") && !cfg!(debug_assertions) && bundle_path().is_some()
}

/// A release version as semantic versioning orders it: the three numbers,
/// then the prerelease identifiers. Build metadata is checked for form and
/// then dropped — it never orders.
#[derive(Debug, PartialEq, Eq)]
struct Version {
    core: [u64; 3],
    pre: Vec<Identifier>,
}

/// One dot-separated prerelease identifier. Numeric ones order below
/// alphanumeric ones, numerically; alphanumeric ones in ASCII order.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Identifier {
    Numeric(u64),
    Alphanumeric(String),
}

/// Digits with no leading zero ("0" itself is fine), as the version's
/// numbers and numeric identifiers must be.
fn number(text: &str) -> Option<u64> {
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    (digits && (text == "0" || !text.starts_with('0')))
        .then(|| text.parse().ok())
        .flatten()
}

/// Dot-separated identifiers of ASCII letters, digits and hyphens, none
/// empty.
fn identifiers(text: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = text.split('.').collect();
    parts
        .iter()
        .all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .then_some(parts)
}

impl Version {
    /// `MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]`, with the `v` a tag may
    /// carry; anything else is not a version.
    fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let (text, build) = match text.split_once('+') {
            Some((text, build)) => (text, Some(build)),
            None => (text, None),
        };
        if build.is_some_and(|build| identifiers(build).is_none()) {
            return None;
        }
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, identifiers(pre)?),
            None => (text, Vec::new()),
        };
        let numbers: Vec<u64> = core.split('.').map(number).collect::<Option<_>>()?;
        let pre = pre
            .into_iter()
            .map(|part| {
                if part.bytes().all(|byte| byte.is_ascii_digit()) {
                    number(part).map(Identifier::Numeric)
                } else {
                    Some(Identifier::Alphanumeric(part.to_owned()))
                }
            })
            .collect::<Option<_>>()?;
        Some(Self {
            core: numbers.try_into().ok()?,
            pre,
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // A prerelease comes before its release.
        self.core
            .cmp(&other.core)
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Whether `candidate` is a strictly newer release than `current`; `None`
/// when either is not a version.
fn is_newer(candidate: &str, current: &str) -> Option<bool> {
    Some(Version::parse(candidate)? > Version::parse(current)?)
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
    release_in(manifest, CURRENT_VERSION)
}

/// What a manifest offers a build at `current`. A feed naming something
/// that is not a version is broken, not up to date.
fn release_in(mut manifest: Manifest, current: &str) -> Result<Option<Available>, String> {
    let newer = is_newer(&manifest.version, current).ok_or_else(|| {
        format!(
            "the release feed names \u{201c}{}\u{201d}, which is not a version",
            manifest.version
        )
    })?;
    let entry = manifest.platforms.remove(platform_key());
    Ok(entry.filter(|_| newer).map(|entry| Available {
        version: manifest.version,
        url: entry.url,
        signature: entry.signature,
    }))
}

fn decode_text(encoded: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|err| err.to_string())?;
    String::from_utf8(bytes).map_err(|err| err.to_string())
}

fn verify(data: &[u8], signature: &str) -> Result<(), String> {
    verify_with(PUBKEY, data, signature)
}

fn verify_with(key: &str, data: &[u8], signature: &str) -> Result<(), String> {
    let key =
        minisign_verify::PublicKey::decode(&decode_text(key)?).map_err(|err| err.to_string())?;
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
        signature: update.signature.clone(),
    })
}

/// Swap the running bundle for the staged one and relaunch into it. On
/// success the new instance is starting and the caller should quit.
pub fn install_and_relaunch(staged: &Staged) -> Result<(), InstallError> {
    let bundle = bundle_path().ok_or_else(|| {
        InstallError::Failed("this build is not running from an app bundle".into())
    })?;
    install(PUBKEY, staged, &bundle)?;
    Command::new("/usr/bin/open")
        .arg("-n")
        .arg(&bundle)
        .spawn()
        .map_err(|err| {
            InstallError::Failed(format!(
                "the update installed, but relaunching failed: {err}"
            ))
        })?;
    Ok(())
}

/// Put the staged update in `bundle`'s place. The archive is read back and
/// verified again, and those bytes — not the file — are what unpack: what
/// installs is exactly what verified, whatever happened to the file since.
fn install(key: &str, staged: &Staged, bundle: &Path) -> Result<(), InstallError> {
    let archive = std::fs::read(&staged.archive)
        .ok()
        .filter(|bytes| verify_with(key, bytes, &staged.signature).is_ok());
    let Some(archive) = archive else {
        let _ = std::fs::remove_file(&staged.archive);
        return Err(InstallError::Unverified);
    };
    let parent = bundle
        .parent()
        .ok_or_else(|| InstallError::Failed("the app bundle has no parent folder".into()))?;
    let failed = |err: std::io::Error| InstallError::Failed(err.to_string());
    let stage = staged.archive.with_extension("extracted");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(failed)?;
    unpack(&archive, &stage).map_err(InstallError::Failed)?;
    let fresh = std::fs::read_dir(&stage)
        .map_err(failed)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .ok_or_else(|| InstallError::Failed("the update archive holds no app bundle".into()))?;

    let backup = parent.join(format!(".Jiji-{}.old.app", std::process::id()));
    let swapped = std::fs::rename(bundle, &backup).and_then(|()| {
        std::fs::rename(&fresh, bundle)
            .or_else(|_| copy_bundle(&fresh, bundle))
            .inspect_err(|_| {
                // Put the running version back rather than leave no app.
                let _ = std::fs::rename(&backup, bundle);
            })
    });
    match swapped {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&backup);
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            privileged_swap(&fresh, bundle).map_err(InstallError::Failed)?;
        }
        Err(err) => {
            return Err(InstallError::Failed(format!(
                "could not replace {}: {err}",
                bundle.display()
            )))
        }
    }
    let _ = std::fs::remove_dir_all(&stage);
    let _ = std::fs::remove_file(&staged.archive);
    Ok(())
}

/// Unpack a gzipped tar held in memory into `into`, through tar's standard
/// input.
fn unpack(archive: &[u8], into: &Path) -> Result<(), String> {
    use std::io::Write as _;
    let mut tar = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg("-")
        .arg("-C")
        .arg(into)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;
    // Dropping the pipe once written is tar's end of input. A tar that
    // gives up early stops reading, which its exit status reports.
    let fed = tar.stdin.take().map(|mut stdin| stdin.write_all(archive));
    let status = tar.wait().map_err(|err| err.to_string())?;
    match (status.success(), fed) {
        (true, Some(Ok(()))) => Ok(()),
        _ => Err("the update archive could not be unpacked".into()),
    }
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
        assert_eq!(is_newer("0.4.2", "0.4.1"), Some(true));
        assert_eq!(is_newer("v0.5.0", "0.4.9"), Some(true));
        assert_eq!(is_newer("1.0.0", "0.99.99"), Some(true));
        assert_eq!(is_newer("0.4.1", "0.4.1"), Some(false));
        assert_eq!(is_newer("0.4.0", "0.4.1"), Some(false));
        // 0.10 is newer than 0.9: numeric, not lexical.
        assert_eq!(is_newer("0.10.0", "0.9.0"), Some(true));
    }

    #[test]
    fn prereleases_and_build_metadata_order_as_semantic_versioning_does() {
        // Prerelease to prerelease: identifiers compare in turn, numbers
        // as numbers, and a longer run wins a tie.
        assert_eq!(is_newer("1.2.3-rc.2", "1.2.3-rc.1"), Some(true));
        assert_eq!(is_newer("1.2.3-rc.10", "1.2.3-rc.9"), Some(true));
        assert_eq!(is_newer("1.2.3-beta", "1.2.3-alpha"), Some(true));
        assert_eq!(is_newer("1.2.3-alpha.1", "1.2.3-alpha"), Some(true));
        assert_eq!(is_newer("1.2.3-alpha.beta", "1.2.3-alpha.1"), Some(true));
        assert_eq!(is_newer("1.2.3-rc.1", "1.2.3-rc.2"), Some(false));
        // Prerelease to its release, and to a later one.
        assert_eq!(is_newer("1.2.3", "1.2.3-rc.1"), Some(true));
        assert_eq!(is_newer("1.2.4-rc.1", "1.2.3-rc.1"), Some(true));
        // A release never goes back to its own prerelease.
        assert_eq!(is_newer("1.2.3-rc.1", "1.2.3"), Some(false));
        assert_eq!(is_newer("1.2.4-rc.1", "1.2.3"), Some(true));
        // Build metadata never orders.
        assert_eq!(is_newer("1.2.3+build.7", "1.2.3"), Some(false));
        assert_eq!(is_newer("1.2.3", "1.2.3+build.7"), Some(false));
        assert_eq!(is_newer("1.2.3-rc.1+b", "1.2.3-rc.1+a"), Some(false));
        assert_eq!(is_newer("1.2.4+build.0007", "1.2.3"), Some(true));
    }

    #[test]
    fn anything_else_is_not_a_version() {
        for text in [
            "",
            "nonsense",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.3-",
            "1.2.3-rc..1",
            "1.2.3-rc.01",
            "1.2.3-rc_1",
            "1.2.3+",
            "1.2.3+b..1",
            " 1.2.3",
            "1.2.3 ",
            "1.2.-3",
            "18446744073709551616.0.0",
        ] {
            assert_eq!(is_newer(text, "0.0.1"), None, "{text:?}");
        }
        assert_eq!(is_newer("1.2.3-0.rc-1.x-y-z", "1.2.2"), Some(true));
    }

    fn manifest(version: &str) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "version": version,
            "platforms": {
                platform_key(): {"signature": "c2ln", "url": "https://example.invalid/a.tar.gz"}
            }
        }))
        .unwrap()
    }

    #[test]
    fn a_feed_naming_no_version_is_broken_not_current() {
        let offered = release_in(manifest("0.5.0-rc.2"), "0.5.0-rc.1").unwrap();
        assert_eq!(
            offered.map(|release| release.version).as_deref(),
            Some("0.5.0-rc.2")
        );
        assert_eq!(release_in(manifest("0.5.0-rc.1"), "0.5.0"), Ok(None));
        assert_eq!(release_in(manifest("0.5.0+build.2"), "0.5.0"), Ok(None));
        let broken = release_in(manifest("0.5.0.1"), "0.5.0").unwrap_err();
        assert!(broken.contains("0.5.0.1"), "{broken}");
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

    /// A signature as the release workflow's signer writes it, made with a
    /// throwaway key: the encoding the feed carries is one this build reads.
    #[test]
    fn a_release_signature_verifies_against_its_key_alone() {
        const KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEFBRTZGOUE1NzM0MENFM0EKUldRNnprQnpwZm5tcWdScWtSdkRZTC9oSGxzaUpXcE41Unh1S0RtRTU3cUhnaSs0SHA2dXVHVWEK";
        const SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRNnprQnpwZm5tcWludklCMWZCNHBPdXk3bzhIL0oyR1gxMVI5ODdMODJHRSt5VENsZTV6dVpSVjRWL1JpWUprdFk4VFR4QVIwNWc4bW54WlE1Mm02OXlodzF6QnNDL2dzPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk1MDM1ODUzCWZpbGU6cGF5bG9hZC5iaW4KNnJiY3F3dExGZTRzeCtmREwyYVhPV1ZSN1ZyNE5Md3RPNHVDYTl5ZnlBN0hjNjkweitwbkcza2hMSy9MLzJiS2ttRitJeWIvU0NsQ0VkSUpOd2M4Q1E9PQo=";
        let payload = b"jiji update signature test payload\n";
        assert_eq!(verify_with(KEY, payload, SIGNATURE), Ok(()));
        assert!(verify_with(KEY, b"another payload", SIGNATURE).is_err());
        // The release key did not make it.
        assert!(verify(payload, SIGNATURE).is_err());
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

    /// A throwaway key, and an `.app.tar.gz` holding
    /// `Jiji.app/Contents/Info.plist` signed with it the way the release
    /// workflow signs; and a second archive of the same shape that nothing
    /// signed — what a changed staged file would hold.
    const TEST_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEM4NDZCQkMzMjEzNjFGRjUKUldUMUh6WWh3N3RHeUhMSnUvUlZCVWNWV25EUjB1N0N4MHhoM2gwQ2hCTE5hdzVjSUhscUc0S1AK";
    const TEST_SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUMUh6WWh3N3RHeUtTTGwzWW5helBEc2FMdHpoL0xDeXQveTlRaC9SK0Q2aURXZm9RZTJ5ZDJDb29ibXlmQ0FEeHRSbE9nMGJzUzNuMHFSVWpyQzNVZFFtY25nOFQzTlFNPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk1MDM1ODUzCWZpbGU6SmlqaS5hcHAudGFyLmd6CmliM1EwVEpGS0NIRi80VmxsRzc1ZDdZbGVNdysyMWZQNE10MzVlQ3BDc3VwWm1CbGlFSy9HY1Z5VnBVa1pyQk1xSzl4TzJKUXlhclN4VXM3aENxWEF3PT0K";
    const SIGNED_ARCHIVE: &str = "H4sIAAAAAAAC/+3RMQ6CMBiG4c6eoicAKi0cwElu0cQqNaQQWuL1JSYuRDdREt9n+f7tH97GX31mhyEX6ylmtTGPnS33xV1rpYQ04gummOw4vxT/qXn2P/QhuZBivoH+Su2Liv6/6X8M5z4bOh/TJ/tXWr/vX+pF/1KZUsiC/qtLrZOj65yNTqbWJnmzUUZ/Ce60EwAAAAAAAAAAAAAAANi8O+AjFs4AKAAA";
    const OTHER_ARCHIVE: &str = "H4sIAAAAAAAC/+3RTQ7CIBBAYdaeghO0gNAewJW9BYlVMYY2gvH6EhM3je6sP/F9i5nZzeJ14RAqP461mI8qWuduu5juB3drtRbSiTc4p+xP5aX4T929/2qIuY851V/QX2ujGvp/pv86bodqPIaUX9m/sfZ5f2Mn/ZfaGSEV/WcXhywve1+GTzKFXew3CwEAAAAAAAAAAAAAAICfcQXNXVTZACgAAA==";

    /// An installed `Jiji.app` and a staged archive beside it, as a
    /// download leaves them.
    fn staged_beside_installed(name: &str, archive: &str) -> (PathBuf, PathBuf, Staged) {
        let dir = std::env::temp_dir().join(format!("jiji-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = dir.join("Applications/Jiji.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            "the installed release\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("updates")).unwrap();
        let file = dir.join("updates/Jiji-9.9.9.app.tar.gz");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(archive)
            .unwrap();
        std::fs::write(&file, bytes).unwrap();
        let staged = Staged {
            version: "9.9.9".into(),
            archive: file,
            signature: TEST_SIGNATURE.into(),
        };
        (dir, bundle, staged)
    }

    fn installed(bundle: &Path) -> String {
        std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap()
    }

    #[test]
    fn an_untouched_staged_update_installs() {
        let (dir, bundle, staged) = staged_beside_installed("untouched", SIGNED_ARCHIVE);
        assert_eq!(install(TEST_KEY, &staged, &bundle), Ok(()));
        assert_eq!(installed(&bundle), "the release that was signed\n");
        // Nothing is left behind: the archive, its unpacked copy, the
        // previous bundle.
        assert!(!staged.archive.exists());
        assert_eq!(
            std::fs::read_dir(dir.join("Applications")).unwrap().count(),
            1
        );
        assert_eq!(std::fs::read_dir(dir.join("updates")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_staged_update_changed_after_verifying_never_installs() {
        // Downloaded and verified, then another archive took its place.
        let (dir, bundle, staged) = staged_beside_installed("changed", OTHER_ARCHIVE);
        let refused = install(TEST_KEY, &staged, &bundle);
        assert_eq!(refused, Err(InstallError::Unverified));
        assert_eq!(installed(&bundle), "the installed release\n");
        // It is not offered again: the next check downloads afresh.
        assert!(!staged.archive.exists());
        let why = refused.unwrap_err().to_string();
        assert!(
            why.contains("changed or removed after it was verified"),
            "{why}"
        );
        assert!(why.contains("installed Jiji is unchanged"), "{why}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
