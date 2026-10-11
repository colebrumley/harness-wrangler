//! Per-run workspaces: a fresh temp directory with the fixture copied in,
//! a git baseline, the overlay materialized by the runner, and a diff
//! snapshot taken after the run.
//!
//! Workspaces live under the system temp directory, never inside the
//! project repo: agent harnesses walk up parent directories looking for
//! instruction files, and the project's own files must not leak in.

use crate::model::{Fixture, Task};
use crate::project::Project;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BASELINE_TAG: &str = "wrangler-baseline";
const MAX_PATCH_BYTES: usize = 2_000_000;

#[derive(Debug, Clone)]
pub struct Workspace {
    /// The agent's working directory.
    pub dir: PathBuf,
    /// Where this run's artifacts are stored.
    pub run_dir: PathBuf,
    pub project: Project,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiffStats {
    pub added: u64,
    pub removed: u64,
    pub files: Vec<String>,
    pub truncated: bool,
}

pub fn workspaces_root() -> PathBuf {
    let base = std::env::temp_dir();
    let base = base.canonicalize().unwrap_or(base);
    base.join("wrangler-ws")
}

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .args(["-c", "user.name=wrangler", "-c", "user.email=wrangler@localhost"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "-c", "advice.detachedHead=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    c
}

fn run_git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git(dir).args(args).output().context("running git")?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Copy the contents of `src` into `dst` (which must exist). Uses APFS
/// clones on macOS so large fixtures cost no extra disk until written.
pub fn copy_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        bail!("{} is not a directory", src.display());
    }
    std::fs::create_dir_all(dst)?;
    let from = format!("{}/.", src.display());
    if cfg!(target_os = "macos") {
        let ok = Command::new("cp").arg("-cR").arg(&from).arg(dst).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if ok {
            return Ok(());
        }
    }
    let st = Command::new("cp").arg("-R").arg(&from).arg(dst).status().context("running cp")?;
    if !st.success() {
        bail!("copying {} to {} failed", src.display(), dst.display());
    }
    Ok(())
}

impl Workspace {
    /// Create the workspace for one run and populate it from the task fixture.
    pub fn create(project: &Project, task: &Task, run_dir: &Path, tag: &str) -> Result<Workspace> {
        let root = workspaces_root();
        std::fs::create_dir_all(&root)?;
        let dir = root.join(format!("{tag}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::create_dir_all(&dir)?;
        std::fs::create_dir_all(run_dir)?;
        let ws = Workspace { dir, run_dir: run_dir.to_path_buf(), project: project.clone() };
        ws.populate(task.fixture.as_ref())?;
        Ok(ws)
    }

    fn populate(&self, fixture: Option<&Fixture>) -> Result<()> {
        match fixture {
            Some(Fixture { repo: Some(repo), git_ref, .. }) => {
                let local = self.project.abs(repo);
                let src = if local.exists() { local.to_string_lossy().to_string() } else { repo.clone() };
                let st = git(&self.dir).args(["clone", "--quiet", &src, "."]).status()?;
                if !st.success() {
                    bail!("cloning fixture repo {repo} failed");
                }
                if let Some(r) = git_ref {
                    run_git(&self.dir, &["checkout", "--quiet", r])?;
                }
            }
            Some(Fixture { dir: Some(d), .. }) => {
                copy_dir_contents(&self.project.abs(d), &self.dir)?;
            }
            _ => {}
        }
        if !self.dir.join(".git").exists() {
            run_git(&self.dir, &["init", "--quiet", "-b", "main"])?;
        }
        Ok(())
    }

    /// Copy an overlay directory's contents over the workspace.
    pub fn apply_overlay(&self, overlay: &str) -> Result<()> {
        let src = self.project.abs(overlay);
        // A bare skill directory (SKILL.md at its root) is installed as a
        // project skill, so an experiment can point at a skill where it
        // lives in the repo instead of a copy laid out as an overlay.
        if src.join("SKILL.md").is_file() {
            let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "skill".into());
            let dst = self.dir.join(".claude/skills").join(name);
            return copy_dir_contents(&src, &dst).with_context(|| format!("installing skill {overlay}"));
        }
        copy_dir_contents(&src, &self.dir).with_context(|| format!("materializing overlay {overlay}"))?;
        let _ = std::fs::remove_file(self.dir.join(".gitkeep"));
        Ok(())
    }

    /// Commit fixture + overlay so the post-run diff shows only the agent's work.
    pub fn commit_baseline(&self) -> Result<()> {
        run_git(&self.dir, &["add", "-A"])?;
        run_git(&self.dir, &["commit", "--quiet", "--allow-empty", "--no-verify", "-m", "wrangler baseline"])?;
        run_git(&self.dir, &["tag", "-f", BASELINE_TAG])?;
        Ok(())
    }

    /// Put the workspace back to the baseline (used before a retry).
    pub fn reset_to_baseline(&self) -> Result<()> {
        run_git(&self.dir, &["reset", "--quiet", "--hard", BASELINE_TAG])?;
        run_git(&self.dir, &["clean", "-fdxq"])?;
        Ok(())
    }

    /// Diff the workspace against the baseline, write `diff.patch` to the
    /// run directory and return the stats.
    pub fn snapshot(&self) -> Result<(String, DiffStats)> {
        run_git(&self.dir, &["add", "-A"])?;
        let numstat = run_git(&self.dir, &["diff", "--cached", "--numstat", BASELINE_TAG])?;
        let mut stats = DiffStats::default();
        for line in numstat.lines() {
            let mut it = line.split('\t');
            let (a, r, f) = (it.next().unwrap_or("0"), it.next().unwrap_or("0"), it.next().unwrap_or(""));
            stats.added += a.parse::<u64>().unwrap_or(0);
            stats.removed += r.parse::<u64>().unwrap_or(0);
            if !f.is_empty() {
                stats.files.push(f.to_string());
            }
        }
        let out = git(&self.dir).args(["diff", "--cached", "--no-color", BASELINE_TAG]).output()?;
        let mut patch = String::from_utf8_lossy(&out.stdout).to_string();
        if patch.len() > MAX_PATCH_BYTES {
            let mut cut = MAX_PATCH_BYTES;
            while !patch.is_char_boundary(cut) {
                cut -= 1;
            }
            patch.truncate(cut);
            patch.push_str("\n[diff truncated]\n");
            stats.truncated = true;
        }
        std::fs::write(self.run_dir.join("diff.patch"), &patch)?;
        Ok((patch, stats))
    }

    /// Read a workspace file; refuses paths that escape the workspace.
    pub fn read_file(&self, rel: &str) -> Option<String> {
        let p = self.resolve(rel)?;
        std::fs::read(&p).ok().map(|b| String::from_utf8_lossy(&b).to_string())
    }

    pub fn resolve(&self, rel: &str) -> Option<PathBuf> {
        let p = Path::new(rel);
        if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return None;
        }
        Some(self.dir.join(p))
    }

