//! The user's project repo: experiments, tasks, overlays, fixtures, rubrics
//! and suites as files, plus the local data directory for the database and
//! run artifacts.

use crate::model::*;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MARKER: &str = "wrangler.toml";

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectCfg {
    /// Where the database and run artifacts live. Default `.wrangler`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suite {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Metric the regression verdicts are computed on.
    #[serde(default = "default_primary")]
    pub primary_metric: String,
    /// Experiment names. Each needs an overlay factor with a baseline level.
    pub experiments: Vec<String>,
}

fn default_primary() -> String {
    "judge_score".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct ExperimentEntry {
    pub name: String,
    pub path: String,
    pub description: String,
    pub error: Option<String>,
    pub variants: usize,
    pub tasks: usize,
    pub runs_per_cell: u32,
    pub ablation: bool,
}

impl Project {
    /// Open the project at `root`.
    pub fn open(root: &Path) -> Result<Project> {
        let root = root.canonicalize().with_context(|| format!("project root {}", root.display()))?;
        let cfg: ProjectCfg = match std::fs::read_to_string(root.join(MARKER)) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {MARKER}"))?,
            Err(_) => ProjectCfg::default(),
        };
        let data_dir = match std::env::var("WRANGLER_DATA_DIR") {
            Ok(d) if !d.is_empty() => PathBuf::from(d),
            _ => root.join(cfg.data_dir.as_deref().unwrap_or(".wrangler")),
        };
        Ok(Project { root, data_dir })
    }

    /// Walk up from `start` to the nearest directory holding `wrangler.toml`
    /// or an `experiments/` directory.
    pub fn discover(start: &Path) -> Result<Project> {
        let start = start.canonicalize()?;
        let mut dir = Some(start.as_path());
        while let Some(d) = dir {
            if d.join(MARKER).is_file() || d.join("experiments").is_dir() {
                return Project::open(d);
            }
            dir = d.parent();
        }
        bail!(
            "no wrangler project found at or above {} (run `wrangler init` to create one)",
            start.display()
        )
    }

    /// Create the project layout. Existing files are left alone.
    pub fn init(root: &Path) -> Result<Project> {
        std::fs::create_dir_all(root)?;
        for d in ["experiments", "tasks", "overlays/none", "fixtures", "rubrics", "suites"] {
            std::fs::create_dir_all(root.join(d))?;
        }
        write_if_missing(&root.join(MARKER), "# Harness Wrangler project. Data (database, run artifacts) lives in .wrangler/\n")?;
        write_if_missing(&root.join("overlays/none/.gitkeep"), "")?;
        let gi = root.join(".gitignore");
        let cur = std::fs::read_to_string(&gi).unwrap_or_default();
        if !cur.lines().any(|l| l.trim() == ".wrangler/" || l.trim() == ".wrangler") {
            let sep = if cur.is_empty() || cur.ends_with('\n') { "" } else { "\n" };
            std::fs::write(&gi, format!("{cur}{sep}.wrangler/\n"))?;
        }
        Project::open(root)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("wrangler.db")
    }
    pub fn runs_dir(&self) -> PathBuf {
        self.data_dir.join("runs")
    }
    pub fn abs(&self, rel: &str) -> PathBuf {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }
    pub fn rel(&self, abs: &Path) -> String {
        abs.strip_prefix(&self.root).unwrap_or(abs).to_string_lossy().to_string()
    }

    pub fn experiment_path(&self, name: &str) -> PathBuf {
        let p = Path::new(name);
        if name.ends_with(".toml") {
            if p.is_absolute() || p.exists() {
                return p.to_path_buf();
            }
            return self.root.join(p);
        }
        self.root.join("experiments").join(format!("{name}.toml"))
    }

    pub fn load_experiment(&self, name: &str) -> Result<Experiment> {
        let path = self.experiment_path(name);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading experiment {}", path.display()))?;
        Experiment::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn experiment_toml(&self, name: &str) -> Result<String> {
        let path = self.experiment_path(name);
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
    }

    pub fn save_experiment_toml(&self, name: &str, text: &str) -> Result<Experiment> {
        let exp = Experiment::parse(text)?;
        if name.contains('/') || name.contains("..") {
            bail!("invalid experiment name `{name}`");
        }
        let path = self.root.join("experiments").join(format!("{name}.toml"));
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, text)?;
        Ok(exp)
    }

    pub fn list_experiments(&self) -> Vec<ExperimentEntry> {
        let mut out = vec![];
        for path in glob_sorted(&self.root, "experiments/*.toml") {
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            let rel = self.rel(&path);
            match self.load_experiment(&stem) {
                Ok(e) => {
                    let tasks = self.load_tasks(&e).map(|t| t.len()).unwrap_or(0);
                    out.push(ExperimentEntry {
                        name: stem,
                        path: rel,
                        description: e.description.clone(),
                        error: None,
                        variants: e.variants().len(),
                        tasks,
                        runs_per_cell: e.runs_per_cell,
                        ablation: e.ablation.is_some(),
                    })
                }
                Err(err) => out.push(ExperimentEntry {
                    name: stem,
                    path: rel,
                    description: String::new(),
                    error: Some(format!("{err:#}")),
                    variants: 0,
                    tasks: 0,
                    runs_per_cell: 0,
                    ablation: false,
                }),
            }
        }
        out
    }

    pub fn load_task_file(&self, path: &Path) -> Result<Task> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading task {}", path.display()))?;
        let mut t = Task::parse(&text).with_context(|| format!("in {}", path.display()))?;
        if let Some(pf) = &t.prompt_file {
            t.prompt = std::fs::read_to_string(self.abs(pf)).with_context(|| format!("task `{}` prompt_file {pf}", t.id))?;
        }
        t.source = self.rel(path);
        Ok(t)
    }

    /// Tasks selected by the experiment's include/exclude globs, sorted by id.
    pub fn load_tasks(&self, exp: &Experiment) -> Result<Vec<Task>> {
        let mut paths: Vec<PathBuf> = vec![];
        for pat in &exp.tasks.include {
            let found = glob_sorted(&self.root, pat);
            if found.is_empty() {
                bail!("task pattern `{pat}` matched no files");
            }
            paths.extend(found);
        }
        let excluded: Vec<PathBuf> = exp.tasks.exclude.iter().flat_map(|p| glob_sorted(&self.root, p)).collect();
        paths.retain(|p| !excluded.contains(p));
        paths.sort();
        paths.dedup();
        let mut tasks = vec![];
        for p in paths {
            tasks.push(self.load_task_file(&p)?);
        }
        let mut ids = std::collections::BTreeSet::new();
        for t in &tasks {
            if !ids.insert(t.id.clone()) {
                bail!("duplicate task id `{}`", t.id);
            }
        }
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(tasks)
    }

    pub fn list_all_tasks(&self) -> Vec<Result<Task>> {
        glob_sorted(&self.root, "tasks/**/*.toml").iter().map(|p| self.load_task_file(p)).collect()
    }

    pub fn list_overlays(&self) -> Vec<String> {
        let mut out = vec![];
        let base = self.root.join("overlays");
        for e in walkdir::WalkDir::new(&base).min_depth(1).max_depth(3).sort_by_file_name().into_iter().flatten() {
            if !e.file_type().is_dir() {
                continue;
            }
            let p = e.path();
            let is_overlay = p.join("CLAUDE.md").exists()
                || p.join("AGENTS.md").exists()
                || p.join(".claude").exists()
                || p.file_name().map(|n| n == "none").unwrap_or(false);
            if is_overlay {
                out.push(self.rel(p));
            }
        }
        out
    }

    pub fn suite_path(&self, name: &str) -> PathBuf {
        self.root.join("suites").join(format!("{name}.toml"))
    }

    pub fn load_suite(&self, name: &str) -> Result<Suite> {
        let path = self.suite_path(name);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading suite {}", path.display()))?;
        let s: Suite = toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;
        Ok(s)
    }

    pub fn save_suite(&self, suite: &Suite) -> Result<()> {
        if suite.name.is_empty() || suite.name.contains('/') {
            bail!("invalid suite name");
        }
        for e in &suite.experiments {
            self.load_experiment(e).with_context(|| format!("suite experiment `{e}`"))?;
        }
        std::fs::create_dir_all(self.root.join("suites"))?;
        std::fs::write(self.suite_path(&suite.name), toml::to_string_pretty(suite)?)?;
        Ok(())
    }

    pub fn list_suites(&self) -> Vec<Suite> {
        glob_sorted(&self.root, "suites/*.toml")
            .iter()
            .filter_map(|p| self.load_suite(&p.file_stem()?.to_string_lossy()).ok())
            .collect()
    }

    /// Check everything the experiment references and return warnings.
    /// Hard problems are errors.
    pub fn validate(&self, exp: &Experiment) -> Result<Vec<String>> {
        let mut warnings = vec![];
        let tasks = self.load_tasks(exp)?;
        if tasks.is_empty() {
            bail!("experiment selects no tasks (set [tasks] include)");
        }
        for f in &exp.factors {
            for l in &f.levels {
                match f.kind {
                    FactorKind::Overlay => {
                        if !is_empty_overlay(l.value()) && !self.abs(l.value()).is_dir() {
                            bail!("overlay `{}` (factor `{}`) is not a directory", l.value(), f.name);
                        }
                    }
                    FactorKind::SystemPromptAppend => {
                        if let Some(p) = l.value().strip_prefix("file:") {
                            if !self.abs(p).is_file() {
                                bail!("system prompt file `{p}` not found");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for t in &tasks {
            if let Some(fx) = &t.fixture {
                match (&fx.dir, &fx.repo) {
                    (Some(d), None) => {
                        if !self.abs(d).is_dir() {
                            bail!("task `{}`: fixture dir `{d}` not found", t.id);
                        }
                    }
                    (None, Some(_)) => {}
                    (None, None) => {}
                    (Some(_), Some(_)) => bail!("task `{}`: fixture has both `dir` and `repo`", t.id),
                }
            }
        }
        if let Some(j) = exp.judge() {
            if let Some(r) = &j.rubric {
                if !self.abs(r).is_file() {
                    bail!("judge rubric `{r}` not found");
                }
            }
            if j.rubric_scoring && j.rubric.is_none() && tasks.iter().all(|t| t.expectations.is_empty() && t.expected_output.is_empty()) {
                warnings.push("judge is enabled but no task has expectations, expected_output, or a rubric".into());
            }
        }
        for p in &exp.runner.plugin_dirs {
            if !self.abs(p).exists() {
                bail!("plugin dir `{p}` not found");
            }
        }
        if exp.isolation == Isolation::Docker {
            if exp.runner.docker_image.as_deref().unwrap_or("").is_empty() {
                bail!("isolation = \"docker\" needs [runner] docker_image: an image with the runner CLI installed");
            }
            let up = crate::isolation::docker_available();
            if !up {
                bail!("isolation = \"docker\" needs a running Docker daemon (`docker info` failed or did not answer within 8 seconds)");
            }
            warnings.push("docker isolation: a subscription login lives in the macOS keychain and does not exist in a container. The runner must authenticate from an environment variable passed with [runner] docker_env (ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN and OPENAI_API_KEY pass by default when set).".into());
            if exp.network == NetworkPolicy::Anthropic {
                warnings.push("docker isolation does not apply network = \"anthropic\"; restrict the network with [runner] docker_args instead".into());
            }
        }
        let variants = exp.variants();
        warnings.extend(crate::runner::capability_warnings(exp, &variants)?);
        let total = variants.len() * tasks.len() * exp.runs_per_cell as usize;
        if total > 500 {
            warnings.push(format!("{total} runs planned; consider fewer levels or repetitions"));
        }
        Ok(warnings)
    }
}

fn write_if_missing(path: &Path, content: &str) -> Result<()> {
    if !path.exists() {
        std::fs::write(path, content)?;
    }
    Ok(())
}

pub fn glob_sorted(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let full = if Path::new(pattern).is_absolute() { pattern.to_string() } else { format!("{}/{}", root.display(), pattern) };
    let mut v: Vec<PathBuf> = glob::glob(&full).map(|g| g.flatten().collect()).unwrap_or_default();
    v.sort();
    v
}

/// Resolve the text of a system-prompt-append level.
pub fn resolve_append(project: &Project, value: &str) -> Result<String> {
    match value.strip_prefix("file:") {
        Some(p) => std::fs::read_to_string(project.abs(p)).map_err(|e| anyhow!("system prompt file {p}: {e}")),
        None => Ok(value.to_string()),
    }
}
