use anyhow::{bail, Context, Result};
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

/// What a confirm dialog needs to say about a batch of plans.
pub struct PlanSummary {
    pub bytes: u64,
    pub files: usize,
    pub live: usize,
}

pub fn summarize(plans: &[DeletePlan]) -> PlanSummary {
    PlanSummary {
        bytes: plans.iter().map(|p| p.bytes).sum(),
        files: plans.iter().map(|p| p.paths.len()).sum(),
        live: plans.iter().filter(|p| p.recent).count(),
    }
}

/// How a batch of plans ended: how many were trashed, and what stopped it.
pub struct Outcome {
    pub ok: usize,
    pub total: usize,
    pub failed: Option<anyhow::Error>,
}

impl Outcome {
    /// The only place either wording lives, so all three front ends agree.
    pub fn summary(&self, bytes: u64) -> String {
        match &self.failed {
            Some(e) => format!("deleted {} of {}, then failed: {e:#}", self.ok, self.total),
            None => format!(
                "moved {} session(s) ({}) to the recycle bin",
                self.ok,
                human_bytes(bytes)
            ),
        }
    }
}

/// Run every plan in order and stop at the first failure, so what was already
/// trashed is never lost track of.
pub fn execute_all(dir: &ClaudeDir, plans: &[DeletePlan]) -> Outcome {
    let mut ok = 0;
    for p in plans {
        if let Err(e) = execute(dir, p).with_context(|| format!("deleting session {}", p.id)) {
            return Outcome {
                ok,
                total: plans.len(),
                failed: Some(e),
            };
        }
        ok += 1;
    }
    Outcome {
        ok,
        total: plans.len(),
        failed: None,
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
    fn plan_covers_the_transcript_and_every_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude");
        let id = "0241ed3f-1b2c-4d5e-8f9a-0b1c2d3e4f5a";
        let project = root.join("projects").join("-src-proj");
        std::fs::create_dir_all(project.join(id)).unwrap();
        std::fs::write(project.join(format!("{id}.jsonl")), b"12345").unwrap();
        std::fs::write(project.join(id).join("state.json"), b"12").unwrap();
        std::fs::create_dir_all(root.join("session-env").join(id)).unwrap();
        std::fs::write(root.join("session-env").join(id).join("env"), b"123").unwrap();
        std::fs::create_dir_all(root.join("file-history").join(id)).unwrap();
        std::fs::write(root.join("file-history").join(id).join("h.json"), b"1").unwrap();
        let dir = ClaudeDir::resolve(Some(&root)).unwrap();

        let meta = SessionMeta {
            id: id.into(),
            path: dir.projects().join("-src-proj").join(format!("{id}.jsonl")),
            project_slug: "-src-proj".into(),
            size_bytes: 5,
            modified_ms: 0,
            first_ts: None,
            last_ts: None,
            title: "t".into(),
            cwd: None,
            git_branch: None,
            user_msgs: 0,
            assistant_msgs: 0,
            tool_calls: 0,
        };

        // Expected paths come off `dir`, which is canonicalized.
        let in_project = dir.projects().join("-src-proj");
        let plan = plan(&dir, &meta);
        assert_eq!(
            plan.paths,
            vec![
                in_project.join(format!("{id}.jsonl")),
                in_project.join(id),
                dir.root.join("session-env").join(id),
                dir.root.join("file-history").join(id),
            ]
        );
        assert_eq!(plan.bytes, 5 + 2 + 3 + 1);
        assert!(!plan.recent, "an epoch timestamp is not a live session");
    }

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

    // The refused plan goes first so the test never reaches the OS recycle
    // bin, which is not something a headless CI runner reliably has.
    #[test]
    fn execute_all_stops_at_the_first_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("claude");
        std::fs::create_dir_all(root.join("projects")).unwrap();
        let dir = ClaudeDir::resolve(Some(&root)).unwrap();

        let outside = tmp.path().join("precious.txt");
        std::fs::write(&outside, "keep me").unwrap();
        let inside = root.join("projects").join("a.jsonl");
        std::fs::write(&inside, "still here").unwrap();

        let plans = vec![
            DeletePlan {
                id: "a".into(),
                title: "a".into(),
                paths: vec![outside.clone()],
                bytes: 0,
                recent: false,
            },
            DeletePlan {
                id: "b".into(),
                title: "b".into(),
                paths: vec![inside.clone()],
                bytes: 10,
                recent: false,
            },
        ];

        let outcome = execute_all(&dir, &plans);
        assert_eq!(outcome.ok, 0);
        assert_eq!(outcome.total, 2);
        assert!(outcome.failed.is_some());
        assert!(
            outcome
                .summary(10)
                .starts_with("deleted 0 of 2, then failed"),
            "{}",
            outcome.summary(10)
        );
        assert!(outside.exists(), "the refused plan must not be touched");
        assert!(inside.exists(), "the plan after the failure must not run");
    }

    #[test]
    fn summarize_adds_up_every_plan() {
        let plans = vec![
            DeletePlan {
                id: "a".into(),
                title: "a".into(),
                paths: vec!["one".into(), "two".into()],
                bytes: 100,
                recent: true,
            },
            DeletePlan {
                id: "b".into(),
                title: "b".into(),
                paths: vec!["three".into()],
                bytes: 20,
                recent: false,
            },
        ];

        let s = summarize(&plans);
        assert_eq!(s.bytes, 120);
        assert_eq!(s.files, 3);
        assert_eq!(s.live, 1);
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
