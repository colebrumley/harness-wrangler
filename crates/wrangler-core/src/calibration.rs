//! Judge calibration: how often the LLM judge agrees with a person on the
//! pairs both of them voted on, per launch and pooled across launches.
//!
//! Agreement is over pairs where the human picked a side. Human ties and
//! skips are left out, and "both bad" votes are counted separately: a high
//! "both bad" rate says something about the task, not about the judge.

use crate::stats;
use crate::store::{Judgment, Launch, RunRow, Store};
use crate::util::hash_u64;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Pairs needed before agreement means anything.
pub const MIN_PAIRS: usize = 8;
/// Agreement below this and the judge is not trusted.
pub const TRUST_AGREEMENT: f64 = 0.7;

/// One run pair that a person voted on, with what the judge said about it.
/// Sides are `lo` / `hi` by run id, so the two votes share an orientation.
#[derive(Debug, Clone)]
pub struct CalPair {
    pub task_id: String,
    pub tags: Vec<String>,
    /// lo | hi | tie | both_bad
    pub human: String,
    /// lo | hi | tie, when the pairwise judge voted on this pair.
    pub judge: Option<String>,
    pub judge_consistent: Option<bool>,
    /// Rubric score of `hi` minus `lo`, when both runs were scored.
    pub rubric_diff: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalibrationRow {
    /// `all`, or a task tag.
    pub scope: String,
    /// Pairs with a decided human vote and a judge vote.
    pub pairs: usize,
    /// Share of those where the judge picked the same side.
    pub agreement: Option<f64>,
    pub kappa: Option<f64>,
    pub kappa_ci_low: Option<f64>,
    pub kappa_ci_high: Option<f64>,
    /// The same, restricted to pairs where the judge agreed with itself across orders.
    pub consistent_pairs: usize,
    pub consistent_agreement: Option<f64>,
    /// Pairs with a decided human vote and rubric scores on both runs.
    pub rubric_pairs: usize,
    /// Share where the higher rubric score went to the human's pick (equal scores count half).
    pub rubric_agreement: Option<f64>,
    pub human_ties: usize,
    pub both_bad: usize,
    /// calibrated | low | not_enough
    pub status: String,
    /// Decided pairs still needed to reach `MIN_PAIRS`.
    pub pairs_needed: usize,
}

impl CalibrationRow {
    pub fn trusted(&self) -> bool {
        self.status == "calibrated"
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Calibration {
    pub judge_model: Option<String>,
    pub rubric: Option<String>,
    pub overall: CalibrationRow,
    pub by_tag: Vec<CalibrationRow>,
}

impl Calibration {
    /// The row for a tag, when tasks carry it.
    pub fn tag(&self, tag: &str) -> Option<&CalibrationRow> {
        self.by_tag.iter().find(|r| r.scope == tag)
    }

    /// Short form stored beside a suite baseline.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({"status": self.overall.status, "agreement": self.overall.agreement, "kappa": self.overall.kappa, "pairs": self.overall.pairs, "judge_model": self.judge_model,
            "untrusted_tags": self.by_tag.iter().filter(|r| !r.trusted()).map(|r| r.scope.clone()).collect::<Vec<_>>()})
    }
}

fn row(scope: &str, pairs: &[&CalPair], seed: u64) -> CalibrationRow {
    let decided: Vec<&&CalPair> = pairs.iter().filter(|p| p.human == "lo" || p.human == "hi").collect();
    let both: Vec<(String, String)> = decided.iter().filter_map(|p| Some((p.human.clone(), p.judge.clone()?))).collect();
    let agree = |v: &[(String, String)]| (!v.is_empty()).then(|| v.iter().filter(|(h, j)| h == j).count() as f64 / v.len() as f64);
    let consistent: Vec<(String, String)> = decided.iter().filter(|p| p.judge_consistent == Some(true)).filter_map(|p| Some((p.human.clone(), p.judge.clone()?))).collect();
    let rubric: Vec<f64> = decided
        .iter()
        .filter_map(|p| {
            let d = p.rubric_diff?;
            let human_hi = p.human == "hi";
            Some(if d == 0.0 { 0.5 } else if (d > 0.0) == human_hi { 1.0 } else { 0.0 })
        })
        .collect();
    let k = stats::kappa(&both, seed);
    let agreement = agree(&both);
    let rubric_agreement = (!rubric.is_empty()).then(|| stats::mean(&rubric));
    // A judge that only scores rubrics has no pairwise votes; it is then
    // held to how well its scores track the human's picks.
    let (basis_n, basis) = if both.is_empty() && !rubric.is_empty() { (rubric.len(), rubric_agreement) } else { (both.len(), agreement) };
    let status = if basis_n < MIN_PAIRS {
        "not_enough"
    } else if basis.unwrap_or(0.0) < TRUST_AGREEMENT {
        "low"
    } else {
        "calibrated"
    };
    CalibrationRow {
        scope: scope.to_string(),
        pairs: both.len(),
        agreement,
        kappa: k.as_ref().map(|k| k.kappa),
        kappa_ci_low: k.as_ref().map(|k| k.ci_low),
        kappa_ci_high: k.as_ref().map(|k| k.ci_high),
        consistent_pairs: consistent.len(),
        consistent_agreement: agree(&consistent),
        rubric_pairs: rubric.len(),
        rubric_agreement,
        human_ties: pairs.iter().filter(|p| p.human == "tie").count(),
        both_bad: pairs.iter().filter(|p| p.human == "both_bad").count(),
        status: status.into(),
        pairs_needed: MIN_PAIRS.saturating_sub(basis_n),
    }
}

