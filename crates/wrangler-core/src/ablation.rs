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

/// The frontmatter split around the description's text, so that
/// `prefix + sentences.concat() + suffix` is the frontmatter byte for byte.
#[derive(Debug, Clone, PartialEq)]
pub struct DescriptionParts {
    pub prefix: String,
    /// Each sentence with the whitespace that follows it.
    pub sentences: Vec<String>,
    pub suffix: String,
}

impl DescriptionParts {
    pub fn text(&self) -> String {
        self.sentences.concat().split_whitespace().collect::<Vec<_>>().join(" ")
    }
    /// The frontmatter with one sentence removed.
    pub fn without(&self, index: usize) -> String {
        let mut value: String = self.sentences.iter().enumerate().filter(|(i, _)| *i != index).map(|(_, s)| s.as_str()).collect();
        // Dropping the last sentence leaves the previous one's trailing space behind.
        if index + 1 == self.sentences.len() {
            value.truncate(value.trim_end().len());
        }
        format!("{}{value}{}", self.prefix, self.suffix)
    }
    pub fn rejoin(&self) -> String {
        format!("{}{}{}", self.prefix, self.sentences.concat(), self.suffix)
    }
}

/// Split text into sentences at `.`, `!` or `?` followed by whitespace. Each
/// piece keeps its trailing whitespace, so the pieces concatenate back.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && chars.peek().map(|n| n.is_whitespace()).unwrap_or(false) {
            while let Some(n) = chars.peek().copied().filter(|n| n.is_whitespace()) {
                cur.push(n);
                chars.next();
            }
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Locate the `description:` value in a frontmatter block (with its `---`
/// fences) and split it into sentences. Handles a plain or quoted value on
/// one line, a plain value continued on indented lines, and `>` / `|` blocks.
pub fn description_parts(frontmatter: &str) -> Option<DescriptionParts> {
    let mut offset = 0;
    let lines: Vec<&str> = frontmatter.split_inclusive('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        let Some(rest) = line.strip_prefix("description:") else {
            offset += line.len();
            continue;
        };
        let lead = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let value_at = offset + "description:".len() + lead;
        let value = rest[lead..].trim_end_matches(['\n', '\r']);
        // Lines that continue the value: indented or blank, up to the next key or fence.
        let mut block_end = offset + line.len();
        for l in &lines[i + 1..] {
            if l.starts_with([' ', '\t']) || l.trim().is_empty() {
                block_end += l.len();
            } else {
                break;
            }
        }
        let (start, end) = if let Some(q) = value.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let close = value[1..].rfind(q)?;
            (value_at + 1, value_at + 1 + close)
        } else if value.starts_with('>') || value.starts_with('|') {
            (offset + line.len(), block_end)
        } else {
            (value_at, block_end)
        };
        // Trailing whitespace (the final newline) belongs to the suffix.
        let raw = &frontmatter[start..end];
        let end = start + raw.trim_end().len();
        let sentences = split_sentences(&frontmatter[start..end]);
        if sentences.is_empty() {
            return None;
        }
        return Some(DescriptionParts { prefix: frontmatter[..start].to_string(), sentences, suffix: frontmatter[end..].to_string() });
    }
    None
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
    /// Ablate the frontmatter description one sentence at a time, body
    /// intact, instead of the body's sections (skills only).
    #[serde(default)]
    pub description_sentences: bool,
    /// Add a placebo variant: the frontmatter kept, the body replaced by
    /// innocuous text of matched length.
    #[serde(default)]
    pub placebo: bool,
    /// Sentences for the placebo instead of the built-in corpus (one per line).
    #[serde(default)]
    pub placebo_file: Option<String>,
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

pub(crate) struct Source {
    /// The Markdown file being ablated.
    pub(crate) file: PathBuf,
    pub(crate) kind: &'static str,
    /// Skill directory (skills) whose other files ride along in every variant.
    skill_dir: Option<PathBuf>,
    name: String,
    /// The skill's own name as the harness reports it (frontmatter `name`).
    pub(crate) display_name: String,
}

