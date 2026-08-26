//! Self-update against GitHub releases.
//!
//! Release assets are named by target triple (`csb-x86_64-pc-windows-msvc.zip`),
//! which is what `self_update` matches on by default, so no `.target()` override
//! is needed here - the release workflow and this module agree by construction.

use anyhow::Result;
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
    pub body: String,
}

/// True when the user has opted out of update checks entirely.
pub fn checks_disabled() -> bool {
    std::env::var_os("CSB_NO_UPDATE_CHECK").is_some_and(|v| !v.is_empty() && v != "0")
}

fn updater(progress: bool) -> Result<self_update::backends::github::Update> {
    let update = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        // Looked up *inside* the archive, so it needs the platform suffix:
        // plain "csb" would never match "csb.exe".
        .bin_name(format!("csb{}", std::env::consts::EXE_SUFFIX))
        .current_version(CURRENT)
        .show_download_progress(progress)
        .show_output(progress)
        // Without this `update()` blocks on a stdin prompt, which would hang the GUI.
        .no_confirm(true)
        .build()?;
    Ok(update)
}

/// Ask GitHub whether a newer release exists. Blocking - never call from the UI thread.
pub fn check() -> Result<Option<Available>> {
    let found = updater(false)?.is_update_available()?;
    Ok(found.map(|r| Available {
        version: r.version().to_string(),
        body: r.body().unwrap_or_default().to_string(),
    }))
}

/// Download the asset for this target and replace the running executable.
///
/// `self_update` stages its temp files in the running executable's own directory,
/// so this fails when the binary lives somewhere unwritable (`C:\Program Files`,
/// `/usr/local/bin` without sudo). The error says so; it is not worth retrying.
pub fn install(progress: bool) -> Result<self_update::VersionStatus> {
    Ok(updater(progress)?.update()?)
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
            .map(|version| Available {
                version,
                body: String::new(),
            }));
    }

    let found = check()?;
    CheckCache {
        last_check_secs: now,
        last_seen: found.as_ref().map(|a| a.version.clone()),
    }
    .store();
    Ok(found)
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
