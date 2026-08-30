use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Resolved layout of a `~/.claude` directory.
#[derive(Debug, Clone)]
pub struct ClaudeDir {
    pub root: PathBuf,
}

impl ClaudeDir {
    /// `--claude-dir` flag wins, then `CLAUDE_CONFIG_DIR`, then `~/.claude`.
    pub fn resolve(flag: Option<&Path>) -> Result<Self> {
        let root = if let Some(p) = flag {
            p.to_path_buf()
        } else if let Some(env) = std::env::var_os("CLAUDE_CONFIG_DIR") {
            PathBuf::from(env)
        } else {
            dirs::home_dir()
                .context("cannot determine home directory")?
                .join(".claude")
        };

        let root =
            canon(&root).with_context(|| format!("claude dir not found: {}", root.display()))?;

        if !root.join("projects").is_dir() {
            bail!("{} has no projects/ subdirectory", root.display());
        }
        Ok(Self { root })
    }

    pub fn projects(&self) -> PathBuf {
        self.root.join("projects")
    }

    /// Every path that belongs to a session and should go away with it.
    /// Only paths that actually exist are returned.
    pub fn session_paths(&self, project_slug: &str, id: &str) -> Vec<PathBuf> {
        // `projects/<slug>/.` is the project dir and `projects/<slug>/..` is
        // all of them; both canonicalize to inside the root and would pass
        // `contains`. Refuse before building the paths at all.
        if !is_session_id(id) {
            return Vec::new();
        }
        let candidates = [
            self.projects()
                .join(project_slug)
                .join(format!("{id}.jsonl")),
            self.projects().join(project_slug).join(id),
            self.root.join("session-env").join(id),
            self.root.join("file-history").join(id),
        ];
        candidates.into_iter().filter(|p| p.exists()).collect()
    }

    /// Guard against deleting anything outside the claude dir.
    pub fn contains(&self, path: &Path) -> bool {
        // Resolve symlinks and `..` before comparing, so nothing can escape.
        let Ok(canonical) = canon(path) else {
            return false;
        };
        canonical.starts_with(&self.root) && canonical != self.root
    }
}

/// Canonicalize, then drop Windows' `\\?\` verbatim prefix so paths stay
/// readable. Both sides of a `contains` check must go through this.
fn canon(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    let s = canonical.to_string_lossy();
    Ok(match s.strip_prefix(r"\\?\") {
        Some(stripped) if !stripped.starts_with("UNC\\") => PathBuf::from(stripped),
        _ => canonical,
    })
}

/// A file stem that can safely be joined onto a directory as one component.
/// Ids come from file names, not validated UUIDs, so this is the only thing
/// standing between a stray `..jsonl` and a delete plan for a whole directory.
pub fn is_session_id(id: &str) -> bool {
    !id.is_empty() && id != "." && id != ".." && !id.contains(['/', '\\'])
}

/// Running inside Windows Subsystem for Linux. The binfmt entry is what WSL
/// registers to run .exe files, and it exists in every distro; the env var is
/// the fallback for a shell that was started without it.
pub fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
            || std::env::var_os("WSL_DISTRO_NAME").is_some())
}

/// A path on a Windows drive mounted into WSL (`/mnt/c/...`). Deleting there
/// goes through drvfs, where `trash` cannot reach the Windows Recycle Bin.
pub fn is_wsl_drvfs(path: &Path) -> bool {
    let mut parts = path.components().skip(1); // RootDir
    matches!(parts.next(), Some(std::path::Component::Normal(m)) if m == "mnt")
        && matches!(parts.next(), Some(std::path::Component::Normal(d))
            if d.len() == 1 && d.to_str().is_some_and(|d| d.chars().all(|c| c.is_ascii_alphabetic())))
}

#[cfg(test)]
mod wsl_tests {
    use super::*;

    #[test]
    fn drvfs_paths_are_windows_drive_mounts_only() {
        assert!(is_wsl_drvfs(Path::new("/mnt/c/Users/x/.claude")));
        assert!(is_wsl_drvfs(Path::new("/mnt/D/x")));
        assert!(!is_wsl_drvfs(Path::new("/mnt/wslg/x")));
        assert!(!is_wsl_drvfs(Path::new("/home/x/.claude")));
        assert!(!is_wsl_drvfs(Path::new("/mnt")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_paths_refuses_directory_walking_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude");
        std::fs::create_dir_all(root.join("projects").join("slug")).unwrap();
        let dir = ClaudeDir::resolve(Some(&root)).unwrap();

        for id in [".", "..", "", "a/b", "a\\b"] {
            assert!(dir.session_paths("slug", id).is_empty(), "{id:?}");
        }
    }

    #[test]
    fn contains_rejects_outside_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude");
        std::fs::create_dir_all(root.join("projects")).unwrap();
        let dir = ClaudeDir::resolve(Some(&root)).unwrap();

        let inside = root.join("projects").join("p");
        std::fs::create_dir_all(&inside).unwrap();
        assert!(dir.contains(&inside));

        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(!dir.contains(&outside));

        // Traversal out of the tree must be rejected.
        assert!(!dir.contains(
            &root
                .join("projects")
                .join("..")
                .join("..")
                .join("elsewhere")
        ));
        // The root itself is never a delete target.
        assert!(!dir.contains(&root));
    }

    #[test]
    fn session_paths_skips_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude");
        std::fs::create_dir_all(root.join("projects").join("slug")).unwrap();
        std::fs::write(root.join("projects").join("slug").join("abc.jsonl"), "").unwrap();
        std::fs::create_dir_all(root.join("session-env").join("abc")).unwrap();
        let dir = ClaudeDir::resolve(Some(&root)).unwrap();

        let paths = dir.session_paths("slug", "abc");
        assert_eq!(paths.len(), 2, "{paths:?}");
    }
}