/// Calibration table from a set of pairs: overall, then one row per tag.
pub fn from_pairs(pairs: &[CalPair], judge_model: Option<String>, rubric: Option<String>, seed: u64) -> Calibration {
    let all: Vec<&CalPair> = pairs.iter().collect();
    let mut tags: Vec<&str> = pairs.iter().flat_map(|p| p.tags.iter().map(|t| t.as_str())).collect();
    tags.sort();
    tags.dedup();
    let by_tag = tags.iter().map(|t| row(t, &pairs.iter().filter(|p| p.tags.iter().any(|x| x == t)).collect::<Vec<_>>(), seed ^ hash_u64(t))).collect();
    Calibration { judge_model, rubric, overall: row("all", &all, seed), by_tag }
}

/// Every human-voted pair of a launch, joined with the judge's vote on the
/// same pair and the two runs' rubric scores.
pub fn pairs_of(launch: &Launch, runs: &[RunRow], judgments: &[Judgment], metrics: &HashMap<i64, BTreeMap<String, f64>>, tasks: Option<&[String]>) -> Vec<CalPair> {
    let side = |j: &Judgment| -> ((i64, i64), &'static str) {
        let lo_is_a = j.run_a <= j.run_b;
        let key = if lo_is_a { (j.run_a, j.run_b) } else { (j.run_b, j.run_a) };
        let s = match (j.winner.as_str(), lo_is_a) {
            ("a", true) | ("b", false) => "lo",
            ("b", true) | ("a", false) => "hi",
            ("both_bad", _) => "both_bad",
            _ => "tie",
        };
        (key, s)
    };
    let judge: HashMap<(i64, i64), (&str, Option<bool>)> = judgments.iter().filter(|j| j.source == "judge").map(|j| (side(j).0, (side(j).1, j.consistent))).collect();
    let score = |id: i64| metrics.get(&id).and_then(|m| m.get("judge_score")).copied();
    let known: std::collections::HashSet<i64> = runs.iter().map(|r| r.id).collect();
    judgments
        .iter()
        .filter(|j| j.source == "human" && j.winner != "skip" && known.contains(&j.run_a) && known.contains(&j.run_b))
        .filter(|j| tasks.map(|t| t.contains(&j.task_id)).unwrap_or(true))
        .map(|j| {
            let (key, human) = side(j);
            let jv = judge.get(&key);
            CalPair {
                task_id: j.task_id.clone(),
                tags: launch.tasks.iter().find(|t| t.id == j.task_id).map(|t| t.tags.clone()).unwrap_or_default(),
                human: human.to_string(),
                judge: jv.map(|v| v.0.to_string()),
                judge_consistent: jv.and_then(|v| v.1),
                rubric_diff: score(key.1).zip(score(key.0)).map(|(hi, lo)| hi - lo),
            }
        })
        .collect()
}

/// Calibration of one launch's judge.
pub fn for_launch(store: &Store, launch_id: i64) -> Result<Calibration> {
    let launch = store.launch(launch_id)?;
    let pairs = pairs_of(&launch, &store.runs(launch_id)?, &store.judgments(launch_id, None)?, &store.metrics_for_launch(launch_id)?, None);
    let cfg = launch.def.judge();
    Ok(from_pairs(&pairs, cfg.as_ref().map(|c| c.model.clone()), cfg.and_then(|c| c.rubric), hash_u64(&format!("{launch_id}|calibration"))))
}

#[derive(Debug, Clone, Serialize)]
pub struct Pooled {
    pub judge_model: String,
    pub rubric: Option<String>,
    pub launches: Vec<i64>,
    #[serde(flatten)]
    pub calibration: Calibration,
}

