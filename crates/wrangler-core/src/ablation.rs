//! Ablation generation: from one skill or `CLAUDE.md`, produce the
//! no-skill baseline, the full file, one leave-one-section-out overlay per
//! section, and optionally a description-only overlay, plus the experiment
//! that compares them.

use crate::model::*;
use crate::project::Project;
use crate::util::{est_tokens, slug};
use crate::workspace::copy_dir_contents;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub index: usize,
    pub title: String,
    pub level: u32,
    /// 1-based inclusive line range in the whole file.
    pub start_line: usize,
    pub end_line: usize,
    pub est_tokens: u64,
    /// False for heading-only sections, which are never ablated.
    pub has_body: bool,
}

/// Split a Markdown file into YAML frontmatter (with its `---` fences) and body.
/// Returns the number of lines the frontmatter occupies.
pub fn split_frontmatter(text: &str) -> (usize, &str, &str) {
    if !text.starts_with("---") {
        return (0, "", text);
    }
    let mut offset = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        offset += line.len();
        if i > 0 && line.trim_end() == "---" {
            return (i + 1, &text[..offset], &text[offset..]);
        }
    }
    (0, "", text)
}

pub fn frontmatter_field(text: &str, key: &str) -> Option<String> {
    let (_, fm, _) = split_frontmatter(text);
    fm.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').trim_matches('\'').to_string())
    })
}

fn heading(line: &str) -> Option<(u32, String)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    Some((hashes as u32, rest.trim().trim_end_matches('#').trim().to_string()))
}

/// Split at headings of level ≤ `depth`. Text before the first heading is
/// an `(intro)` section. Headings inside fenced code blocks are ignored.
pub fn split_sections(text: &str, depth: u32) -> Vec<Section> {
    let (fm_lines, _, body) = split_frontmatter(text);
    let lines: Vec<&str> = body.lines().collect();
    let mut starts: Vec<(usize, u32, String)> = vec![];
    let mut fence: Option<&str> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if let Some(f) = fence {
            if t.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if t.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        if let Some((level, title)) = heading(line) {
            if level <= depth {
                starts.push((i, level, title));
            }
        }
    }
    let first = starts.first().map(|s| s.0).unwrap_or(lines.len());
    if lines[..first].iter().any(|l| !l.trim().is_empty()) {
        starts.insert(0, (0, 0, "(intro)".into()));
    }
    let mut out = vec![];
    for (n, (start, level, title)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map(|s| s.0).unwrap_or(lines.len());
        let body_from = if *level == 0 { *start } else { start + 1 };
        let chunk = lines[*start..end].join("\n");
        out.push(Section {
            index: n,
            title: title.clone(),
            level: *level,
            start_line: fm_lines + start + 1,
            end_line: fm_lines + end,
            est_tokens: est_tokens(&chunk),
            has_body: lines[body_from..end].iter().any(|l| !l.trim().is_empty()),
        });
    }
    out
}

/// The file with one section's lines removed.
pub fn without_section(text: &str, s: &Section) -> String {
    let mut out: Vec<&str> = vec![];
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let n = i + 1;
        if n < s.start_line || n > s.end_line {
            out.push(line);
        }
    }
    out.concat()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AblationRequest {
    /// A `SKILL.md`, a skill directory, a `CLAUDE.md`, or an overlay
    /// directory holding exactly one of those. Relative to the project root.
    pub source: String,
    /// Experiment name. Default `<skill>-ablation`.
    #[serde(default)]
    pub name: Option<String>,
    /// Heading depth to split at. Default 2.
    #[serde(default)]
    pub depth: Option<u32>,
    /// Also generate the frontmatter-only variant (skills only).
    #[serde(default)]
    pub description_only: bool,
    /// Task globs for the generated experiment.
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default)]
    pub runs_per_cell: Option<u32>,
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub judge_model: Option<String>,
    #[serde(default)]
    pub rubric: Option<String>,
    #[serde(default)]
    pub primary_metric: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AblationPlan {
    pub experiment: String,
    pub experiment_path: String,
    pub kind: String,
    pub source: String,
    pub overlays: Vec<String>,
    pub sections: Vec<Section>,
    pub skipped_sections: Vec<String>,
}

struct Source {
    /// The Markdown file being ablated.
    file: PathBuf,
    kind: &'static str,
    /// Skill directory (skills) whose other files ride along in every variant.
    skill_dir: Option<PathBuf>,
    name: String,
}