pub(crate) fn resolve_source(project: &Project, source: &str) -> Result<Source> {
    let p = project.abs(source);
    let skill = |dir: &Path| -> Result<Source> {
        let file = dir.join("SKILL.md");
        let text = std::fs::read_to_string(&file)?;
        let name = frontmatter_field(&text, "name").unwrap_or_else(|| dir.file_name().unwrap().to_string_lossy().to_string());
        Ok(Source { file, kind: "skill", skill_dir: Some(dir.to_path_buf()), name: slug(&name), display_name: name })
    };
    if p.is_file() {
        let fname = p.file_name().unwrap().to_string_lossy().to_string();
        if fname == "SKILL.md" {
            return skill(p.parent().unwrap());
        }
        let parent = p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        return Ok(Source { file: p.clone(), kind: "claude_md", skill_dir: None, name: slug(&format!("{parent}-claude-md")), display_name: String::new() });
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
            display_name: String::new(),
        }),
        (0, false) => bail!("`{source}` holds no SKILL.md or CLAUDE.md"),
        _ => bail!("`{source}` holds more than one skill or instruction file; point at the one to ablate"),
    }
}

fn write_variant(src: &Source, dir: &Path, content: Option<&str>) -> Result<()> {
    write_variant_files(src, dir, content, true)
}

