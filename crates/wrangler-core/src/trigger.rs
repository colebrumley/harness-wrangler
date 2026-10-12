//! Trigger corpora: draft should-fire and should-not-fire prompts for a
//! skill from its description. The drafts are a starting point the user
//! edits; they are written with a `draft` tag so `wrangler validate` lists them.

use crate::ablation::{description_parts, resolve_source, split_frontmatter};
use crate::graders::judge::{backend_for, JudgeBackend};
use crate::model::{JudgeCfg, Task};
use crate::project::Project;
use crate::util::slug;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct TriggerSet {
    pub skill: String,
    pub description: String,
    pub should: Vec<String>,
    pub should_not: Vec<String>,
    /// Task files written, relative to the project root.
    pub written: Vec<String>,
    pub cost_usd: f64,
}

/// Find a skill by path (a `SKILL.md`, a skill directory, or an overlay
/// holding one skill) or by name among the project's overlays and skill directories.
pub fn find_skill(project: &Project, skill: &str) -> Result<(String, String)> {
    let by_path = project.abs(skill).exists();
    let source = if by_path {
        skill.to_string()
    } else {
        let mut places = project.list_skill_dirs();
        places.extend(project.list_overlays());
        places
            .into_iter()
            .find(|p| project.overlay_skills(p).iter().any(|s| s.name == skill || s.dir == skill))
            .with_context(|| format!("no skill named `{skill}` in this project (give a path to its SKILL.md or directory)"))?
    };
    let src = resolve_source(project, &source)?;
    if src.kind != "skill" {
        bail!("`{skill}` is not a skill; a trigger set is drafted from a skill's description");
    }
    let text = std::fs::read_to_string(&src.file)?;
    let (_, fm, _) = split_frontmatter(&text);
    let description = description_parts(fm).map(|p| p.text()).with_context(|| format!("{} has no frontmatter description", src.file.display()))?;
    Ok((src.display_name, description))
}

pub fn prompt(skill: &str, description: &str, n: usize) -> String {
    format!(
        "A coding agent decides whether to load a skill from the skill's name and description alone, before it reads the skill's body. Draft a test corpus of user prompts for this skill.\n\n<skill_name>{skill}</skill_name>\n<description>\n{description}\n</description>\n\nReturn two lists of {n} prompts each.\n\n`should`: prompts on which the skill ought to fire. Vary them: different phrasings, lengths and sub-cases the description covers. Do not name the skill and do not quote the description; write what a person would actually type.\n\n`should_not`: prompts on which the skill ought to stay quiet. Every one must be a NEAR MISS: the same domain and the same vocabulary as the description, but the wrong moment for this skill, such as an adjacent task the description does not cover, or a question about the topic rather than a request to do it. Do not write random unrelated prompts; a prompt about cooking tells nothing about a writing skill.\n\nEach prompt is one to three sentences, self-contained, and needs no files."
    )
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {"should": {"type": "array", "items": {"type": "string"}}, "should_not": {"type": "array", "items": {"type": "string"}}},
        "required": ["should", "should_not"]
    })
}

/// Draft `n` prompts of each kind with `model` and write them under
/// `tasks/trigger/<skill>/`. Existing files are never overwritten.
pub fn generate(project: &Project, skill: &str, n: usize, model: &str, cancel: &Arc<AtomicBool>) -> Result<TriggerSet> {
    let (name, description) = find_skill(project, skill)?;
    let cfg = JudgeCfg { model: model.to_string(), effort: None, rubric: None, rubric_scoring: false, pairwise: false, pairwise_mode: None, pairwise_reps: None, transcript: None };
    let backend: Box<dyn JudgeBackend> = backend_for(&cfg);
    let reply = backend.complete(&prompt(&name, &description, n), &schema(), &cfg, cancel)?;
    let list = |key: &str| -> Vec<String> { reply.value[key].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(n).collect() };
    let (should, should_not) = (list("should"), list("should_not"));
    if should.is_empty() || should_not.is_empty() {
        bail!("the model returned no prompts for one of the lists; try again or another --model");
    }
    let dir = format!("tasks/trigger/{}", slug(&name));
    std::fs::create_dir_all(project.abs(&dir))?;
    let mut written = vec![];
    for (kind, prompts) in [("should", &should), ("should_not", &should_not)] {
        let stem = kind.replace('_', "-");
        let mut next = 1;
        for p in prompts {
            // Number past whatever is already there.
            let (rel, id) = loop {
                let rel = format!("{dir}/{stem}-{next:02}.toml");
                let id = format!("trigger-{}-{stem}-{next:02}", slug(&name));
                next += 1;
                if !project.abs(&rel).exists() {
                    break (rel, id);
                }
            };
            let task = Task { id, prompt: p.clone(), tags: vec!["trigger".into(), "draft".into()], max_turns: Some(2), trigger: Some(kind.to_string()), ..Default::default() };
            std::fs::write(project.abs(&rel), toml::to_string_pretty(&task)?)?;
            written.push(rel);
        }
    }
    Ok(TriggerSet { skill: name, description, should, should_not, written, cost_usd: reply.cost_usd })
}

/// An experiment file for a skill's trigger corpus: one overlay level (a
/// baseline without the skill can never fire), two turns, no judge.
pub fn experiment_template(skill: &str, overlay: &str, tasks_dir: &str) -> String {
    format!(
        "name = \"{0}-trigger\"\ndescription = \"Does {0} fire on the prompts it should, and stay quiet on the near misses?\"\nruns_per_cell = 3\ntimeout_secs = 300\nprimary_metric = \"trigger_f1\"\n\n[runner]\nkind = \"claude-code\"\nconcurrency = 3\nmax_turns = 2\n\n[trigger]\nskill = \"{skill}\"\n\n[[factors]]\nname = \"overlay\"\nkind = \"overlay\"\nlevels = [\"{overlay}\"]\n\n[[factors]]\nname = \"model\"\nkind = \"model\"\nlevels = [\"claude-haiku-5-5\", \"claude-sonnet-5-5\"]\n\n[tasks]\ninclude = [\"{tasks_dir}/*.toml\"]\n",
        slug(skill)
    )
}
