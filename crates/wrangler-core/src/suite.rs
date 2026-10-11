//! Tracked suites: experiments pinned as a regression suite, rerun per
//! model, with the launch for each model saved as that model's baseline.

use crate::model::*;
use crate::project::{Project, Suite};
use crate::report::Data;
use crate::scheduler::{self, EventSink, RunOptions};
use crate::stats::{self, Effect, Verdict};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The experiment as it will run for one model: the model factor (if any)
/// is collapsed to that model.
pub fn pin_model(exp: &Experiment, model: &str) -> Result<Experiment> {
    let mut e = exp.clone();
    if e.factors.iter().any(|f| f.kind == FactorKind::Runner && f.levels.iter().any(|l| l.extra("model").is_some())) {
        bail!("experiment `{}` sets models per runner level; a suite cannot pin one model across runners", e.name);
    }
    e.factors.retain(|f| f.kind != FactorKind::Model);
    e.runner.model = Some(model.to_string());
    overlay_factor(&e)?;
    Ok(e)
}

/// The factor whose levels are the tracked patterns, and its baseline label.
fn overlay_factor(exp: &Experiment) -> Result<(&Factor, String)> {
    let f = exp
        .factors
        .iter()
        .find(|f| f.kind == FactorKind::Overlay)
        .or_else(|| exp.factors.iter().find(|f| f.kind == FactorKind::SystemPromptAppend))
        .with_context(|| format!("experiment `{}` has no overlay factor, so there is no pattern to track", exp.name))?;
    if f.levels.len() < 2 {
        bail!("experiment `{}`: factor `{}` needs a baseline and at least one pattern", exp.name, f.name);
    }
    Ok((f, f.levels[0].label()))
}

