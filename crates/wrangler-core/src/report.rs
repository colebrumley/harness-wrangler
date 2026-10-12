//! The report model: everything the CLI and GUI show for one launch, computed
//! from the store on demand.

use crate::model::*;
use crate::stats::{self, Bout, BtItem, Contrast, Direction, Effect, Headroom, MetricInfo, Summary, TriggerTask, Verdict};
use crate::store::{Judgment, Launch, RunRow, Store};
use crate::util::hash_u64;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub timeout: usize,
    pub pending: usize,
    pub running: usize,
    pub cancelled: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorInfo {
    pub name: String,
    pub kind: String,
    pub levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantSummary {
    pub key: String,
    pub label: String,
    pub levels: BTreeMap<String, String>,
    pub runner: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub counts: Counts,
    pub metrics: BTreeMap<String, Summary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comparison {
    /// `factor`: one level against the factor's first level, paired by task
    /// and the other factors. `variant`: one variant against the first, paired by task.
    pub kind: String,
    pub factor: Option<String>,
    pub baseline: String,
    pub level: String,
    /// For `variant` comparisons, the variant key.
    pub variant_key: Option<String>,
    pub metric: String,
    #[serde(flatten)]
    pub effect: Effect,
}

/// The effect of one factor's level within a single level of another factor:
/// "what does the skill do on this model?" With `vs_first`, how that effect
/// differs from the same effect at the other factor's first level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conditional {
    pub factor: String,
    pub baseline: String,
    pub level: String,
    pub by_factor: String,
    pub by_level: String,
    pub metric: String,
    #[serde(flatten)]
    pub effect: Effect,
    pub vs_first: Option<Contrast>,
    pub first_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cell {
    pub variant_key: String,
    pub counts: Counts,
    pub metrics: BTreeMap<String, Summary>,
    /// Metrics on which this cell's spread marks the task as flaky.
    pub flaky: Vec<String>,
    pub run_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBreakdown {
    pub task_id: String,
    pub tags: Vec<String>,
    /// Room the baseline cell leaves on the primary metric: none, little or
    /// ok, and whether it sits at the ceiling or the floor.
    pub headroom: Headroom,
    /// The same for each quality metric that has a bound.
    pub headroom_by_metric: BTreeMap<String, Headroom>,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flaky {
    pub task_id: String,
    pub variant: String,
    pub metric: String,
    pub mean: f64,
    pub sd: f64,
    pub n: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairCount {
    pub a: String,
    pub b: String,
    pub a_wins: f64,
    pub b_wins: f64,
    pub ties: f64,
    pub both_bad: f64,
    /// Share of decided-or-tied votes going to `b` (ties count half), with a bootstrapped CI.
    pub b_rate: Option<Summary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ranking {
    /// judge | human | all
    pub source: String,
    pub votes: usize,
    /// Bradley–Terry strengths in variant order. Used when there are more than two variants.
    pub strengths: Vec<BtItem>,
    pub pairs: Vec<PairCount>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PositionBias {
    /// Pairs judged in both orders.
    pub pairs: usize,
    /// Pairs where the two orders disagreed.
    pub inconsistent: usize,
    pub inconsistent_rate: f64,
    /// Share of non-tie picks that went to whichever result was listed first. 0.5 is unbiased.
    pub first_position_rate: f64,
    pub decided_picks: usize,
}

/// One human vote, with the variants named and the reviewer's reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanVote {
    pub task_id: String,
    pub a: String,
    pub b: String,
    /// a | b | tie | both_bad
    pub winner: String,
    pub reason: String,
    pub run_a: i64,
    pub run_b: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pairwise {
    pub variants: Vec<String>,
    pub rankings: Vec<Ranking>,
    pub position_bias: Option<PositionBias>,
    /// Among run pairs judged by both a human and the judge, how often they agreed.
    pub human_judge_agreement: Option<f64>,
    pub human_judge_overlap: usize,
    /// Every human vote, newest first.
    #[serde(default)]
    pub human_votes: Vec<HumanVote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionAttribution {
    pub index: usize,
    pub title: String,
    pub overlay_label: String,
    pub est_tokens: u64,
    pub lines: (usize, usize),
    /// full − without-this-section on the primary metric. Positive means the section contributes.
    pub effect: Option<Effect>,
    /// carries | hurts | no_effect | inconclusive | no_headroom | no_data
    pub attribution: String,
    /// full − without-this-section on cost and tokens: what the section costs per run.
    pub cost_effect: Option<Effect>,
    pub tokens_effect: Option<Effect>,
    /// Description ablations only: full − without-this-sentence on trigger
    /// precision and recall. A negative precision effect means the sentence
    /// makes the skill fire where it should not.
    #[serde(default)]
    pub precision_effect: Option<Effect>,
    #[serde(default)]
    pub recall_effect: Option<Effect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AblationReport {
    pub source: String,
    pub kind: String,
    /// `sections` or `description_sentences`.
    #[serde(default)]
    pub mode: String,
    pub metric: String,
    /// full − none.
    pub full_effect: Option<Effect>,
    /// description-only − none: what triggering alone buys.
    pub description_only_effect: Option<Effect>,
    /// full − description-only: what the body adds beyond the description.
    pub body_effect: Option<Effect>,
    pub skill_invoked_rate: Option<f64>,
    /// Present when the ablation carries a placebo variant.
    #[serde(default)]
    pub placebo: Option<PlaceboReport>,
    pub sections: Vec<SectionAttribution>,
}

/// Is the gain from these words, or from having words at all?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceboReport {
    /// full − placebo.
    pub full_vs_placebo: Option<Effect>,
    /// placebo − none.
    pub placebo_vs_none: Option<Effect>,
    /// content: full beats placebo. presence: placebo matches full and beats
    /// none. neither: nothing beats none. Otherwise inconclusive.
    pub attribution: String,
}

/// Whole-file attribution from the three comparisons.
pub fn placebo_attribution(full_vs_none: Option<&Effect>, full_vs_placebo: Option<&Effect>, placebo_vs_none: Option<&Effect>) -> &'static str {
    let v = |e: Option<&Effect>| e.map(|e| e.verdict);
    let beats = |e: Option<&Effect>| matches!(v(e), Some(Verdict::Better) | Some(Verdict::Higher));
    let not_better = |e: Option<&Effect>| matches!(v(e), Some(Verdict::Neutral) | Some(Verdict::Worse) | Some(Verdict::Lower));
    if beats(full_vs_placebo) {
        "content"
    } else if v(full_vs_placebo) == Some(Verdict::Neutral) && beats(placebo_vs_none) {
        "presence"
    } else if not_better(full_vs_none) && not_better(placebo_vs_none) {
        "neither"
    } else {
        "inconclusive"
    }
}

/// A trigger prompt that went the wrong way in one variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerPrompt {
    pub task_id: String,
    pub prompt: String,
    /// Share of runs in which the skill fired.
    pub fire_rate: f64,
    pub runs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerVariant {
    pub variant_key: String,
    pub label: String,
    pub model: Option<String>,
    /// Undefined (`None`) when the skill never fired in this variant.
    pub precision: Option<Summary>,
    pub recall: Option<Summary>,
    pub f1: Option<Summary>,
    pub should_tasks: usize,
    pub should_not_tasks: usize,
    /// Should-fire prompts on which the skill never fired.
    pub false_negatives: Vec<TriggerPrompt>,
    /// Should-not prompts on which it fired at least once.
    pub false_positives: Vec<TriggerPrompt>,
    /// The skill's description as it was when the launch was created.
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerModel {
    pub model: String,
    pub precision: Option<Summary>,
    pub recall: Option<Summary>,
    pub f1: Option<Summary>,
}

/// Triggering as a classifier: does the skill fire on the prompts it should
/// and stay quiet on the near misses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerReport {
    pub skills: Vec<String>,
    pub variants: Vec<TriggerVariant>,
    /// Variants that install the skill, pooled by model. Empty without a model factor.
    pub by_model: Vec<TriggerModel>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventCounts {
    pub rate_limited_runs: usize,
    pub retries: usize,
    pub timeouts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub launch_id: i64,
    pub experiment: String,
    pub description: String,
    pub status: String,
    pub phase: String,
    pub created_at: String,
    pub def_hash: String,
    pub runs_per_cell: u32,
    pub counts: Counts,
    pub cost_usd: f64,
    pub judge_cost_usd: f64,
    pub primary_metric: String,
    pub metrics: Vec<MetricInfo>,
    pub factors: Vec<FactorInfo>,
    pub variants: Vec<VariantSummary>,
    pub comparisons: Vec<Comparison>,
    /// Each factor's effect within each level of every other factor.
    pub conditional: Vec<Conditional>,
    pub tasks: Vec<TaskBreakdown>,
    pub flaky: Vec<Flaky>,
    /// Set when cells have too few runs for flakiness to be judged.
    pub flaky_note: Option<String>,
    pub pairwise: Option<Pairwise>,
    pub ablation: Option<AblationReport>,
    pub trigger: Option<TriggerReport>,
    /// How far the judge can be trusted here: its agreement with human
    /// votes on the same pairs. Present when the launch has a judge.
    pub calibration: Option<crate::calibration::Calibration>,
    pub events: EventCounts,
    pub warnings: Vec<String>,
}

fn count(runs: &[&RunRow]) -> Counts {
    let mut c = Counts { total: runs.len(), ..Default::default() };
    for r in runs {
        match r.status.as_str() {
            "done" => c.done += 1,
            "failed" => c.failed += 1,
            "timeout" => c.timeout += 1,
            "pending" => c.pending += 1,
            "running" => c.running += 1,
            _ => c.cancelled += 1,
        }
    }
    c
}

/// Loaded launch data with the lookups the analyses share.
pub struct Data {
    pub launch: Launch,
    pub runs: Vec<RunRow>,
    pub metrics: HashMap<i64, BTreeMap<String, f64>>,
}

impl Data {
    pub fn load(store: &Store, launch_id: i64) -> Result<Data> {
        Ok(Data { launch: store.launch(launch_id)?, runs: store.runs(launch_id)?, metrics: store.metrics_for_launch(launch_id)? })
    }

    fn values(&self, metric: &str, pred: &dyn Fn(&RunRow) -> bool) -> Vec<f64> {
        self.runs.iter().filter(|r| pred(r)).filter_map(|r| self.metrics.get(&r.id)?.get(metric).copied()).collect()
    }

    fn variant(&self, key: &str) -> Option<&Variant> {
        self.launch.variants.iter().find(|v| v.key == key)
    }

    /// Fire records of the trigger tasks among the runs matching `pred`,
    /// one per pairing unit.
    pub fn trigger_tasks(&self, pred: &dyn Fn(&RunRow) -> bool, unit: &dyn Fn(&RunRow) -> String) -> BTreeMap<String, TriggerTask> {
        let mut out: BTreeMap<String, TriggerTask> = BTreeMap::new();
        for r in self.runs.iter().filter(|r| pred(r)) {
            let Some(should) = self.launch.tasks.iter().find(|t| t.id == r.task_id).and_then(|t| t.should_trigger()) else { continue };
            let Some(fired) = self.metrics.get(&r.id).and_then(|m| m.get("trigger_fired")).copied() else { continue };
            out.entry(unit(r)).or_insert_with(|| TriggerTask { should, fired: vec![] }).fired.push(fired);
        }
        out
    }

    fn trigger_summary(&self, metric: &str, pred: &dyn Fn(&RunRow) -> bool, seed: u64) -> Option<Summary> {
        let tasks: Vec<TriggerTask> = self.trigger_tasks(pred, &|r| r.task_id.clone()).into_values().collect();
        stats::trigger_summary(&tasks, metric, seed)
    }

    pub fn available_metrics(&self) -> Vec<String> {
        let present: BTreeSet<&str> = self.metrics.values().flat_map(|m| m.keys().map(|k| k.as_str())).collect();
        let mut out: Vec<String> = stats::HEADLINE.iter().filter(|m| present.contains(**m)).map(|s| s.to_string()).collect();
        if present.contains("trigger_fired") {
            let at = out.iter().position(|m| m == "trigger_fired").unwrap_or(0);
            for (i, m) in stats::TRIGGER_METRICS.iter().enumerate() {
                out.insert(at + i, m.to_string());
            }
        }
        // Per-check pass rates, when a task has more than one check to tell apart.
        let checks: Vec<&str> = present.iter().copied().filter(|m| m.starts_with("check.")).collect();
        if checks.len() > 1 {
            out.extend(checks.iter().map(|s| s.to_string()));
        }
        if present.contains("mock_quality") {
            out.push("mock_quality".into());
        }
        out
    }

    pub fn primary_metric(&self) -> String {
        if let Some(m) = &self.launch.def.primary_metric {
            return m.clone();
        }
        let avail = self.available_metrics();
        if self.launch.def.trigger.is_some() && avail.iter().any(|a| a == "trigger_f1") {
            return "trigger_f1".into();
        }
        for m in ["judge_score", "pass", "expectations_passed", "checks_passed"] {
            if avail.iter().any(|a| a == m) {
                return m.to_string();
            }
        }
        avail.first().cloned().unwrap_or_else(|| "cost_usd".into())
    }

    /// Paired effect of the runs matching `treat` against those matching
    /// `control`, pairing on the unit key (task, plus whatever else the
    /// caller holds fixed).
    pub fn effect(&self, metric: &str, control: &dyn Fn(&RunRow) -> bool, treat: &dyn Fn(&RunRow) -> bool, unit: &dyn Fn(&RunRow) -> String) -> Option<Effect> {
        // Precision, recall and F1 are properties of a set of tasks, not of
        // a run: both arms are scored on the same resampled tasks.
        if stats::TRIGGER_METRICS.contains(&metric) {
            let (mut a, b) = (self.trigger_tasks(control, unit), self.trigger_tasks(treat, unit));
            let units: Vec<(TriggerTask, TriggerTask)> = b.into_iter().filter_map(|(k, tb)| Some((a.remove(&k)?, tb))).collect();
            return stats::trigger_effect(&units, metric, hash_u64(&format!("{}|{metric}", self.launch.id)));
        }
        let units = self.units(metric, control, treat, unit);
        stats::paired_effect(&units, &stats::metric_info(metric), hash_u64(&format!("{}|{metric}", self.launch.id)))
    }

    /// Control and treatment values grouped by pairing unit.
    pub fn units(&self, metric: &str, control: &dyn Fn(&RunRow) -> bool, treat: &dyn Fn(&RunRow) -> bool, unit: &dyn Fn(&RunRow) -> String) -> Vec<(Vec<f64>, Vec<f64>)> {
        let mut units: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
        for r in &self.runs {
            let Some(v) = self.metrics.get(&r.id).and_then(|m| m.get(metric)).copied() else { continue };
            if control(r) {
                units.entry(unit(r)).or_default().0.push(v);
            } else if treat(r) {
                units.entry(unit(r)).or_default().1.push(v);
            }
        }
        units.into_values().collect()
    }

    /// Units for one factor level against its baseline, within one level of
    /// another factor, paired by task and whatever factors remain.
    pub fn conditional_units(&self, metric: &str, factor: &str, base: &str, level: &str, by: &str, by_level: &str) -> Vec<(Vec<f64>, Vec<f64>)> {
        let label = |r: &RunRow, f: &str| self.variant(&r.variant_key).and_then(|v| v.level_label(f)).map(|s| s.to_string());
        let rest = |r: &RunRow| {
            let others = self.variant(&r.variant_key).map(|v| v.levels.iter().filter(|(n, _)| n != factor && n != by).map(|(n, l)| format!("{n}={}", l.label)).collect::<Vec<_>>().join("|")).unwrap_or_default();
            format!("{}#{others}", r.task_id)
        };
        let here = |r: &RunRow| label(r, by).as_deref() == Some(by_level);
        self.units(metric, &|r| here(r) && label(r, factor).as_deref() == Some(base), &|r| here(r) && label(r, factor).as_deref() == Some(level), &rest)
    }

    /// Effect of one overlay-factor level against another, paired by task
    /// and the remaining factors.
    pub fn level_effect(&self, metric: &str, factor: &str, base_label: &str, level_label: &str) -> Option<Effect> {
        let label_of = |r: &RunRow| self.variant(&r.variant_key).and_then(|v| v.level_label(factor)).map(|s| s.to_string());
        let rest = |r: &RunRow| {
            let others = self.variant(&r.variant_key).map(|v| v.levels.iter().filter(|(n, _)| n != factor).map(|(n, l)| format!("{n}={}", l.label)).collect::<Vec<_>>().join("|")).unwrap_or_default();
            format!("{}#{others}", r.task_id)
        };
        self.effect(metric, &|r| label_of(r).as_deref() == Some(base_label), &|r| label_of(r).as_deref() == Some(level_label), &rest)
    }
}

fn level_label_for_value(exp: &Experiment, factor: &str, value: &str) -> Option<String> {
    exp.factors.iter().find(|f| f.name == factor)?.levels.iter().find(|l| l.value() == value).map(|l| l.label())
}

pub fn build(store: &Store, launch_id: i64, metric_override: Option<&str>) -> Result<Report> {
    let data = Data::load(store, launch_id)?;
    let launch = &data.launch;
    let exp = &launch.def;
    let metric_names = data.available_metrics();
    let primary = metric_override.map(|s| s.to_string()).unwrap_or_else(|| data.primary_metric());
    let mut warnings = vec![];
    if metric_override.is_some() && !data.metrics.values().any(|m| m.contains_key(&primary)) {
        warnings.push(format!("no run has metric `{primary}`"));
    }
    let seed = |s: &str| hash_u64(&format!("{launch_id}|{s}"));
    let all: Vec<&RunRow> = data.runs.iter().collect();

    let summaries = |pred: &dyn Fn(&RunRow) -> bool, tag: &str| -> BTreeMap<String, Summary> {
        metric_names
            .iter()
            .filter_map(|m| {
                let s = if stats::TRIGGER_METRICS.contains(&m.as_str()) { data.trigger_summary(m, pred, seed(&format!("{tag}|{m}"))) } else { stats::summarize(&data.values(m, pred), seed(&format!("{tag}|{m}"))) };
                s.map(|s| (m.clone(), s))
            })
            .collect()
    };

    let variants: Vec<VariantSummary> = launch
        .variants
        .iter()
        .map(|v| VariantSummary {
            key: v.key.clone(),
            label: v.label.clone(),
            levels: v.levels.iter().map(|(n, l)| (n.clone(), l.label.clone())).collect(),
            runner: v.runner.clone(),
            model: v.model.clone(),
            effort: v.effort.clone(),
            counts: count(&all.iter().copied().filter(|r| r.variant_key == v.key).collect::<Vec<_>>()),
            metrics: summaries(&|r| r.variant_key == v.key, &v.key),
        })
        .collect();

    let mut comparisons = vec![];
    for f in &exp.factors {
        let labels: Vec<String> = f.levels.iter().map(|l| l.label()).collect();
        for level in labels.iter().skip(1) {
            for m in &metric_names {
                if let Some(effect) = data.level_effect(m, &f.name, &labels[0], level) {
                    comparisons.push(Comparison { kind: "factor".into(), factor: Some(f.name.clone()), baseline: labels[0].clone(), level: level.clone(), variant_key: None, metric: m.clone(), effect });
                }
            }
        }
    }
    if exp.factors.len() > 1 {
        let base = &launch.variants[0];
        for v in launch.variants.iter().skip(1) {
            for m in &metric_names {
                if let Some(effect) = data.effect(m, &|r| r.variant_key == base.key, &|r| r.variant_key == v.key, &|r| r.task_id.clone()) {
                    comparisons.push(Comparison { kind: "variant".into(), factor: None, baseline: base.label.clone(), level: v.label.clone(), variant_key: Some(v.key.clone()), metric: m.clone(), effect });
                }
            }
        }
    }

    // Does a factor behave differently across the levels of another factor?
    let mut conditional = vec![];
    let cond_metrics: Vec<String> = metric_names.iter().filter(|m| **m == primary || ["pass", "judge_score", "expectations_passed", "cost_usd", "wall_time_s"].contains(&m.as_str())).cloned().collect();
    for f in &exp.factors {
        let f_labels: Vec<String> = f.levels.iter().map(|l| l.label()).collect();
        for g in exp.factors.iter().filter(|g| g.name != f.name) {
            let g_labels: Vec<String> = g.levels.iter().map(|l| l.label()).collect();
            for level in f_labels.iter().skip(1) {
                for m in &cond_metrics {
                    let info = stats::metric_info(m);
                    let first_units = data.conditional_units(m, &f.name, &f_labels[0], level, &g.name, &g_labels[0]);
                    for (gi, g_level) in g_labels.iter().enumerate() {
                        let units = if gi == 0 { first_units.clone() } else { data.conditional_units(m, &f.name, &f_labels[0], level, &g.name, g_level) };
                        let tag = format!("cond|{}|{level}|{}|{g_level}|{m}", f.name, g.name);
                        let Some(effect) = stats::paired_effect(&units, &info, seed(&tag)) else { continue };
                        let vs_first = if gi == 0 { None } else { stats::effect_difference(&first_units, &units, seed(&format!("{tag}|diff"))) };
                        conditional.push(Conditional { factor: f.name.clone(), baseline: f_labels[0].clone(), level: level.clone(), by_factor: g.name.clone(), by_level: g_level.clone(), metric: m.clone(), effect, vs_first, first_level: g_labels[0].clone() });
                    }
                }
            }
        }
    }

    let mut flaky = vec![];
    let mut tasks = vec![];
    for t in &launch.tasks {
        let mut cells = vec![];
        for v in &launch.variants {
            let in_cell = |r: &RunRow| r.task_id == t.id && r.variant_key == v.key;
            let cell_runs: Vec<&RunRow> = all.iter().copied().filter(|r| in_cell(r)).collect();
            let mut cell_flaky = vec![];
            for m in &metric_names {
                let info = stats::metric_info(m);
                // One flag per underlying signal: the headline quality metrics only.
                if info.direction != Direction::Higher || !(m == "pass" || m == "judge_score" || *m == primary) {
                    continue;
                }
                let xs = data.values(m, &in_cell);
                if stats::is_flaky(&xs, info.scale) {
                    cell_flaky.push(m.clone());
                    flaky.push(Flaky { task_id: t.id.clone(), variant: v.label.clone(), metric: m.clone(), mean: stats::mean(&xs), sd: stats::sd(&xs), n: xs.len() });
                }
            }
            cells.push(Cell {
                variant_key: v.key.clone(),
                counts: count(&cell_runs),
                metrics: summaries(&in_cell, &format!("{}|{}", t.id, v.key)),
                flaky: cell_flaky,
                run_ids: cell_runs.iter().map(|r| r.id).collect(),
            });
        }
        // Headroom is read off the baseline cell: the first variant.
        let base_key = launch.variants.first().map(|v| v.key.as_str()).unwrap_or("");
        let room = |m: &str| stats::saturation(&data.values(m, &|r| r.task_id == t.id && r.variant_key == base_key), &stats::metric_info(m));
        let headroom_by_metric: BTreeMap<String, Headroom> = metric_names.iter().filter(|m| stats::metric_info(m).scale != stats::Scale::Positive && stats::metric_info(m).direction == Direction::Higher).map(|m| (m.clone(), room(m))).collect();
        tasks.push(TaskBreakdown { task_id: t.id.clone(), tags: t.tags.clone(), headroom: room(&primary), headroom_by_metric, cells });
    }
    let small_cells = tasks.iter().flat_map(|t| &t.cells).filter(|c| c.counts.done >= 2 && c.counts.done < stats::FLAKY_MIN_N).count();
    let flaky_note = (small_cells > 0).then(|| format!("too few runs to judge flakiness ({small_cells} cell(s) with fewer than {} runs)", stats::FLAKY_MIN_N));
    let saturated: Vec<&TaskBreakdown> = tasks.iter().filter(|t| t.headroom.is_none()).collect();
    if !saturated.is_empty() {
        let pinfo = stats::metric_info(&primary);
        let at = |w: &str| saturated.iter().filter(|t| t.headroom.at.as_deref() == Some(w)).map(|t| t.task_id.as_str()).collect::<Vec<_>>();
        for (side, what) in [("ceiling", "already at the top"), ("floor", "at the bottom")] {
            let ids = at(side);
            if !ids.is_empty() {
                warnings.push(format!("no headroom on {} for {} of {} task(s): the baseline is {what} with no variance ({}). Comparisons leave a task out when the other variant sits at the same bound.", pinfo.label.to_lowercase(), ids.len(), tasks.len(), ids.join(", ")));
            }
        }
    }

    let mut events = EventCounts::default();
    for r in &data.runs {
        let evs = r.events.as_array().cloned().unwrap_or_default();
        let retries = evs.iter().filter(|e| e["kind"] == "retry").count();
        if evs.iter().any(|e| e["kind"] == "rate_limit") {
            events.rate_limited_runs += 1;
        }
        events.retries += retries;
        if r.status == "timeout" {
            events.timeouts += 1;
        }
    }
    let counts = count(&all);
    if launch.status == "budget" {
        warnings.push(format!("stopped at the ${:.2} budget with {} run(s) not started; resume with a higher cap (`wrangler resume {} --budget <usd>`) to continue", exp.budget_usd.unwrap_or(0.0), counts.pending, launch_id));
    }
    if counts.failed > 0 {
        warnings.push(format!("{} run(s) failed and are excluded from quality metrics", counts.failed));
    }
    if counts.timeout > 0 {
        warnings.push(format!("{} run(s) timed out", counts.timeout));
    }
    if events.rate_limited_runs > 0 {
        warnings.push(format!("{} run(s) hit a rate limit ({} retries); their wall time excludes backoff waits", events.rate_limited_runs, events.retries));
    }

    let judgments = store.judgments(launch_id, None)?;
    let pairwise = build_pairwise(&data, &judgments, launch_id);
    if let Some(pb) = pairwise.as_ref().and_then(|p| p.position_bias.as_ref()) {
        if pb.pairs >= 6 && pb.inconsistent_rate > 0.3 {
            warnings.push(format!("pairwise judge disagreed with itself across orders on {:.0}% of pairs; treat its votes with care", pb.inconsistent_rate * 100.0));
        }
    }
    let ablation = exp.ablation.as_ref().map(|meta| build_ablation(&data, meta, &primary));
    let trigger = build_trigger(&data, launch_id);
    let calibration = exp.judge().map(|cfg| {
        let pairs = crate::calibration::pairs_of(launch, &data.runs, &judgments, &data.metrics, None);
        crate::calibration::from_pairs(&pairs, Some(cfg.model.clone()), cfg.rubric.clone(), seed("calibration"))
    });
    let sum = |name: &str| data.metrics.values().filter_map(|m| m.get(name)).sum::<f64>() + 0.0;

    Ok(Report {
        launch_id,
        experiment: launch.experiment.clone(),
        description: exp.description.clone(),
        status: launch.status.clone(),
        phase: launch.phase.clone(),
        created_at: launch.created_at.clone(),
        def_hash: launch.def_hash.clone(),
        runs_per_cell: exp.runs_per_cell,
        counts,
        cost_usd: sum("cost_usd"),
        judge_cost_usd: sum("judge_cost_usd") + judgments.iter().filter_map(|j| j.detail["cost_usd"].as_f64()).sum::<f64>() + 0.0,
        primary_metric: primary,
        metrics: metric_names.iter().map(|m| stats::metric_info(m)).collect(),
        factors: exp.factors.iter().map(|f| FactorInfo { name: f.name.clone(), kind: f.kind.as_str().into(), levels: f.levels.iter().map(|l| l.label()).collect() }).collect(),
        variants,
        comparisons,
        conditional,
        tasks,
        flaky,
        flaky_note,
        pairwise,
        ablation,
        trigger,
        calibration,
        events,
        warnings,
    })
}

fn build_trigger(data: &Data, launch_id: i64) -> Option<TriggerReport> {
    let launch = &data.launch;
    let skills = launch.def.trigger.as_ref()?.names();
    if !data.metrics.values().any(|m| m.contains_key("trigger_fired")) {
        return None;
    }
    let prompt_of = |id: &str| launch.tasks.iter().find(|t| t.id == id).map(|t| t.prompt.clone()).unwrap_or_default();
    let stat = |m: &str, pred: &dyn Fn(&RunRow) -> bool, tag: &str| data.trigger_summary(m, pred, hash_u64(&format!("{launch_id}|trigger|{tag}|{m}")));
    let installs = |v: &Variant| v.skills.iter().any(|s| skills.contains(&s.name) || skills.contains(&s.dir));
    let variants: Vec<TriggerVariant> = launch
        .variants
        .iter()
        .map(|v| {
            let mine = |r: &RunRow| r.variant_key == v.key;
            let tasks = data.trigger_tasks(&mine, &|r| r.task_id.clone());
            let wrong = |should: bool| -> Vec<TriggerPrompt> {
                tasks
                    .iter()
                    .filter(|(_, t)| t.should == should && !t.fired.is_empty())
                    .map(|(id, t)| (id, stats::mean(&t.fired), t.fired.len()))
                    .filter(|(_, rate, _)| if should { *rate == 0.0 } else { *rate > 0.0 })
                    .map(|(id, rate, runs)| TriggerPrompt { task_id: id.clone(), prompt: prompt_of(id), fire_rate: rate, runs })
                    .collect()
            };
            TriggerVariant {
                variant_key: v.key.clone(),
                label: v.label.clone(),
                model: v.model.clone(),
                precision: stat("trigger_precision", &mine, &v.key),
                recall: stat("trigger_recall", &mine, &v.key),
                f1: stat("trigger_f1", &mine, &v.key),
                should_tasks: tasks.values().filter(|t| t.should).count(),
                should_not_tasks: tasks.values().filter(|t| !t.should).count(),
                false_negatives: wrong(true),
                false_positives: wrong(false),
                description: v.skills.iter().find(|s| skills.contains(&s.name) || skills.contains(&s.dir)).map(|s| s.description.clone()),
            }
        })
        .collect();
    let mut by_model = vec![];
    if launch.def.factors.iter().any(|f| f.kind == FactorKind::Model) {
        // Pool the variants that install the skill; one with no skill cannot fire and would only dilute recall.
        let any_installed = launch.variants.iter().any(installs);
        let mut models: Vec<String> = vec![];
        for v in &launch.variants {
            if let Some(m) = &v.model {
                if !models.contains(m) {
                    models.push(m.clone());
                }
            }
        }
        for model in models {
            let keys: Vec<&str> = launch.variants.iter().filter(|v| v.model.as_deref() == Some(model.as_str()) && (!any_installed || installs(v))).map(|v| v.key.as_str()).collect();
            let pred = |r: &RunRow| keys.contains(&r.variant_key.as_str());
            let pooled: Vec<TriggerTask> = data.trigger_tasks(&pred, &|r| format!("{}#{}", r.task_id, r.variant_key)).into_values().collect();
            let s = |m: &str| stats::trigger_summary(&pooled, m, hash_u64(&format!("{launch_id}|trigger-model|{model}|{m}")));
            by_model.push(TriggerModel { precision: s("trigger_precision"), recall: s("trigger_recall"), f1: s("trigger_f1"), model });
        }
    }
    Some(TriggerReport { skills, variants, by_model })
}

fn build_pairwise(data: &Data, judgments: &[Judgment], launch_id: i64) -> Option<Pairwise> {
    let judgments: Vec<&Judgment> = judgments.iter().filter(|j| j.winner != "skip").collect();
    if judgments.is_empty() {
        return None;
    }
    let variants = &data.launch.variants;
    let vidx: HashMap<i64, usize> = data.runs.iter().filter_map(|r| Some((r.id, variants.iter().position(|v| v.key == r.variant_key)?))).collect();
    let mut rankings = vec![];
    for source in ["judge", "human", "all"] {
        let js: Vec<&&Judgment> = judgments.iter().filter(|j| source == "all" || j.source == source).collect();
        if js.is_empty() || (source == "all" && rankings.len() < 2) {
            continue;
        }
        let mut bouts = vec![];
        let mut pair_scores: BTreeMap<(usize, usize), (Vec<f64>, f64)> = BTreeMap::new();
        for j in &js {
            let (Some(&ia), Some(&ib)) = (vidx.get(&j.run_a), vidx.get(&j.run_b)) else { continue };
            if ia == ib {
                continue;
            }
            // Orient every vote so `lo` is the earlier variant.
            let (lo, hi, lo_is_a) = if ia < ib { (ia, ib, true) } else { (ib, ia, false) };
            let entry = pair_scores.entry((lo, hi)).or_default();
            let score_lo = match (j.winner.as_str(), lo_is_a) {
                ("a", true) | ("b", false) => 1.0,
                ("b", true) | ("a", false) => 0.0,
                ("both_bad", _) => {
                    entry.1 += 1.0;
                    continue;
                }
                _ => 0.5,
            };
            entry.0.push(1.0 - score_lo);
            bouts.push(Bout { i: lo, j: hi, score_i: score_lo });
        }
        let strengths = stats::bradley_terry_ci(variants.len(), &bouts, hash_u64(&format!("{launch_id}|bt|{source}")));
        let pairs = pair_scores
            .into_iter()
            .map(|((lo, hi), (hi_scores, both_bad))| PairCount {
                a: variants[lo].label.clone(),
                b: variants[hi].label.clone(),
                a_wins: hi_scores.iter().filter(|s| **s == 0.0).count() as f64,
                b_wins: hi_scores.iter().filter(|s| **s == 1.0).count() as f64,
                ties: hi_scores.iter().filter(|s| **s == 0.5).count() as f64,
                both_bad,
                b_rate: stats::win_rate(&hi_scores, hash_u64(&format!("{launch_id}|wr|{source}|{lo}|{hi}"))),
            })
            .collect();
        rankings.push(Ranking { source: source.into(), votes: bouts.len(), strengths, pairs });
    }

    let judge: Vec<&&Judgment> = judgments.iter().filter(|j| j.source == "judge" && j.consistent.is_some()).collect();
    let position_bias = (!judge.is_empty()).then(|| {
        let inconsistent = judge.iter().filter(|j| j.consistent == Some(false)).count();
        let picks: Vec<&str> = judge.iter().flat_map(|j| [j.detail["first_pick"].as_str().unwrap_or("tie"), j.detail["second_pick"].as_str().unwrap_or("tie")]).filter(|p| *p != "tie").collect();
        let first = picks.iter().filter(|p| **p == "A").count();
        PositionBias {
            pairs: judge.len(),
            inconsistent,
            inconsistent_rate: inconsistent as f64 / judge.len() as f64,
            first_position_rate: if picks.is_empty() { 0.5 } else { first as f64 / picks.len() as f64 },
            decided_picks: picks.len(),
        }
    });

    // Agreement between human and judge on the same run pair, in either orientation.
    let norm = |j: &Judgment| -> ((i64, i64), &str) {
        if j.run_a <= j.run_b {
            ((j.run_a, j.run_b), match j.winner.as_str() { "a" => "lo", "b" => "hi", _ => "tie" })
        } else {
            ((j.run_b, j.run_a), match j.winner.as_str() { "a" => "hi", "b" => "lo", _ => "tie" })
        }
    };
    let judge_votes: HashMap<(i64, i64), &str> = judgments.iter().filter(|j| j.source == "judge").map(|j| norm(j)).collect();
    let (mut overlap, mut agree) = (0usize, 0usize);
    for j in judgments.iter().filter(|j| j.source == "human") {
        let (k, v) = norm(j);
        if let Some(jv) = judge_votes.get(&k) {
            overlap += 1;
            if *jv == v {
                agree += 1;
            }
        }
    }
    let human_votes = judgments
        .iter()
        .rev()
        .filter(|j| j.source == "human")
        .filter_map(|j| {
            let (ia, ib) = (*vidx.get(&j.run_a)?, *vidx.get(&j.run_b)?);
            Some(HumanVote {
                task_id: j.task_id.clone(),
                a: variants[ia].label.clone(),
                b: variants[ib].label.clone(),
                winner: j.winner.clone(),
                reason: j.reason.clone(),
                run_a: j.run_a,
                run_b: j.run_b,
                created_at: j.created_at.clone(),
            })
        })
        .collect();
    Some(Pairwise {
        variants: variants.iter().map(|v| v.label.clone()).collect(),
        rankings,
        position_bias,
        human_judge_agreement: (overlap > 0).then(|| agree as f64 / overlap as f64),
        human_judge_overlap: overlap,
        human_votes,
    })
}

fn build_ablation(data: &Data, meta: &AblationMeta, metric: &str) -> AblationReport {
    let exp = &data.launch.def;
    let label = |value: &str| level_label_for_value(exp, &meta.factor, value).unwrap_or_else(|| value.to_string());
    let (none, full) = (label(&meta.baseline), label(&meta.full));
    let eff = |m: &str, base: &str, treat: &str| data.level_effect(m, &meta.factor, base, treat);
    let desc = meta.description_only.as_ref().map(|d| label(d));
    let invoked = data.values("skill_invoked", &|r| data.variant(&r.variant_key).and_then(|v| v.level_label(&meta.factor)) == Some(full.as_str()));
    let sections = meta
        .sections
        .iter()
        .map(|s| {
            let without = label(&s.overlay);
            let effect = eff(metric, &without, &full);
            let attribution = match effect.as_ref().map(|e| e.verdict) {
                None => "no_data",
                Some(Verdict::Better) | Some(Verdict::Higher) => "carries",
                Some(Verdict::Worse) | Some(Verdict::Lower) => "hurts",
                Some(Verdict::Neutral) => "no_effect",
                Some(Verdict::Inconclusive) => "inconclusive",
                Some(Verdict::NoHeadroom) => "no_headroom",
            };
            SectionAttribution {
                index: s.index,
                title: s.title.clone(),
                overlay_label: without.clone(),
                est_tokens: s.est_tokens,
                lines: s.lines,
                effect,
                attribution: attribution.into(),
                cost_effect: eff("cost_usd", &without, &full),
                tokens_effect: eff("total_tokens", &without, &full),
                precision_effect: meta.by_sentence().then(|| eff("trigger_precision", &without, &full)).flatten(),
                recall_effect: meta.by_sentence().then(|| eff("trigger_recall", &without, &full)).flatten(),
            }
        })
        .collect();
    let full_effect = eff(metric, &none, &full);
    let placebo = meta.placebo.as_ref().map(|p| {
        let p = label(p);
        let (full_vs_placebo, placebo_vs_none) = (eff(metric, &p, &full), eff(metric, &none, &p));
        PlaceboReport { attribution: placebo_attribution(full_effect.as_ref(), full_vs_placebo.as_ref(), placebo_vs_none.as_ref()).into(), full_vs_placebo, placebo_vs_none }
    });
    AblationReport {
        source: meta.source.clone(),
        kind: meta.kind.clone(),
        mode: meta.mode.clone().unwrap_or_else(|| "sections".into()),
        metric: metric.to_string(),
        full_effect,
        placebo,
        description_only_effect: desc.as_ref().and_then(|d| eff(metric, &none, d)),
        body_effect: desc.as_ref().and_then(|d| eff(metric, d, &full)),
        skill_invoked_rate: (!invoked.is_empty()).then(|| stats::mean(&invoked)),
        sections,
    }
}

// ------------------------------------------------------------- text output

pub fn fmt_value(info: &MetricInfo, v: f64) -> String {
    match (info.unit.as_str(), info.scale) {
        ("$", _) => format!("${v:.4}"),
        (_, stats::Scale::Rate) => format!("{:.0}%", v * 100.0),
        (_, stats::Scale::Score10) => format!("{v:.2}"),
        ("tok", _) => format!("{:.1}k", v / 1000.0),
        ("s", _) => format!("{v:.1}s"),
        _ => format!("{v:.1}"),
    }
}

pub fn fmt_delta(info: &MetricInfo, v: f64) -> String {
    let sign = if v >= 0.0 { "+" } else { "−" };
    let a = v.abs();
    let body = match (info.unit.as_str(), info.scale) {
        ("$", _) => format!("${a:.4}"),
        (_, stats::Scale::Rate) => format!("{:.0}pp", a * 100.0),
        (_, stats::Scale::Score10) => format!("{a:.2}"),
        ("tok", _) => format!("{:.1}k", a / 1000.0),
        ("s", _) => format!("{a:.1}s"),
        _ => format!("{a:.1}"),
    };
    format!("{sign}{body}")
}

pub fn fmt_effect(info: &MetricInfo, e: &Effect) -> String {
    let dropped = if e.saturated_units > 0 && e.verdict != Verdict::NoHeadroom { format!(" ({} saturated left out)", e.saturated_units) } else { String::new() };
    format!("{} [{}, {}] {}{dropped}", fmt_delta(info, e.effect), fmt_delta(info, e.ci_low), fmt_delta(info, e.ci_high), e.verdict.as_str())
}

/// Per-task headroom and spread of the baseline cells: what a
/// baseline-only run is for.
pub fn render_headroom_text(r: &Report) -> String {
    let pm = stats::metric_info(&r.primary_metric);
    let base = r.variants.first().map(|v| v.label.clone()).unwrap_or_default();
    let mut s = format!("\nHEADROOM (baseline \"{base}\"; {})\n", pm.label);
    let mut rows = vec![vec!["task".into(), "baseline".into(), "sd".into(), "n".into(), "headroom".into()]];
    for t in &r.tasks {
        let cell = t.cells.first().and_then(|c| c.metrics.get(&r.primary_metric));
        rows.push(vec![
            t.task_id.clone(),
            cell.map(|x| fmt_value(&pm, x.mean)).unwrap_or_else(|| "–".into()),
            cell.map(|x| fmt_value(&pm, x.sd).trim_start_matches('$').to_string()).unwrap_or_else(|| "–".into()),
            cell.map(|x| x.n.to_string()).unwrap_or_else(|| "0".into()),
            if cell.is_some() { t.headroom.describe() } else { "not run".into() },
        ]);
    }
    s.push_str(&table(&rows));
    let none = r.tasks.iter().filter(|t| t.headroom.is_none()).count();
    if none == r.tasks.len() && none > 0 {
        s.push_str("Every task is saturated: this experiment cannot show a difference on this metric. Add harder tasks or pick a metric that still varies.\n");
    } else if none > 0 {
        s.push_str(&format!("{none} of {} task(s) cannot show a difference on this metric.\n", r.tasks.len()));
    }
    s
}

fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols).map(|c| rows.iter().filter_map(|r| r.get(c)).map(|s| s.chars().count()).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for r in rows {
        let line: Vec<String> = r.iter().enumerate().map(|(i, s)| format!("{s}{}", " ".repeat(widths[i] - s.chars().count()))).collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

pub fn render_text(r: &Report) -> String {
    let mut s = String::new();
    let info = |m: &str| stats::metric_info(m);
    s.push_str(&format!("{}  launch #{}  {}{}\n", r.experiment, r.launch_id, r.status, if r.phase.is_empty() { String::new() } else { format!(" ({})", r.phase) }));
    if !r.description.is_empty() {
        s.push_str(&format!("{}\n", r.description));
    }
    s.push_str(&format!(
        "runs: {}/{} done, {} failed, {} timeout, {} pending · agent cost ${:.4} · judge cost ${:.4} · primary metric: {}\n",
        r.counts.done, r.counts.total, r.counts.failed, r.counts.timeout, r.counts.pending + r.counts.running, r.cost_usd, r.judge_cost_usd, r.primary_metric
    ));
    for w in &r.warnings {
        s.push_str(&format!("warning: {w}\n"));
    }

    let shown: Vec<&MetricInfo> = r.metrics.iter().filter(|m| ["pass", "judge_score", "expectations_passed", "skill_invoked", "cost_usd", "wall_time_s", "total_tokens", "turns"].contains(&m.name.as_str()) || m.name == r.primary_metric).collect();
    s.push_str("\nVARIANTS (mean ± sd)\n");
    let mut rows = vec![std::iter::once("variant".to_string()).chain(std::iter::once("n".into())).chain(shown.iter().map(|m| m.label.clone())).collect::<Vec<_>>()];
    for v in &r.variants {
        let mut row = vec![v.label.clone(), v.counts.done.to_string()];
        for m in &shown {
            row.push(v.metrics.get(&m.name).map(|x| format!("{} ±{}", fmt_value(m, x.mean), fmt_value(m, x.sd).trim_start_matches('$'))).unwrap_or_else(|| "–".into()));
        }
        rows.push(row);
    }
    s.push_str(&table(&rows));

    let key_metrics: Vec<String> = [r.primary_metric.as_str(), "pass", "cost_usd", "wall_time_s"].iter().filter(|m| r.metrics.iter().any(|x| x.name == **m)).map(|m| m.to_string()).collect::<BTreeSet<_>>().into_iter().collect();
    for f in &r.factors {
        let cs: Vec<&Comparison> = r.comparisons.iter().filter(|c| c.kind == "factor" && c.factor.as_deref() == Some(&f.name) && key_metrics.contains(&c.metric)).collect();
        if cs.is_empty() {
            continue;
        }
        s.push_str(&format!("\nFACTOR {} (vs {}; effect [95% CI], paired by task)\n", f.name, f.levels[0]));
        let mut rows = vec![vec!["level".into(), "metric".into(), "effect".into(), "relative".into(), "units".into()]];
        for c in cs {
            let i = info(&c.metric);
            rows.push(vec![c.level.clone(), i.label.clone(), fmt_effect(&i, &c.effect), c.effect.rel.map(|x| format!("{:+.0}%", x * 100.0)).unwrap_or_default(), c.effect.units.to_string()]);
        }
        s.push_str(&table(&rows));
    }

    // Per-level view, for pattern-like factors: what does the overlay do on each model?
    let mut seen = BTreeSet::new();
    for c in r.conditional.iter().filter(|c| c.metric == r.primary_metric) {
        let kind = |name: &str| r.factors.iter().find(|f| f.name == name).map(|f| f.kind.as_str()).unwrap_or("");
        if !matches!(kind(&c.factor), "overlay" | "system_prompt_append") || !seen.insert((c.factor.clone(), c.level.clone(), c.by_factor.clone())) {
            continue;
        }
        let i = info(&c.metric);
        s.push_str(&format!("\n{} \"{}\" WITHIN EACH {} ({}; vs \"{}\")\n", c.factor.to_uppercase(), c.level, c.by_factor.to_uppercase(), i.label, c.baseline));
        let mut rows = vec![vec![c.by_factor.clone(), "effect [95% CI]".into(), "without → with".into(), format!("difference from {}", c.first_level)]];
        for x in r.conditional.iter().filter(|x| x.metric == c.metric && x.factor == c.factor && x.level == c.level && x.by_factor == c.by_factor) {
            let diff = match &x.vs_first {
                None => "(reference)".to_string(),
                Some(d) => format!("{} [{}, {}] {}", fmt_delta(&i, d.diff), fmt_delta(&i, d.ci_low), fmt_delta(&i, d.ci_high), if d.differs { "differs" } else { "not distinguishable" }),
            };
            rows.push(vec![x.by_level.clone(), fmt_effect(&i, &x.effect), format!("{} → {}", fmt_value(&i, x.effect.base_mean), fmt_value(&i, x.effect.treat_mean)), diff]);
        }
        s.push_str(&table(&rows));
    }

    // Precision and recall belong to a set of tasks; per task, show how often the skill fired.
    let task_metric = if stats::TRIGGER_METRICS.contains(&r.primary_metric.as_str()) { "trigger_fired".to_string() } else { r.primary_metric.clone() };
    let pm = info(&task_metric);
    if r.tasks.len() > 1 || r.variants.len() > 1 {
        s.push_str(&format!("\nPER TASK ({}; * = flaky)\n", pm.label));
        let mut rows = vec![std::iter::once("task".to_string()).chain(std::iter::once("headroom".to_string())).chain(r.variants.iter().map(|v| v.label.clone())).collect::<Vec<_>>()];
        for t in &r.tasks {
            let mut row = vec![t.task_id.clone(), t.headroom.describe()];
            for c in &t.cells {
                let star = if c.flaky.contains(&task_metric) { "*" } else { "" };
                row.push(c.metrics.get(&task_metric).map(|x| format!("{}{star} (n={})", fmt_value(&pm, x.mean), x.n)).unwrap_or_else(|| "–".into()));
            }
            rows.push(row);
        }
        s.push_str(&table(&rows));
    }
    if let Some(note) = &r.flaky_note {
        s.push_str(&format!("note: {note}\n"));
    }
    if !r.flaky.is_empty() {
        s.push_str("\nFLAKY CELLS\n");
        for f in &r.flaky {
            let i = info(&f.metric);
            s.push_str(&format!("  {} · {} · {}: mean {} sd {} (n={})\n", f.task_id, f.variant, i.label, fmt_value(&i, f.mean), fmt_value(&i, f.sd), f.n));
        }
    }

    if let Some(p) = &r.pairwise {
        for rk in &p.rankings {
            s.push_str(&format!("\nPAIRWISE ({}; {} votes)\n", rk.source, rk.votes));
            if p.variants.len() > 2 {
                let mut order: Vec<usize> = (0..p.variants.len()).collect();
                order.sort_by(|a, b| rk.strengths[*b].strength.total_cmp(&rk.strengths[*a].strength));
                let mut rows = vec![vec!["variant".into(), "BT strength".into(), "95% CI".into(), "W".into(), "L".into(), "T".into()]];
                for i in order {
                    let x = &rk.strengths[i];
                    rows.push(vec![p.variants[i].clone(), format!("{:.3}", x.strength), format!("[{:.3}, {:.3}]", x.ci_low, x.ci_high), x.wins.to_string(), x.losses.to_string(), x.ties.to_string()]);
                }
                s.push_str(&table(&rows));
            }
            for pc in &rk.pairs {
                let rate = pc.b_rate.as_ref().map(|x| format!("{:.0}% [{:.0}%, {:.0}%]", x.mean * 100.0, x.ci_low * 100.0, x.ci_high * 100.0)).unwrap_or_default();
                s.push_str(&format!("  {} vs {}: {}–{} ({} ties, {} both bad) · preference for {}: {}\n", pc.a, pc.b, pc.a_wins, pc.b_wins, pc.ties, pc.both_bad, pc.b, rate));
            }
        }
        if let Some(pb) = &p.position_bias {
            s.push_str(&format!(
                "  position bias: {}/{} pairs inconsistent across orders ({:.0}%); first-listed result picked {:.0}% of {} decided picks (50% = unbiased)\n",
                pb.inconsistent, pb.pairs, pb.inconsistent_rate * 100.0, pb.first_position_rate * 100.0, pb.decided_picks
            ));
        }
        if let Some(a) = p.human_judge_agreement {
            s.push_str(&format!("  human–judge agreement: {:.0}% over {} shared pairs\n", a * 100.0, p.human_judge_overlap));
        }
        let reasons: Vec<&HumanVote> = p.human_votes.iter().filter(|v| !v.reason.is_empty()).collect();
        if !reasons.is_empty() {
            s.push_str("\nHUMAN VOTE REASONS\n");
            for v in reasons {
                let pick = match v.winner.as_str() {
                    "a" => format!("preferred {} over {}", v.a, v.b),
                    "b" => format!("preferred {} over {}", v.b, v.a),
                    "both_bad" => format!("{} and {} both bad", v.a, v.b),
                    _ => format!("{} and {} tied", v.a, v.b),
                };
                s.push_str(&format!("  {}: {pick}: {}\n", v.task_id, v.reason));
            }
        }
    }

    if let Some(c) = r.calibration.as_ref().filter(|c| c.overall.pairs + c.overall.rubric_pairs + c.overall.both_bad + c.overall.human_ties > 0) {
        s.push_str(&format!("\nJUDGE CALIBRATION ({}; human votes against the judge on the same pairs)\n", c.judge_model.clone().unwrap_or_default()));
        s.push_str(&crate::calibration::render_text(c));
    }
    if let Some(t) = &r.trigger {
        s.push_str(&render_trigger_text(t));
    }
    if let Some(a) = &r.ablation {
        s.push_str(&render_ablation_text(a));
    }
    s
}

pub fn render_trigger_text(t: &TriggerReport) -> String {
    let rate = |x: &Option<Summary>| x.as_ref().map(|x| format!("{:.0}% [{:.0}%, {:.0}%]", x.mean * 100.0, x.ci_low * 100.0, x.ci_high * 100.0)).unwrap_or_else(|| "–".into());
    let mut s = format!("\nTRIGGER ({}; rates over tasks [95% CI])\n", t.skills.join(", "));
    let mut rows = vec![vec!["variant".into(), "precision".into(), "recall".into(), "F1".into(), "should".into(), "should not".into()]];
    for v in &t.variants {
        rows.push(vec![v.label.clone(), rate(&v.precision), rate(&v.recall), rate(&v.f1), v.should_tasks.to_string(), v.should_not_tasks.to_string()]);
    }
    s.push_str(&table(&rows));
    if !t.by_model.is_empty() {
        s.push_str("by model\n");
        let mut rows = vec![vec!["model".into(), "precision".into(), "recall".into(), "F1".into()]];
        for m in &t.by_model {
            rows.push(vec![m.model.clone(), rate(&m.precision), rate(&m.recall), rate(&m.f1)]);
        }
        s.push_str(&table(&rows));
    }
    for v in &t.variants {
        let line = |p: &TriggerPrompt| format!("    {} ({:.0}% of {} runs): {}\n", p.task_id, p.fire_rate * 100.0, p.runs, crate::util::clip(&p.prompt, 110));
        if !v.false_negatives.is_empty() {
            s.push_str(&format!("  {}: never fired on {} should-fire prompt(s)\n", v.label, v.false_negatives.len()));
            v.false_negatives.iter().for_each(|p| s.push_str(&line(p)));
        }
        if !v.false_positives.is_empty() {
            s.push_str(&format!("  {}: fired on {} should-not prompt(s)\n", v.label, v.false_positives.len()));
            v.false_positives.iter().for_each(|p| s.push_str(&line(p)));
        }
    }
    s
}

pub fn render_ablation_text(a: &AblationReport) -> String {
    let i = stats::metric_info(&a.metric);
    let mut s = format!("\nABLATION of {} ({})\n", a.source, i.label);
    let e = |x: &Option<Effect>| x.as_ref().map(|e| fmt_effect(&i, e)).unwrap_or_else(|| "no data".into());
    s.push_str(&format!("  full vs none: {}\n", e(&a.full_effect)));
    if a.description_only_effect.is_some() {
        s.push_str(&format!("  description-only vs none: {}\n", e(&a.description_only_effect)));
        s.push_str(&format!("  full vs description-only: {}\n", e(&a.body_effect)));
    }
    if let Some(p) = &a.placebo {
        s.push_str(&format!("  full vs placebo: {}\n", e(&p.full_vs_placebo)));
        s.push_str(&format!("  placebo vs none: {}\n", e(&p.placebo_vs_none)));
        s.push_str(&format!(
            "  whole file: {}\n",
            match p.attribution.as_str() {
                "content" => "content (the gain comes from these instructions, not from having instructions)",
                "presence" => "presence (a placebo of the same length does as well; the gain comes from having instructions at all)",
                "neither" => "neither (nothing beats no file)",
                _ => "inconclusive (content and presence cannot be told apart on this data)",
            }
        ));
    }
    if let Some(rate) = a.skill_invoked_rate {
        s.push_str(&format!("  skill invoked in {:.0}% of full-skill runs\n", rate * 100.0));
    }
    let by_sentence = a.mode == "description_sentences";
    let span = |e: &Option<Effect>, i: &MetricInfo| e.as_ref().map(|e| format!("{} [{}, {}]", fmt_delta(i, e.effect), fmt_delta(i, e.ci_low), fmt_delta(i, e.ci_high))).unwrap_or_else(|| "–".into());
    let mut head: Vec<String> = vec!["#".into(), if by_sentence { "sentence".into() } else { "section".into() }, "~tokens".into(), "contribution (full − without) [95% CI]".into(), "attribution".into()];
    if by_sentence {
        head.extend(["precision".to_string(), "recall".to_string()]);
    }
    let mut rows = vec![head];
    for sec in &a.sections {
        let mut row = vec![(sec.index + 1).to_string(), crate::util::clip(&sec.title, 70), sec.est_tokens.to_string(), span(&sec.effect, &i), sec.attribution.replace('_', " ")];
        if by_sentence {
            row.push(span(&sec.precision_effect, &stats::metric_info("trigger_precision")));
            row.push(span(&sec.recall_effect, &stats::metric_info("trigger_recall")));
        }
        rows.push(row);
    }
    s.push_str(&table(&rows));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect(verdict: Verdict) -> Effect {
        Effect { effect: 0.0, ci_low: 0.0, ci_high: 0.0, rel: None, base_mean: 0.0, treat_mean: 0.0, units: 3, n_a: 9, n_b: 9, verdict, saturated_units: 0 }
    }

    #[test]
    fn placebo_attribution_cases() {
        let (better, neutral, unsure, worse) = (effect(Verdict::Better), effect(Verdict::Neutral), effect(Verdict::Inconclusive), effect(Verdict::Worse));
        let attr = |fvn: &Effect, fvp: &Effect, pvn: &Effect| placebo_attribution(Some(fvn), Some(fvp), Some(pvn));
        // placebo ≈ full > none: having instructions is what helps.
        assert_eq!(attr(&better, &neutral, &better), "presence");
        // full > placebo ≈ none: these instructions are what helps.
        assert_eq!(attr(&better, &better, &neutral), "content");
        // Both beat none and full beats placebo too: still content.
        assert_eq!(attr(&better, &better, &better), "content");
        assert_eq!(attr(&neutral, &neutral, &neutral), "neither");
        assert_eq!(attr(&worse, &neutral, &worse), "neither");
        assert_eq!(attr(&better, &unsure, &better), "inconclusive");
        assert_eq!(attr(&better, &neutral, &unsure), "inconclusive");
        assert_eq!(placebo_attribution(Some(&better), None, None), "inconclusive");
    }
}
