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

/// Display label for a project with no recorded `cwd`. Slugification maps every
/// separator to `-`, so it cannot be reversed - show the slug as-is rather than
/// inventing a path that looks real but isn't.
pub fn slug_hint(slug: &str) -> String {
    slug.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
