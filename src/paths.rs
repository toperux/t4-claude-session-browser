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

/// Running inside Windows Subsystem for Linux. The kernel release names
/// Microsoft in every WSL1/WSL2 build ("5.15.167.4-microsoft-standard-WSL2"),
/// unlike the binfmt entry or WSL_DISTRO_NAME, which interop settings, sudo
/// and systemd services can strip.
pub fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|r| r.to_ascii_lowercase().contains("microsoft"))
}

/// A path on a Windows drive mounted into WSL. Deleting there goes through
/// drvfs, where `trash` cannot reach the Windows Recycle Bin. Decided from the
/// mount table rather than a `/mnt/<letter>` prefix, since `[automount] root`
/// and manual `mount -t drvfs` put such mounts anywhere.
pub fn is_wsl_drvfs(path: &Path) -> bool {
    std::fs::read_to_string("/proc/mounts").is_ok_and(|m| on_drvfs_mount(path, &m))
}

/// `mounts` is `/proc/mounts`: "<dev> <mountpoint> <fstype> <opts> ...". WSL
/// mounts Windows drives as `drvfs` (WSL1) or `9p` with `aname=drvfs` (WSL2).
/// The longest mount point that prefixes `path` is the one that holds it.
fn on_drvfs_mount(path: &Path, mounts: &str) -> bool {
    mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_dev, point, fstype, opts) = (f.next()?, f.next()?, f.next()?, f.next()?);
            // /proc/mounts escapes spaces in mount points as \040.
            let point = PathBuf::from(point.replace("\\040", " "));
            path.starts_with(&point).then_some((point, fstype, opts))
        })
        .max_by_key(|(point, _, _)| point.as_os_str().len())
        .is_some_and(|(_, fstype, opts)| {
            fstype == "drvfs" || (fstype == "9p" && opts.contains("aname=drvfs"))
        })
}

#[cfg(test)]
mod wsl_tests {
    use super::*;

    const MOUNTS: &str = "\
/dev/sdc / ext4 rw,relatime,discard,errors=remount-ro,data=ordered 0 0
none /mnt/wslg tmpfs rw,relatime 0 0
C:\\134 /mnt/c 9p rw,noatime,dirsync,aname=drvfs;path=C:\\;uid=1000;gid=1000;symlinkroot=/mnt/,mmap,access=client,msize=65536,trans=fd,rfd=6,wfd=6 0 0
D:\\134 /mnt/data drvfs rw,relatime 0 0
none /mnt/c/Users/x/tmp\\040dir tmpfs rw 0 0
";

    #[test]
    fn drvfs_is_decided_by_the_longest_matching_mount() {
        assert!(on_drvfs_mount(Path::new("/mnt/c/Users/x/.claude"), MOUNTS));
        assert!(on_drvfs_mount(Path::new("/mnt/data/.claude"), MOUNTS));
        assert!(!on_drvfs_mount(Path::new("/mnt/wslg/x"), MOUNTS));
        assert!(!on_drvfs_mount(Path::new("/home/x/.claude"), MOUNTS));
        // A non-drvfs mount nested inside a drvfs one wins for its subtree.
        assert!(!on_drvfs_mount(
            Path::new("/mnt/c/Users/x/tmp dir/f"),
            MOUNTS
        ));
        assert!(!on_drvfs_mount(Path::new("/mnt/c"), ""));
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