    pub fn remove(&self) {
        let root = workspaces_root();
        if self.dir.starts_with(&root) && self.dir != root {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_overlay_and_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let project = Project::init(root).unwrap();
        std::fs::create_dir_all(root.join("fixtures/f")).unwrap();
        std::fs::write(root.join("fixtures/f/a.txt"), "one\ntwo\n").unwrap();
        std::fs::create_dir_all(root.join("overlays/o/.claude/skills/s")).unwrap();
        std::fs::write(root.join("overlays/o/.claude/skills/s/SKILL.md"), "skill").unwrap();
        let task = Task::parse("id='t'\nprompt='p'\nfixture={dir='fixtures/f'}\n").unwrap();
        let ws = Workspace::create(&project, &task, &root.join("run"), "test").unwrap();
        ws.apply_overlay("overlays/o").unwrap();
        ws.commit_baseline().unwrap();
        assert!(ws.dir.join(".claude/skills/s/SKILL.md").exists());
        std::fs::write(ws.dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(ws.dir.join("new.md"), "hello\n").unwrap();
        let (patch, stats) = ws.snapshot().unwrap();
        assert_eq!(stats.files, vec!["a.txt", "new.md"]);
        assert_eq!((stats.added, stats.removed), (3, 1));
        assert!(patch.contains("+TWO") && !patch.contains("SKILL.md"));
        assert!(ws.read_file("../x").is_none());
        ws.reset_to_baseline().unwrap();
        assert!(!ws.dir.join("new.md").exists());
        // A bare skill directory used as an overlay is installed as a project skill.
        std::fs::create_dir_all(root.join("plugins/p/skills/my-skill/references")).unwrap();
        std::fs::write(root.join("plugins/p/skills/my-skill/SKILL.md"), "---\nname: my-skill\n---\nbody").unwrap();
        std::fs::write(root.join("plugins/p/skills/my-skill/references/r.md"), "ref").unwrap();
        ws.apply_overlay("plugins/p/skills/my-skill").unwrap();
        assert!(ws.dir.join(".claude/skills/my-skill/SKILL.md").exists() && ws.dir.join(".claude/skills/my-skill/references/r.md").exists());
        assert!(!ws.dir.join("SKILL.md").exists());
        ws.remove();
        assert!(!ws.dir.exists());
    }
}
