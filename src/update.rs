//! Self-update against GitHub releases.
//!
//! Release assets are named by target triple (`csb-x86_64-pc-windows-msvc.zip`),
//! which is what `self_update` matches on by default, so no `.target()` override
//! is needed here - the release workflow and this module agree by construction.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const REPO_OWNER: &str = "toperux";
pub const REPO_NAME: &str = "t4-claude-session-browser";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// How long a background check result is reused before hitting the network again.
const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// A release newer than the running binary.
pub struct Available {
    pub version: String,
}

/// True when the user has opted out of update checks entirely.
pub fn checks_disabled() -> bool {
    std::env::var_os("CSB_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Binaries that a release ships and an update therefore has to replace.
/// Windows carries a second, GUI-subsystem executable for the Start Menu
/// shortcut; every other platform ships one binary.
const BINARIES: &[&str] = if cfg!(windows) {
    &["csb", "csb-gui"]
} else {
    &["csb"]
};

/// Archive format the release workflow uses for this platform.
const ARCHIVE_EXT: &str = if cfg!(windows) { ".zip" } else { ".tar.gz" };

/// `stem` is the binary's name without any platform suffix. `install_path` aims
/// the replacement at one specific file rather than at whatever happens to be
/// running - with two binaries, "the current exe" is the wrong target for one of
/// them.
fn updater(
    stem: &str,
    install_path: Option<&std::path::Path>,
    progress: bool,
    force: bool,
) -> Result<self_update::backends::github::Update> {
    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        // Looked up *inside* the archive, so it needs the platform suffix:
        // plain "csb" would never match "csb.exe".
        .bin_name(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
        // Asset matching is substring-based and takes the first hit, so name
        // the archive type too: the installer (`csb-setup-*.exe`) must never be
        // picked, whatever GitHub's asset order turns out to be.
        .asset_identifier(ARCHIVE_EXT)
        // Every pass compares against the *running* binary's version, not the
        // version of the file it is about to overwrite - there is no cheap way
        // to read the latter. So after a half-applied update (see `install`),
        // the stale sibling looks current and nothing would repair it. Forcing
        // a floor version makes every pass install unconditionally, which is
        // what `csb update --force` is for.
        .current_version(if force { "0.0.0" } else { CURRENT })
        .show_download_progress(progress)
        .show_output(progress)
        // Without this `update()` blocks on a stdin prompt, which would hang the GUI.
        .no_confirm(true);
    if let Some(path) = install_path {
        builder.bin_install_path(path);
    }
    Ok(builder.build()?)
}

/// Ask GitHub whether a newer release exists. Blocking - never call from the UI thread.
pub fn check() -> Result<Option<Available>> {
    let found = updater("csb", None, false, false)?.is_update_available()?;
    Ok(found.map(|r| Available {
        version: r.version().to_string(),
    }))
}

/// What to tell the user instead of updating, when this binary was installed
/// by the .deb/.rpm and therefore belongs to the package manager.
pub const PACKAGE_MANAGED_HINT: &str =
    "csb was installed by a system package; upgrade it with your package manager \
     (re-run installer/install.sh, or install the new .deb/.rpm from the release)";

/// True when the running binary lives in `/usr/bin`, which is where the
/// .deb/.rpm put it and where nothing else installs (the tarball route uses
/// `~/.local/bin`, `/usr/local/bin`, or wherever the user unpacked it). An
/// in-place swap there would work under sudo, but it leaves dpkg/rpm believing
/// they own a file they no longer wrote - and the next package upgrade would
/// quietly put the old binary back.
pub fn package_managed() -> bool {
    cfg!(unix)
        && std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d == std::path::Path::new("/usr/bin")))
            .unwrap_or(false)
}

/// Download the release and replace every binary this platform ships.
///
/// `self_update` stages its temp files in the target's own directory, so this
/// fails when the binaries live somewhere unwritable (`C:\Program Files`,
/// `/usr/local/bin` without sudo). The error says so; it is not worth retrying.
///
/// On Windows both `csb.exe` and `csb-gui.exe` are replaced. Updating only one
/// would leave the other reporting the old version - and since the GUI is what
/// shows the update banner, a stale `csb-gui.exe` would offer the same update
/// forever.
pub fn install(progress: bool, force: bool) -> Result<self_update::VersionStatus> {
    if package_managed() {
        anyhow::bail!("{PACKAGE_MANAGED_HINT}");
    }
    let exe = std::env::current_exe().context("cannot locate the running executable")?;
    let dir = exe
        .parent()
        .context("running executable has no parent directory")?;

    let mut status = None;
    for (i, stem) in BINARIES.iter().enumerate() {
        let path = dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
        let outcome = updater(stem, Some(&path), progress, force).and_then(|u| Ok(u.update()?));

        match outcome {
            Ok(done) => status.get_or_insert(done),
            Err(e) => {
                // `self_update` self-replaces the *running* binary but plainly
                // moves over any other, and Windows refuses to overwrite an exe
                // that some other process still has open. Name the file and the
                // fix rather than surfacing a bare permission error.
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let hint = if cfg!(windows) {
                    format!(
                        "{name} could not be replaced - close it, then run `csb update --force`"
                    )
                } else {
                    format!("{name} could not be replaced")
                };
                return Err(match i {
                    // A later binary failing means the earlier ones already
                    // landed: say so, or "failed" reads as "nothing changed".
                    n if n > 0 => e.context(format!(
                        "{hint}. The update is half-applied: {} already updated",
                        BINARIES[..i].join(", ")
                    )),
                    _ => e.context(hint),
                });
            }
        };
    }

    status.context("no binaries were configured to update")
}

/// `check()` behind a 24h throttle and the `CSB_NO_UPDATE_CHECK` opt-out.
/// This is the one the GUI calls on startup.
pub fn check_throttled() -> Result<Option<Available>> {
    if checks_disabled() {
        return Ok(None);
    }
    let now = chrono::Utc::now().timestamp();
    let cache = CheckCache::load();

    if now - cache.last_check_secs < CHECK_INTERVAL_SECS {
        // Inside the window: replay the last result instead of asking again. Re-test
        // it against the current version, since we may have installed it since.
        return Ok(cache
            .last_seen
            .filter(|v| is_newer(v))
            .map(|version| Available { version }));
    }

    check_now()
}

/// `check()` ignoring the throttle, but still recording the result so the
/// 24h window restarts here and later launches replay this answer. This is
/// what the GUI's "Check for updates" button calls; it does not consult
/// `CSB_NO_UPDATE_CHECK` - an explicit click is the caller's decision.
pub fn check_now() -> Result<Option<Available>> {
    // The throttle advances on failure too: "once a day" means once a day,
    // not once per launch while offline.
    let found = check();
    CheckCache {
        last_check_secs: chrono::Utc::now().timestamp(),
        last_seen: found
            .as_ref()
            .ok()
            .and_then(|f| f.as_ref().map(|a| a.version.clone())),
    }
    .store();
    found
}

fn is_newer(version: &str) -> bool {
    self_update::version::bump_is_greater(CURRENT, version).unwrap_or(false)
}

/// Throttle state, kept beside the session index cache.
#[derive(Default, Serialize, Deserialize)]
struct CheckCache {
    #[serde(default)]
    last_check_secs: i64,
    #[serde(default)]
    last_seen: Option<String>,
}

impl CheckCache {
    fn file() -> Option<PathBuf> {
        Some(
            dirs::cache_dir()?
                .join("claude-session-browser")
                .join("update.json"),
        )
    }

    /// Anything unreadable or unparseable counts as "never checked" - a corrupt
    /// throttle file must not be able to suppress updates forever.
    fn load() -> Self {
        let Some(path) = Self::file() else {
            return Self::default();
        };
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn store(&self) {
        let Some(path) = Self::file() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_throttle_state_reads_as_never_checked() {
        let cache: CheckCache = serde_json::from_slice(b"{ not json").unwrap_or_default();
        assert_eq!(cache.last_check_secs, 0);
        assert!(cache.last_seen.is_none());
    }

    #[test]
    fn partial_throttle_state_still_parses() {
        let cache: CheckCache = serde_json::from_slice(br#"{"last_check_secs": 42}"#).unwrap();
        assert_eq!(cache.last_check_secs, 42);
        assert!(cache.last_seen.is_none());
    }

    #[test]
    fn round_trips() {
        let json = serde_json::to_vec(&CheckCache {
            last_check_secs: 99,
            last_seen: Some("9.9.9".into()),
        })
        .unwrap();
        let back: CheckCache = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.last_check_secs, 99);
        assert_eq!(back.last_seen.as_deref(), Some("9.9.9"));
    }

    #[test]
    fn only_strictly_newer_versions_replay() {
        assert!(is_newer("999.0.0"));
        assert!(!is_newer(CURRENT));
        assert!(!is_newer("0.0.1"));
        // An unparseable tag must not be treated as an update.
        assert!(!is_newer("nightly"));
    }
}