/// `siblings`: copy the skill directory's other files (references, scripts)
/// alongside. A placebo leaves them out: its body points at none of them.
fn write_variant_files(src: &Source, dir: &Path, content: Option<&str>, siblings: bool) -> Result<()> {
    let _ = std::fs::remove_dir_all(dir);
    match &src.skill_dir {
        Some(skill_dir) => {
            let dst = dir.join(".claude/skills").join(skill_dir.file_name().unwrap());
            std::fs::create_dir_all(&dst)?;
            if siblings {
                copy_dir_contents(skill_dir, &dst)?;
            }
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
    let by_sentence = req.description_sentences;
    let (fm_lines, fm, body) = split_frontmatter(&text);
    let description = if by_sentence {
        if src.kind != "skill" {
            bail!("--description-sentences applies to skills (a CLAUDE.md has no frontmatter description)");
        }
        let parts = description_parts(fm).context("skill has no frontmatter description to split")?;
        if parts.sentences.len() < 2 {
            bail!("the description is one sentence; there is nothing to ablate");
        }
        Some(parts)
    } else {
        None
    };
    // In sentence mode each "section" is one sentence of the description.
    let sections: Vec<Section> = match &description {
        Some(parts) => {
            let mut line = parts.prefix.matches('\n').count() + 1;
            parts
                .sentences
                .iter()
                .enumerate()
                .map(|(i, sentence)| {
                    let start = line;
                    line += sentence.matches('\n').count();
                    Section { index: i, title: sentence.split_whitespace().collect::<Vec<_>>().join(" "), level: 0, start_line: start, end_line: line.min(fm_lines.max(start)), est_tokens: est_tokens(sentence), has_body: true }
                })
                .collect()
        }
        None => split_sections(&text, depth),
    };
    let ablatable: Vec<&Section> = sections.iter().filter(|s| s.has_body).collect();
    if ablatable.len() < 2 {
        bail!("found {} section(s) with content at heading depth {depth}; nothing to ablate (try a deeper --depth)", ablatable.len());
    }
    let exp_name = req.name.clone().unwrap_or_else(|| format!("{}-{}", src.name, if by_sentence { "description-ablation" } else { "ablation" }));
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
        let dir = format!("without-{}{:02}-{}", if by_sentence { "sentence-" } else { "" }, s.index + 1, slug(&s.title));
        let rel = format!("{base_rel}/{dir}");
        let content = match &description {
            // The body is appended untouched: only the description changes.
            Some(parts) => format!("{}{body}", parts.without(s.index)),
            None => without_section(&text, s),
        };
        write_variant(&src, &base.join(&dir), Some(&content))?;
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

    let mut placebo = None;
    if req.placebo || req.placebo_file.is_some() {
        let corpus = match &req.placebo_file {
            Some(f) => crate::placebo::Corpus::parse(&std::fs::read_to_string(project.abs(f)).with_context(|| format!("reading placebo file {f}"))?),
            None => crate::placebo::Corpus::builtin(),
        };
        if corpus.sentences.is_empty() {
            bail!("the placebo file has no sentences (one per line)");
        }
        let rel = format!("{base_rel}/placebo");
        write_variant_files(&src, &base.join("placebo"), Some(&crate::placebo::placebo_file(&text, &corpus)), false)?;
        overlays.push(rel.clone());
        levels.push(Level::Detailed { value: rel.clone(), label: Some("placebo".into()), extra: Default::default() });
        placebo = Some(rel);
    }

    let mut graders = std::collections::BTreeMap::new();
    if let Some(m) = &req.judge_model {
        graders.insert(
            "default".to_string(),
            GraderDefaults {
                judge: Some(JudgeCfg { model: m.clone(), effort: None, rubric: req.rubric.clone(), rubric_scoring: true, pairwise: false, pairwise_mode: None, pairwise_reps: None, transcript: None }),
                human: None,
                deterministic: vec![],
            },
        );
    }
    // A description ablation is a trigger experiment: it runs the skill's
    // trigger corpus when there is one, and attributes precision and recall.
    let trigger_glob = format!("tasks/trigger/{}/*.toml", src.name);
    let has_corpus = by_sentence && !crate::project::glob_sorted(&project.root, &trigger_glob).is_empty();
    let default_tasks = if has_corpus { trigger_glob } else { "tasks/**/*.toml".to_string() };
    let exp = Experiment {
        name: exp_name.clone(),
        description: if by_sentence { format!("Description ablation of {} (one sentence removed per variant, body intact)", project.rel(&src.file)) } else { format!("Section ablation of {} (split at heading depth {depth})", project.rel(&src.file)) },
        runs_per_cell: req.runs_per_cell.unwrap_or(DEFAULT_RUNS_PER_CELL),
        timeout_secs: DEFAULT_TIMEOUT_SECS,
        isolation: Isolation::Workspace,
        network: NetworkPolicy::Full,
        primary_metric: req.primary_metric.clone().or_else(|| by_sentence.then(|| "trigger_f1".to_string())),
        budget_usd: None,
        keep_workspaces: false,
        runner: RunnerCfg { kind: req.runner.clone().unwrap_or_else(|| "claude-code".into()), model: req.model.clone(), effort: req.effort.clone(), ..Default::default() },
        factors: vec![Factor { name: "overlay".into(), kind: FactorKind::Overlay, levels, flag: None, env: None }],
        tasks: TaskSel { include: if req.tasks.is_empty() { vec![default_tasks] } else { req.tasks.clone() }, exclude: vec![] },
        graders,
        ablation: Some(AblationMeta {
            source: project.rel(&src.file),
            kind: src.kind.into(),
            depth,
            factor: "overlay".into(),
            baseline: "overlays/none".into(),
            full: full_rel,
            description_only,
            mode: by_sentence.then(|| "description_sentences".to_string()),
            placebo,
            sections: meta_sections,
        }),
        trigger: by_sentence.then(|| TriggerCfg { skill: Some(src.display_name.clone()), skills: vec![] }),
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
    fn description_sentences_round_trip() {
        let plain = "---\nname: s\ndescription: Use when writing. Also for emails! Not for code? Keeps meaning fixed.\nlicense: MIT\n---\n";
        let quoted = "---\ndescription: \"Use when writing. Makes it short.\"\nname: s\n---\n";
        let folded = "---\nname: s\ndescription: >\n  Use when writing or revising.\n  Makes the writing short. Keeps\n  the meaning fixed.\n---\n";
        let wrapped = "---\ndescription: Use when writing.\n  Second sentence here.\nname: s\n---\n";
        for fm in [plain, quoted, folded, wrapped] {
            let parts = description_parts(fm).unwrap();
            assert_eq!(parts.rejoin(), fm, "sentence split must round-trip the frontmatter");
            assert!(parts.sentences.len() >= 2, "{parts:?}");
        }
        let p = description_parts(plain).unwrap();
        assert_eq!(p.sentences, vec!["Use when writing. ", "Also for emails! ", "Not for code? ", "Keeps meaning fixed."]);
        assert_eq!(p.without(1), "---\nname: s\ndescription: Use when writing. Not for code? Keeps meaning fixed.\nlicense: MIT\n---\n");
        assert_eq!(p.without(3), "---\nname: s\ndescription: Use when writing. Also for emails! Not for code?\nlicense: MIT\n---\n");
        assert_eq!(description_parts(quoted).unwrap().without(0), "---\ndescription: \"Makes it short.\"\nname: s\n---\n");
        assert_eq!(description_parts(folded).unwrap().text(), "Use when writing or revising. Makes the writing short. Keeps the meaning fixed.");
        assert!(description_parts("---\nname: s\n---\n").is_none());
    }

    #[test]
    fn description_ablation_leaves_the_body_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Project::init(tmp.path()).unwrap();
        let skill = "---\nname: demo-skill\ndescription: Use when writing summaries. Also for emails. Never for code.\n---\n# Demo\n\nBody line.  \n\n## One\n\ttabbed\n";
        let dir = tmp.path().join("overlays/demo/.claude/skills/demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), skill).unwrap();
        std::fs::create_dir_all(tmp.path().join("tasks/trigger/demo-skill")).unwrap();
        std::fs::write(tmp.path().join("tasks/trigger/demo-skill/a.toml"), "id='a'\nprompt='Write a summary'\ntrigger='should'\n").unwrap();
        let plan = generate(&project, &AblationRequest { source: "overlays/demo".into(), description_sentences: true, ..Default::default() }).unwrap();
        assert_eq!(plan.experiment, "demo-skill-description-ablation");
        assert_eq!(plan.sections.len(), 3);
        let exp = project.load_experiment(&plan.experiment).unwrap();
        let meta = exp.ablation.clone().unwrap();
        assert!(meta.by_sentence());
        assert_eq!(exp.trigger.as_ref().unwrap().names(), vec!["demo-skill"]);
        assert_eq!(exp.primary_metric.as_deref(), Some("trigger_f1"));
        assert_eq!(exp.tasks.include, vec!["tasks/trigger/demo-skill/*.toml"]);
        let (_, _, body) = split_frontmatter(skill);
        for (i, s) in meta.sections.iter().enumerate() {
            let text = std::fs::read_to_string(project.abs(&s.overlay).join(".claude/skills/demo/SKILL.md")).unwrap();
            let (_, fm, b) = split_frontmatter(&text);
            assert_eq!(b, body, "body must be byte-identical");
            assert_eq!(description_parts(fm).unwrap().sentences.len(), 2, "variant {i} drops exactly one sentence");
            assert!(fm.contains("name: demo-skill"));
        }
        assert_eq!(meta.sections[1].title, "Also for emails.");
        project.validate(&exp).unwrap();
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
        assert!(meta.placebo.is_none());

        // --placebo adds one variant: frontmatter kept, body replaced, no sibling files.
        let plan = generate(&project, &AblationRequest { source: "overlays/demo".into(), placebo: true, ..Default::default() }).unwrap();
        assert_eq!(plan.overlays.last().unwrap(), "overlays/ablation/demo-skill-ablation/placebo");
        let exp = project.load_experiment("demo-skill-ablation").unwrap();
        assert_eq!(exp.variants().last().unwrap().label, "placebo");
        assert_eq!(exp.ablation.as_ref().unwrap().placebo.as_deref(), Some("overlays/ablation/demo-skill-ablation/placebo"));
        let placebo = std::fs::read_to_string(base.join("placebo/.claude/skills/demo/SKILL.md")).unwrap();
        assert!(placebo.starts_with("---\nname: demo-skill\ndescription: Use when writing.\n---\n"));
        assert!(!placebo.contains("Body one") && !placebo.contains("Deeper"));
        assert!(!base.join("placebo/.claude/skills/demo/references").exists());
        // A user's own sentences.
        std::fs::write(tmp.path().join("mine.txt"), "Water is wet.\nThe sky is up.\n").unwrap();
        generate(&project, &AblationRequest { source: "overlays/demo".into(), placebo_file: Some("mine.txt".into()), ..Default::default() }).unwrap();
        assert!(std::fs::read_to_string(base.join("placebo/.claude/skills/demo/SKILL.md")).unwrap().contains("Water is wet."));
    }
}