fn resolve_source(project: &Project, source: &str) -> Result<Source> {
    let p = project.abs(source);
    let skill = |dir: &Path| -> Result<Source> {
        let file = dir.join("SKILL.md");
        let text = std::fs::read_to_string(&file)?;
        let name = frontmatter_field(&text, "name").unwrap_or_else(|| dir.file_name().unwrap().to_string_lossy().to_string());
        Ok(Source { file, kind: "skill", skill_dir: Some(dir.to_path_buf()), name: slug(&name) })
    };
    if p.is_file() {
        let fname = p.file_name().unwrap().to_string_lossy().to_string();
        if fname == "SKILL.md" {
            return skill(p.parent().unwrap());
        }
        let parent = p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        return Ok(Source { file: p.clone(), kind: "claude_md", skill_dir: None, name: slug(&format!("{parent}-claude-md")) });
    }
    if !p.is_dir() {
        bail!("ablation source `{source}` not found");
    }
    if p.join("SKILL.md").is_file() {
        return skill(&p);
    }
    let skills: Vec<PathBuf> = std::fs::read_dir(p.join(".claude/skills"))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|d| d.join("SKILL.md").is_file()).collect())
        .unwrap_or_default();
    match (skills.len(), p.join("CLAUDE.md").is_file()) {
        (1, false) => skill(&skills[0]),
        (0, true) => Ok(Source {
            file: p.join("CLAUDE.md"),
            kind: "claude_md",
            skill_dir: None,
            name: slug(&format!("{}-claude-md", p.file_name().unwrap().to_string_lossy())),
        }),
        (0, false) => bail!("`{source}` holds no SKILL.md or CLAUDE.md"),
        _ => bail!("`{source}` holds more than one skill or instruction file; point at the one to ablate"),
    }
}

fn write_variant(src: &Source, dir: &Path, content: Option<&str>) -> Result<()> {
    let _ = std::fs::remove_dir_all(dir);
    match &src.skill_dir {
        Some(skill_dir) => {
            let dst = dir.join(".claude/skills").join(skill_dir.file_name().unwrap());
            std::fs::create_dir_all(&dst)?;
            copy_dir_contents(skill_dir, &dst)?;
            if let Some(c) = content {
                std::fs::write(dst.join("SKILL.md"), c)?;
            }
        }
        None => {
            std::fs::create_dir_all(dir)?;
            std::fs::write(dir.join("CLAUDE.md"), content.map(|c| c.to_string()).unwrap_or(std::fs::read_to_string(&src.file)?))?;
        }
    }
    Ok(())
}

/// Write the ablation overlays and experiment file into the project.
pub fn generate(project: &Project, req: &AblationRequest) -> Result<AblationPlan> {
    let src = resolve_source(project, &req.source)?;
    let text = std::fs::read_to_string(&src.file).with_context(|| format!("reading {}", src.file.display()))?;
    let depth = req.depth.unwrap_or(2).clamp(1, 6);
    let sections = split_sections(&text, depth);
    let ablatable: Vec<&Section> = sections.iter().filter(|s| s.has_body).collect();
    if ablatable.len() < 2 {
        bail!("found {} section(s) with content at heading depth {depth}; nothing to ablate (try a deeper --depth)", ablatable.len());
    }
    let exp_name = req.name.clone().unwrap_or_else(|| format!("{}-ablation", src.name));
    if exp_name.contains('/') {
        bail!("invalid experiment name `{exp_name}`");
    }
    let base_rel = format!("overlays/ablation/{exp_name}");
    let base = project.abs(&base_rel);
    std::fs::create_dir_all(project.abs("overlays/none"))?;

    let mut overlays = vec!["overlays/none".to_string()];
    let mut levels = vec![Level::Detailed { value: "overlays/none".into(), label: Some("none".into()), extra: Default::default() }];
    let full_rel = format!("{base_rel}/full");
    write_variant(&src, &base.join("full"), None)?;
    overlays.push(full_rel.clone());
    levels.push(Level::Detailed { value: full_rel.clone(), label: Some("full".into()), extra: Default::default() });

    let mut meta_sections = vec![];
    for s in &ablatable {
        let dir = format!("without-{:02}-{}", s.index + 1, slug(&s.title));
        let rel = format!("{base_rel}/{dir}");
        write_variant(&src, &base.join(&dir), Some(&without_section(&text, s)))?;
        overlays.push(rel.clone());
        levels.push(Level::Detailed { value: rel.clone(), label: Some(format!("−{}", short_title(&s.title))), extra: Default::default() });
        meta_sections.push(AblationSection { index: s.index, title: s.title.clone(), overlay: rel, est_tokens: s.est_tokens, lines: (s.start_line, s.end_line) });
    }
    let mut description_only = None;
    if req.description_only {
        if src.kind != "skill" {
            bail!("description-only applies to skills (a CLAUDE.md has no frontmatter description)");
        }
        let (_, fm, _) = split_frontmatter(&text);
        if fm.is_empty() {
            bail!("skill has no frontmatter, so there is no description-only variant");
        }
        let rel = format!("{base_rel}/description-only");
        write_variant(&src, &base.join("description-only"), Some(fm))?;
        overlays.push(rel.clone());
        levels.push(Level::Detailed { value: rel.clone(), label: Some("description-only".into()), extra: Default::default() });
        description_only = Some(rel);
    }

    let mut graders = std::collections::BTreeMap::new();
    if let Some(m) = &req.judge_model {
        graders.insert(
            "default".to_string(),
            GraderDefaults {
                judge: Some(JudgeCfg { model: m.clone(), effort: None, rubric: req.rubric.clone(), rubric_scoring: true, pairwise: false, pairwise_mode: None, pairwise_reps: None }),
                human: None,
                deterministic: vec![],
            },
        );
    }
    let exp = Experiment {
        name: exp_name.clone(),
        description: format!("Section ablation of {} (split at heading depth {depth})", project.rel(&src.file)),
        runs_per_cell: req.runs_per_cell.unwrap_or(DEFAULT_RUNS_PER_CELL),
        timeout_secs: DEFAULT_TIMEOUT_SECS,
        isolation: Isolation::Workspace,
        network: NetworkPolicy::Full,
        primary_metric: req.primary_metric.clone(),
        keep_workspaces: false,
        runner: RunnerCfg { kind: req.runner.clone().unwrap_or_else(|| "claude-code".into()), model: req.model.clone(), effort: req.effort.clone(), ..Default::default() },
        factors: vec![Factor { name: "overlay".into(), kind: FactorKind::Overlay, levels, flag: None, env: None }],
        tasks: TaskSel { include: if req.tasks.is_empty() { vec!["tasks/**/*.toml".into()] } else { req.tasks.clone() }, exclude: vec![] },
        graders,
        ablation: Some(AblationMeta {
            source: project.rel(&src.file),
            kind: src.kind.into(),
            depth,
            factor: "overlay".into(),
            baseline: "overlays/none".into(),
            full: full_rel,
            description_only,
            sections: meta_sections,
        }),
    };
    let toml_text = exp.to_toml()?;
    project.save_experiment_toml(&exp_name, &toml_text)?;
    Ok(AblationPlan {
        experiment: exp_name.clone(),
        experiment_path: format!("experiments/{exp_name}.toml"),
        kind: src.kind.into(),
        source: project.rel(&src.file),
        overlays,
        skipped_sections: sections.iter().filter(|s| !s.has_body).map(|s| s.title.clone()).collect(),
        sections: ablatable.into_iter().cloned().collect(),
    })
}

