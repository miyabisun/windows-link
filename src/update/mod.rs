//! Automatic updates from GitHub Releases: on startup and every hour the server looks at
//! the latest release, and when it is newer, downloads it, checks its SHA-256 and the
//! version it reports, swaps it in place of the running exe and restarts without asking.
//!
//! Windows cannot delete or overwrite a running exe but can rename it, so the running
//! file is renamed to `*.old`, the new one takes its name, and the new process deletes
//! `*.old` once it is listening (see [`swap`]).

pub mod api;
pub mod github;
pub mod swap;

use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use sha2::{Digest, Sha256};
use tracing::{error, info, warn};

/// Release asset for this build, and the `sha256sum`-style file next to it.
pub const ASSET: &str = "windows-link-x86_64-pc-windows-msvc.exe";
pub const CHECKSUM_ASSET: &str = "windows-link-x86_64-pc-windows-msvc.exe.sha256";

/// The public repository's web address; `WINDOWS_LINK_UPDATE_URL` overrides it. Its
/// `/releases/latest` sends to the latest release (`/releases/tag/<tag>`), whose files lie
/// at `/releases/download/<tag>/<name>`. GitHub's API is not used: without a token it
/// allows a home 60 calls an hour, shared by every machine there, and once they were
/// spent the checks failed (2026-10-26).
pub const DEFAULT_FEED: &str = "https://github.com/miyabisun/windows-link";

pub const CHECK_INTERVAL: Duration = Duration::from_hours(1);

/// Size limits for what the feed sends; the exe is about 5 MB.
pub const CHECKSUM_LIMIT: u64 = 1 << 10;
pub const EXE_LIMIT: u64 = 64 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// Strict `MAJOR.MINOR.PATCH` (no pre-release, no leading zeros).
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let mut next = || {
            let part = parts.next()?;
            let valid = !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            valid.then(|| part.parse().ok()).flatten()
        };
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    /// A release tag: `v` followed by a strict version.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::parse(tag.strip_prefix('v')?)
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is MAJOR.MINOR.PATCH")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub exe_url: String,
    pub checksum_url: String,
}

/// The release `/releases/latest` of the repository at `feed` sent to (`location`,
/// `.../releases/tag/<tag>`), with where its exe and checksum file are.
pub fn release_from_location(feed: &str, location: &str) -> Result<Release, String> {
    let (_, tag) = location
        .rsplit_once("/releases/tag/")
        .ok_or("no release is published")?;
    let version = Version::from_tag(tag).ok_or_else(|| format!("unexpected release tag {tag}"))?;
    let files = format!("{}/releases/download/{tag}", feed.trim_end_matches('/'));
    Ok(Release {
        version,
        exe_url: format!("{files}/{ASSET}"),
        checksum_url: format!("{files}/{CHECKSUM_ASSET}"),
    })
}

/// The digest in a `sha256sum`-style file, which must name exactly `asset`:
/// one line, `<64 hex digits>  <file name>` (an optional `*` marks binary mode).
pub fn expected_digest(checksum_file: &str, asset: &str) -> Result<[u8; 32], String> {
    let mut lines = checksum_file.lines().filter(|l| !l.trim().is_empty());
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return Err("checksum file must have exactly one line".into());
    };
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [hex, name] = fields.as_slice() else {
        return Err("checksum line must be `<sha256>  <file name>`".into());
    };
    if name.strip_prefix('*').unwrap_or(name) != asset {
        return Err(format!("checksum file is for {name}, not {asset}"));
    }
    if hex.len() != 64 {
        return Err("checksum is not a SHA-256".into());
    }
    let mut digest = [0u8; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "checksum is not hexadecimal".to_owned())?;
    }
    Ok(digest)
}

pub fn matches_digest(bytes: &[u8], digest: &[u8; 32]) -> bool {
    Sha256::digest(bytes).as_slice() == digest
}

/// Where the installed exe lives: `%LOCALAPPDATA%\Programs\windows-link`.
pub fn install_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(Path::new(&base).join("Programs").join("windows-link"))
}

