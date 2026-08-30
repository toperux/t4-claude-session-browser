use anyhow::{bail, Result};
use std::path::PathBuf;

use crate::index::SessionMeta;
use crate::paths::ClaudeDir;

/// Everything that will be moved to the recycle bin for one session.
#[derive(Debug, Clone)]
pub struct DeletePlan {
    pub id: String,
    pub title: String,
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    /// The session looks live - confirm harder before removing it.
    pub recent: bool,
}

impl DeletePlan {
    pub fn short_id(&self) -> &str {
        crate::index::short_id(&self.id)
    }
}

/// Resolve the transcript plus its sidecar dirs. Missing paths are skipped.
pub fn plan(dir: &ClaudeDir, meta: &SessionMeta) -> DeletePlan {
    let paths = dir.session_paths(&meta.project_slug, &meta.id);
    let bytes = paths.iter().map(|p| dir_size(p)).sum();
    DeletePlan {
        id: meta.id.clone(),
        title: meta.title.clone(),
        paths,
        bytes,
        recent: meta.is_recent(),
    }
}

/// Move every planned path to the OS recycle bin.
pub fn execute(dir: &ClaudeDir, plan: &DeletePlan) -> Result<()> {
    if plan.paths.is_empty() {
        bail!("nothing to delete for session {}", plan.id);
    }
    // Never hand trash a path that escaped the claude dir.
    for p in &plan.paths {
        if !dir.contains(p) {
            bail!(
                "refusing to delete {} - outside {}",
                p.display(),
                dir.root.display()
            );
        }
    }
    // On a Windows drive mounted into WSL, `trash` sees a foreign mount and
    // makes a `/mnt/c/.Trash-<uid>` of its own: files vanish from Claude's
    // view but never reach the Recycle Bin, and drvfs often refuses the
    // rename anyway. Nothing sensible to do from this side of the boundary.
    if crate::paths::is_wsl() {
        if let Some(p) = plan.paths.iter().find(|p| crate::paths::is_wsl_drvfs(p)) {
            bail!(
                "{} is on a Windows drive; WSL cannot move it to the Recycle Bin - run csb from Windows instead",
                p.display()
            );
        }
    }
    trash::delete_all(&plan.paths)?;
    Ok(())
}

fn dir_size(path: &std::path::Path) -> u64 {
    // symlink_metadata, not metadata: following links would let a cycle recurse
    // forever, and would count bytes that deleting the link never frees.
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if meta.is_file() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries.flatten().map(|e| dir_size(&e.path())).sum()
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_rejects_paths_outside_the_claude_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude");
        std::fs::create_dir_all(root.join("projects")).unwrap();
        let dir = ClaudeDir::resolve(Some(&root)).unwrap();

        let outside = tmp.path().join("precious.txt");
        std::fs::write(&outside, "keep me").unwrap();

        let plan = DeletePlan {
            id: "x".into(),
            title: "x".into(),
            paths: vec![outside.clone()],
            bytes: 0,
            recent: false,
        };
        assert!(execute(&dir, &plan).is_err());
        assert!(outside.exists(), "guard must run before any deletion");
    }

    #[test]
    fn empty_plan_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude");
        std::fs::create_dir_all(root.join("projects")).unwrap();
        let dir = ClaudeDir::resolve(Some(&root)).unwrap();
        let plan = DeletePlan {
            id: "x".into(),
            title: "x".into(),
            paths: vec![],
            bytes: 0,
            recent: false,
        };
        assert!(execute(&dir, &plan).is_err());
    }

    #[test]
    fn dir_size_sums_a_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("s");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("a.txt"), b"12345").unwrap();
        std::fs::write(root.join("nested").join("b.txt"), b"123").unwrap();

        assert_eq!(dir_size(&root), 8);
        assert_eq!(dir_size(&root.join("a.txt")), 5);
        assert_eq!(dir_size(&root.join("missing")), 0);
    }

    #[test]
    fn bytes_are_human_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(21 * 1024 * 1024), "21.0 MB");
    }
}