fn short_title(t: &str) -> String {
    let s: String = t.chars().take(32).collect();
    if t.chars().count() > 32 {
        format!("{s}…")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = "---\nname: demo-skill\ndescription: Use when writing.\n---\n# Demo\n\nIntro line.\n\n## One\n\nBody one.\n\n```md\n## not a heading\n```\n\n## Two\n\nBody two.\n\n### Deep\n\nDeeper.\n\n## Empty\n";

    #[test]
    fn splits_sections_respecting_fences_and_frontmatter() {
        let s = split_sections(SKILL, 2);
        let titles: Vec<_> = s.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(titles, vec!["Demo", "One", "Two", "Empty"]);
        assert!(s[0].has_body && !s[3].has_body);
        assert_eq!((s[1].start_line, s[1].end_line), (9, 16));
        let w = without_section(SKILL, &s[1]);
        assert!(!w.contains("Body one") && !w.contains("not a heading") && w.contains("Body two") && w.starts_with("---\nname: demo-skill"));
        assert_eq!(split_sections(SKILL, 3).len(), 5);
        assert_eq!(split_sections("plain intro\n\n# H\nx\n", 1)[0].title, "(intro)");
        assert_eq!(frontmatter_field(SKILL, "name").as_deref(), Some("demo-skill"));
    }

    #[test]
    fn generates_overlays_and_experiment() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::init(tmp.path()).unwrap();
        let skill_dir = tmp.path().join("overlays/demo/.claude/skills/demo");
        std::fs::create_dir_all(skill_dir.join("references")).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), SKILL).unwrap();
        std::fs::write(skill_dir.join("references/extra.md"), "ref").unwrap();
        std::fs::create_dir_all(tmp.path().join("tasks")).unwrap();
        std::fs::write(tmp.path().join("tasks/t.toml"), "id='t'\nprompt='p'\n").unwrap();
        let plan = generate(&project, &AblationRequest { source: "overlays/demo".into(), description_only: true, ..Default::default() }).unwrap();
        assert_eq!(plan.experiment, "demo-skill-ablation");
        assert_eq!(plan.sections.len(), 3);
        assert_eq!(plan.skipped_sections, vec!["Empty"]);
        // none, full, 3 leave-one-out, description-only
        assert_eq!(plan.overlays.len(), 6);
        let exp = project.load_experiment("demo-skill-ablation").unwrap();
        assert_eq!(exp.variants().len(), 6);
        project.validate(&exp).unwrap();
        let base = tmp.path().join("overlays/ablation/demo-skill-ablation");
        let without_one = std::fs::read_to_string(base.join("without-02-one/.claude/skills/demo/SKILL.md")).unwrap();
        assert!(!without_one.contains("Body one") && without_one.contains("Body two"));
        assert!(base.join("without-02-one/.claude/skills/demo/references/extra.md").exists());
        let desc = std::fs::read_to_string(base.join("description-only/.claude/skills/demo/SKILL.md")).unwrap();
        assert_eq!(desc, "---\nname: demo-skill\ndescription: Use when writing.\n---\n");
        let meta = exp.ablation.unwrap();
        assert_eq!(meta.sections[1].title, "One");
        assert!(meta.description_only.is_some());
    }
}