/// Only a release build started from the install directory updates itself; a
/// development build or a copy run from elsewhere never replaces files.
pub fn eligibility(
    release_build: bool,
    exe: Option<&Path>,
    install_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    if !release_build {
        return Err("development build".into());
    }
    let exe = exe.ok_or("cannot locate the running exe")?;
    let dir = install_dir.ok_or("LOCALAPPDATA is not set")?;
    let same_dir = exe.parent().is_some_and(|parent| {
        parent
            .to_string_lossy()
            .eq_ignore_ascii_case(&dir.to_string_lossy())
    });
    if same_dir {
        Ok(exe.to_path_buf())
    } else {
        Err(format!("not started from {}", dir.display()))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate {
        latest: Version,
    },
    /// The new exe is in place; call [`Updater::restart`].
    Restarting {
        latest: Version,
    },
    Skipped {
        reason: String,
    },
}

pub struct Updater {
    current: Version,
    feed: String,
    /// The installed exe, or why this process does not update itself.
    exe: Result<PathBuf, String>,
    busy: Mutex<()>,
}

impl Updater {
    pub fn from_env() -> Self {
        let exe = std::env::current_exe().ok();
        Self {
            current: Version::current(),
            feed: std::env::var("WINDOWS_LINK_UPDATE_URL").unwrap_or_else(|_| DEFAULT_FEED.into()),
            exe: eligibility(
                !cfg!(debug_assertions),
                exe.as_deref(),
                install_dir().as_deref(),
            ),
            busy: Mutex::new(()),
        }
    }

    pub fn current(&self) -> Version {
        self.current
    }

    /// The installed exe when this process updates itself.
    pub fn installed_exe(&self) -> Option<&Path> {
        self.exe.as_deref().ok()
    }

    /// Look at the feed and, when it has a newer version, put it in place of the
    /// running exe. Blocking; any failure leaves the running version as it is.
    pub fn check(&self) -> Result<Outcome, String> {
        let exe = match &self.exe {
            Ok(exe) => exe,
            Err(reason) => {
                return Ok(Outcome::Skipped {
                    reason: reason.clone(),
                });
            }
        };
        let Ok(_guard) = self.busy.try_lock() else {
            return Ok(Outcome::Skipped {
                reason: "another check is running".into(),
            });
        };
        let latest = format!("{}/releases/latest", self.feed.trim_end_matches('/'));
        let release = release_from_location(&self.feed, &github::fetch_location(&latest)?)?;
        if release.version.cmp(&self.current) != Ordering::Greater {
            return Ok(Outcome::UpToDate {
                latest: release.version,
            });
        }
        let bytes = github::fetch_bytes(&release.exe_url, EXE_LIMIT)?;
        let checksum = github::fetch_bytes(&release.checksum_url, CHECKSUM_LIMIT)?;
        let digest = expected_digest(&String::from_utf8_lossy(&checksum), ASSET)?;
        if !matches_digest(&bytes, &digest) {
            return Err(format!(
                "SHA-256 of {ASSET} {} does not match its checksum file",
                release.version
            ));
        }
        let staged = swap::stage(exe, &bytes, &release.version.to_string())?;
        swap::replace(exe, &staged)?;
        Ok(Outcome::Restarting {
            latest: release.version,
        })
    }

    /// Start the new exe and exit. If it cannot be started, put the old exe back and
    /// keep running.
    pub fn restart(&self) {
        let Some(exe) = self.installed_exe() else {
            return;
        };
        match swap::relaunch(exe) {
            Ok(()) => {
                info!("restarting into the new version");
                std::process::exit(0);
            }
            Err(err) => {
                error!(%err, "cannot start the new version; keeping this one");
                if let Err(err) = swap::rollback(exe) {
                    error!(%err, "cannot put the previous exe back");
                }
            }
        }
    }
}

/// Check on startup and every hour; restart when a newer version was installed.
pub async fn run_periodically(updater: Arc<Updater>) {
    if let Err(reason) = &updater.exe {
        info!(%reason, "automatic updates are off");
        return;
    }
    loop {
        let worker = updater.clone();
        match tokio::task::spawn_blocking(move || worker.check()).await {
            Ok(Ok(Outcome::Restarting { latest })) => {
                info!(%latest, "installed a new version");
                updater.restart();
            }
            Ok(Ok(Outcome::UpToDate { latest })) => {
                info!(current = %updater.current, %latest, "up to date");
            }
            Ok(Ok(Outcome::Skipped { reason })) => info!(%reason, "update check skipped"),
            Ok(Err(err)) => warn!(%err, "update check failed; will try again"),
            Err(err) => warn!(%err, "update check stopped unexpectedly"),
        }
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sha2::{Digest, Sha256};

    use super::{
        ASSET, CHECKSUM_ASSET, Version, eligibility, expected_digest, matches_digest,
        release_from_location,
    };

    #[test]
    fn versions_are_strict_and_ordered_numerically() {
        assert_eq!(Version::parse("0.1.10"), Some(Version(0, 1, 10)));
        assert!(Version::parse("0.1.10") > Version::parse("0.1.9"));
        assert!(Version::parse("1.0.0") > Version::parse("0.99.99"));
        assert_eq!(Version::from_tag("v2.3.4"), Some(Version(2, 3, 4)));
        for bad in [
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-rc.1",
            "1..3",
            " 1.2.3",
            "a.b.c",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version::from_tag("2.3.4"), None);
        assert_eq!(Version::from_tag("v2.3"), None);
        assert_eq!(Version(1, 20, 3).to_string(), "1.20.3");
    }

    const FEED: &str = "https://github.com/miyabisun/windows-link";

    #[test]
    fn the_latest_release_is_where_its_page_sends_to() {
        let picked = release_from_location(FEED, &format!("{FEED}/releases/tag/v0.2.0")).unwrap();
        assert_eq!(picked.version, Version(0, 2, 0));
        let files = format!("{FEED}/releases/download/v0.2.0");
        assert_eq!(picked.exe_url, format!("{files}/{ASSET}"));
        assert_eq!(picked.checksum_url, format!("{files}/{CHECKSUM_ASSET}"));
        // A test server on this machine may send a path alone.
        let local =
            release_from_location("http://127.0.0.1:4799/", "/releases/tag/v0.3.1").unwrap();
        assert_eq!(
            local.exe_url,
            format!("http://127.0.0.1:4799/releases/download/v0.3.1/{ASSET}")
        );
    }

    #[test]
    fn no_release_or_an_odd_tag_is_no_update() {
        assert_eq!(
            release_from_location(FEED, &format!("{FEED}/releases")),
            Err("no release is published".to_owned())
        );
        for odd in ["nightly", "v0.2", "v0.2.0-rc.1", "v0.2.0/x", ""] {
            assert!(
                release_from_location(FEED, &format!("{FEED}/releases/tag/{odd}")).is_err(),
                "{odd}"
            );
        }
    }

    #[test]
    fn checksum_file_must_name_the_asset_and_hold_one_sha256() {
        let bytes = b"new exe";
        let hex = Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |hex, b| hex + &format!("{b:02x}"));

        let digest = expected_digest(&format!("{hex}  {ASSET}\n"), ASSET).unwrap();
        assert!(matches_digest(bytes, &digest));
        assert!(!matches_digest(b"tampered exe", &digest));

        let upper = expected_digest(&format!("{}  *{ASSET}", hex.to_uppercase()), ASSET);
        assert_eq!(upper.unwrap(), digest);

        assert!(expected_digest(&format!("{hex}  other.exe"), ASSET).is_err());
        assert!(expected_digest(&hex, ASSET).is_err());
        assert!(expected_digest(&format!("{hex}  {ASSET}\n{hex}  {ASSET}"), ASSET).is_err());
        assert!(expected_digest(&format!("{}  {ASSET}", &hex[..62]), ASSET).is_err());
        assert!(expected_digest(&format!("{}zz  {ASSET}", &hex[..62]), ASSET).is_err());
    }

    #[test]
    fn only_a_release_build_in_the_install_directory_updates() {
        let dir = Path::new(r"C:\Users\me\AppData\Local\Programs\windows-link");
        let installed = dir.join("windows-link.exe");
        assert_eq!(
            eligibility(true, Some(&installed), Some(dir)).unwrap(),
            installed
        );
        let other_case =
            Path::new(r"c:\users\me\appdata\local\programs\WINDOWS-LINK\windows-link.exe");
        assert!(eligibility(true, Some(other_case), Some(dir)).is_ok());

        assert!(eligibility(false, Some(&installed), Some(dir)).is_err());
        let elsewhere = Path::new(r"C:\src\windows-link\target\release\windows-link.exe");
        assert!(eligibility(true, Some(elsewhere), Some(dir)).is_err());
        assert!(eligibility(true, Some(&installed), None).is_err());
        assert!(eligibility(true, None, Some(dir)).is_err());
    }
}