/// Pool every judgment in the project's database by judge model and rubric.
/// The launch definition carries the judge configuration, so nothing extra
/// is stored. Filters narrow to one judge model, one rubric, or one task tag.
pub fn pooled(store: &Store, judge: Option<&str>, rubric: Option<&str>, tag: Option<&str>) -> Result<Vec<Pooled>> {
    let mut groups: BTreeMap<(String, Option<String>), (Vec<i64>, Vec<CalPair>)> = BTreeMap::new();
    for l in store.list_launches(None)? {
        let Ok(launch) = store.launch(l.id) else { continue };
        let Some(cfg) = launch.def.judge() else { continue };
        if judge.map(|j| j != cfg.model).unwrap_or(false) || rubric.map(|r| Some(r) != cfg.rubric.as_deref()).unwrap_or(false) {
            continue;
        }
        let mut pairs = pairs_of(&launch, &store.runs(l.id)?, &store.judgments(l.id, None)?, &store.metrics_for_launch(l.id)?, None);
        if let Some(t) = tag {
            pairs.retain(|p| p.tags.iter().any(|x| x == t));
        }
        if pairs.is_empty() {
            continue;
        }
        let g = groups.entry((cfg.model.clone(), cfg.rubric.clone())).or_default();
        g.0.push(l.id);
        g.1.extend(pairs);
    }
    Ok(groups
        .into_iter()
        .map(|((model, rubric), (mut launches, pairs))| {
            launches.sort();
            let calibration = from_pairs(&pairs, Some(model.clone()), rubric.clone(), hash_u64(&format!("pooled|{model}|{rubric:?}")));
            Pooled { judge_model: model, rubric, launches, calibration }
        })
        .collect())
}

pub fn render_text(c: &Calibration) -> String {
    let pct = |x: Option<f64>| x.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "–".into());
    let mut rows = vec![vec!["scope".to_string(), "pairs".into(), "agreement".into(), "kappa [95% CI]".into(), "when judge consistent".into(), "rubric sign".into(), "both bad".into(), "status".into()]];
    for r in std::iter::once(&c.overall).chain(c.by_tag.iter()) {
        rows.push(vec![
            r.scope.clone(),
            r.pairs.to_string(),
            pct(r.agreement),
            match (r.kappa, r.kappa_ci_low, r.kappa_ci_high) {
                (Some(k), Some(lo), Some(hi)) => format!("{k:.2} [{lo:.2}, {hi:.2}]"),
                (Some(k), _, _) => format!("{k:.2}"),
                _ => "–".into(),
            },
            format!("{} ({})", pct(r.consistent_agreement), r.consistent_pairs),
            format!("{} ({})", pct(r.rubric_agreement), r.rubric_pairs),
            r.both_bad.to_string(),
            match r.status.as_str() {
                "not_enough" => format!("not enough human votes to calibrate ({} more)", r.pairs_needed),
                "low" => "low agreement".into(),
                s => s.to_string(),
            },
        ]);
    }
    let widths: Vec<usize> = (0..rows[0].len()).map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0)).collect();
    let mut s = String::new();
    for r in &rows {
        let line: Vec<String> = r.iter().enumerate().map(|(i, x)| format!("{x}{}", " ".repeat(widths[i] - x.chars().count()))).collect();
        s.push_str(line.join("  ").trim_end());
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(tag: &str, human: &str, judge: Option<&str>, consistent: bool, diff: Option<f64>) -> CalPair {
        CalPair { task_id: "t".into(), tags: vec![tag.into()], human: human.into(), judge: judge.map(|s| s.to_string()), judge_consistent: judge.map(|_| consistent), rubric_diff: diff }
    }

    #[test]
    fn rows_split_by_tag_and_count_what_they_leave_out() {
        let mut pairs = vec![];
        for i in 0..8 {
            let side = if i % 2 == 0 { "lo" } else { "hi" };
            pairs.push(pair("writing", side, Some(side), true, Some(if side == "hi" { 2.0 } else { -2.0 })));
            pairs.push(pair("coding", side, Some(if side == "lo" { "hi" } else { "lo" }), i < 4, Some(0.0)));
        }
        pairs.push(pair("writing", "tie", Some("lo"), true, None));
        pairs.push(pair("writing", "both_bad", Some("lo"), true, None));
        pairs.push(pair("writing", "hi", None, false, None)); // no judge vote: not a calibration pair
        let c = from_pairs(&pairs, Some("m".into()), None, 1);
        assert_eq!((c.overall.pairs, c.overall.human_ties, c.overall.both_bad), (16, 1, 1));
        assert_eq!(c.overall.agreement, Some(0.5));
        assert_eq!(c.overall.status, "low");
        let (w, k) = (c.tag("writing").unwrap(), c.tag("coding").unwrap());
        assert_eq!((w.agreement, w.status.as_str(), w.rubric_agreement), (Some(1.0), "calibrated", Some(1.0)));
        assert!(w.kappa.unwrap() > 0.99);
        assert_eq!((k.agreement, k.status.as_str(), k.rubric_agreement), (Some(0.0), "low", Some(0.5)));
        assert_eq!((k.consistent_pairs, k.consistent_agreement), (4, Some(0.0)));
        // Fewer than eight pairs is not a verdict on the judge.
        let few = from_pairs(&pairs[..6], None, None, 1);
        assert_eq!((few.overall.status.as_str(), few.overall.pairs_needed), ("not_enough", 2));
        assert!(render_text(&few).contains("not enough human votes to calibrate (2 more)"));
        assert!(render_text(&c).contains("low agreement"));
    }
}