/// Run every experiment in the suite on `model` and save the launches as
/// that model's baseline.
pub fn run_model(project: &Project, store: &Store, suite_name: &str, model: &str, opts: &RunOptions, cancel: Arc<AtomicBool>, sink: EventSink) -> Result<Vec<i64>> {
    let suite = project.load_suite(suite_name)?;
    let mut launches = vec![];
    // Validate everything before spending anything.
    let pinned: Vec<Experiment> = suite.experiments.iter().map(|n| pin_model(&project.load_experiment(n)?, model)).collect::<Result<_>>()?;
    for exp in &pinned {
        project.validate(exp).with_context(|| format!("suite experiment `{}`", exp.name))?;
    }
    for exp in &pinned {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (id, _) = scheduler::create_launch(project, store, exp, Some(suite_name), &format!("suite {suite_name} on {model}"))?;
        scheduler::execute(project, store, id, opts, cancel.clone(), sink)?;
        if store.launch(id)?.status == "done" {
            store.set_baseline(suite_name, model, &exp.name, id)?;
        }
        launches.push(id);
    }
    Ok(launches)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCell {
    pub model: String,
    pub launch_id: i64,
    pub created_at: String,
    /// Pattern vs no-pattern baseline on the suite's primary metric.
    pub effect: Option<Effect>,
    pub cost_effect: Option<Effect>,
    /// helping | neutral | harming | inconclusive | no_data
    pub verdict: String,
    /// Effect on this model minus effect on the previous model.
    pub change_vs_prev: Option<f64>,
    pub prev_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternRow {
    pub experiment: String,
    pub pattern: String,
    pub cells: Vec<ModelCell>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dashboard {
    pub suite: Suite,
    /// Models in the order their baselines were first recorded.
    pub models: Vec<String>,
    pub rows: Vec<PatternRow>,
    pub missing: Vec<String>,
}

fn verdict_word(e: &Option<Effect>) -> &'static str {
    match e.as_ref().map(|e| e.verdict) {
        None => "no_data",
        Some(Verdict::Better) | Some(Verdict::Higher) => "helping",
        Some(Verdict::Worse) | Some(Verdict::Lower) => "harming",
        Some(Verdict::Neutral) => "neutral",
        Some(Verdict::Inconclusive) => "inconclusive",
    }
}

pub fn dashboard(project: &Project, store: &Store, suite_name: &str) -> Result<Dashboard> {
    let suite = project.load_suite(suite_name)?;
    let baselines = store.baselines(suite_name)?;
    let mut models: Vec<String> = vec![];
    for b in &baselines {
        if !models.contains(&b.model) {
            models.push(b.model.clone());
        }
    }
    let mut rows: Vec<PatternRow> = vec![];
    let mut missing = vec![];
    for exp_name in &suite.experiments {
        let mut exp_rows: Vec<PatternRow> = vec![];
        for model in &models {
            let Some(b) = baselines.iter().find(|b| &b.model == model && &b.experiment == exp_name) else {
                missing.push(format!("{exp_name} has no baseline for {model}"));
                continue;
            };
            let Ok(data) = Data::load(store, b.launch_id) else {
                missing.push(format!("{exp_name} on {model}: launch #{} no longer exists", b.launch_id));
                continue;
            };
            let Ok((factor, base)) = overlay_factor(&data.launch.def) else { continue };
            for level in factor.levels.iter().skip(1).map(|l| l.label()) {
                let effect = data.level_effect(&suite.primary_metric, &factor.name, &base, &level);
                let cell = ModelCell {
                    model: model.clone(),
                    launch_id: b.launch_id,
                    created_at: b.created_at.clone(),
                    verdict: verdict_word(&effect).into(),
                    cost_effect: data.level_effect("cost_usd", &factor.name, &base, &level),
                    effect,
                    change_vs_prev: None,
                    prev_model: None,
                };
                match exp_rows.iter_mut().find(|r| r.pattern == level) {
                    Some(r) => r.cells.push(cell),
                    None => exp_rows.push(PatternRow { experiment: exp_name.clone(), pattern: level, cells: vec![cell] }),
                }
            }
        }
        rows.extend(exp_rows);
    }
    for row in &mut rows {
        for i in 1..row.cells.len() {
            let prev = row.cells[i - 1].clone();
            let cur = &mut row.cells[i];
            cur.prev_model = Some(prev.model.clone());
            if let (Some(a), Some(b)) = (&prev.effect, &cur.effect) {
                cur.change_vs_prev = Some(b.effect - a.effect);
            }
        }
    }
    Ok(Dashboard { suite, models, rows, missing })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffItem {
    pub experiment: String,
    pub pattern: String,
    /// regressed | redundant | inconclusive | no_effect | earning | new
    pub category: String,
    pub priority: u32,
    pub summary: String,
    pub new: ModelCell,
    pub prev: Option<ModelCell>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDiff {
    pub suite: String,
    pub primary_metric: String,
    pub new_model: String,
    pub prev_model: Option<String>,
    /// Prioritized: regressions first, then redundant patterns, then the rest.
    pub items: Vec<DiffItem>,
}

/// Compare a model's suite results with the previous model's stored baseline.
pub fn diff(project: &Project, store: &Store, suite_name: &str, new_model: &str, prev_model: Option<&str>) -> Result<ModelDiff> {
    let dash = dashboard(project, store, suite_name)?;
    let Some(new_idx) = dash.models.iter().position(|m| m == new_model) else {
        bail!("suite `{suite_name}` has no baseline for model `{new_model}`; run `wrangler suite run {suite_name} --model {new_model}` first");
    };
    let prev_model: Option<String> = match prev_model {
        Some(p) => {
            if !dash.models.iter().any(|m| m == p) {
                bail!("suite `{suite_name}` has no baseline for model `{p}`");
            }
            Some(p.to_string())
        }
        None => (new_idx > 0).then(|| dash.models[new_idx - 1].clone()),
    };
    let info = stats::metric_info(&dash.suite.primary_metric);
    let fmt = |e: &Option<Effect>| e.as_ref().map(|e| crate::report::fmt_effect(&info, e)).unwrap_or_else(|| "no data".into());
    let mut items = vec![];
    for row in &dash.rows {
        let Some(new) = row.cells.iter().find(|c| c.model == new_model).cloned() else { continue };
        let prev = prev_model.as_ref().and_then(|p| row.cells.iter().find(|c| &c.model == p).cloned());
        let band = new.effect.as_ref().map(|e| stats::equivalence_band(info.scale, e.base_mean)).unwrap_or(0.0);
        // Has the bare new model caught up with what the pattern used to deliver?
        let caught_up = match (&new.effect, prev.as_ref().and_then(|p| p.effect.as_ref())) {
            (Some(n), Some(p)) => match info.direction {
                stats::Direction::Lower => n.base_mean <= p.treat_mean + band,
                _ => n.base_mean >= p.treat_mean - band,
            },
            _ => false,
        };
        let prev_v = prev.as_ref().map(|p| p.verdict.as_str());
        let (category, priority) = match (new.verdict.as_str(), prev_v) {
            ("harming", _) => ("regressed", 1),
            ("neutral", Some("helping")) if caught_up => ("redundant", 3),
            ("neutral", Some("helping")) => ("regressed", 2),
            ("inconclusive", Some("helping")) => ("inconclusive", 4),
            ("inconclusive", _) | ("no_data", _) => ("inconclusive", 5),
            ("neutral", _) => ("no_effect", 6),
            ("helping", None) => ("new", 7),
            _ => ("earning", 8),
        };
        let was = match &prev {
            Some(p) => format!("{} on {}: {}", p.verdict, p.model, fmt(&p.effect)),
            None => "no previous baseline".to_string(),
        };
        let lead = match category {
            "regressed" if new.verdict == "harming" => "now hurts",
            "regressed" => "stopped helping",
            "redundant" => "now redundant: the model alone reaches what this pattern used to add",
            "inconclusive" => "cannot tell yet",
            "no_effect" => "no measurable effect",
            "new" => "helping (first baseline)",
            _ => "still earning its keep",
        };
        items.push(DiffItem {
            experiment: row.experiment.clone(),
            pattern: row.pattern.clone(),
            category: category.into(),
            priority,
            summary: format!("{lead}. {} on {new_model}: {}. Was {was}.", info.label, fmt(&new.effect)),
            new,
            prev,
        });
    }
    items.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.experiment.cmp(&b.experiment)).then(a.pattern.cmp(&b.pattern)));
    Ok(ModelDiff { suite: suite_name.to_string(), primary_metric: dash.suite.primary_metric.clone(), new_model: new_model.to_string(), prev_model, items })
}

pub fn render_dashboard_text(d: &Dashboard) -> String {
    let info = stats::metric_info(&d.suite.primary_metric);
    let mut s = format!("suite {} · primary metric: {} · {} experiment(s)\n", d.suite.name, info.label, d.suite.experiments.len());
    if d.models.is_empty() {
        s.push_str(&format!("no baselines yet. Run: wrangler suite run {} --model <model>\n", d.suite.name));
        return s;
    }
    for row in &d.rows {
        s.push_str(&format!("\n{} · {}\n", row.experiment, row.pattern));
        for c in &row.cells {
            let eff = c.effect.as_ref().map(|e| crate::report::fmt_effect(&info, e)).unwrap_or_else(|| "no data".into());
            let cost = c.cost_effect.as_ref().map(|e| format!(" · cost {}", crate::report::fmt_delta(&stats::metric_info("cost_usd"), e.effect))).unwrap_or_default();
            let change = c.change_vs_prev.map(|x| format!(" · vs {}: {}", c.prev_model.clone().unwrap_or_default(), crate::report::fmt_delta(&info, x))).unwrap_or_default();
            s.push_str(&format!("  {:<22} {:<12} {eff}{cost}{change}  (launch #{})\n", c.model, c.verdict, c.launch_id));
        }
    }
    for m in &d.missing {
        s.push_str(&format!("note: {m}\n"));
    }
    s
}

pub fn render_diff_text(d: &ModelDiff) -> String {
    let mut s = format!("suite {} · {} vs {}\n", d.suite, d.new_model, d.prev_model.clone().unwrap_or_else(|| "(no previous model)".into()));
    let mut last = "";
    for it in &d.items {
        if it.category != last {
            s.push_str(&format!("\n{}\n", it.category.replace('_', " ").to_uppercase()));
            last = &it.category;
        }
        s.push_str(&format!("  {} · {}: {}\n", it.experiment, it.pattern, it.summary));
    }
    s
}
