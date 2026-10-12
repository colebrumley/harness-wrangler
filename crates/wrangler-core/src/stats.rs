//! Statistics: summaries, paired effects with bootstrapped confidence
//! intervals, Bradley–Terry aggregation of pairwise votes, and flakiness.
//!
//! All resampling is seeded, so the same data always yields the same report.

use crate::util::Rng;
use serde::{Deserialize, Serialize};

pub const BOOTSTRAP_ITERS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Higher,
    Lower,
    /// Neither direction is better in itself (turns, words, tool calls).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scale {
    /// 0..1 rates such as pass.
    Rate,
    /// 1..10 judge scores.
    Score10,
    /// Non-negative magnitudes: cost, time, tokens, counts.
    Positive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricInfo {
    pub name: String,
    pub label: String,
    pub direction: Direction,
    pub scale: Scale,
    pub unit: String,
}

pub fn metric_info(name: &str) -> MetricInfo {
    use Direction::*;
    use Scale::*;
    let (label, direction, scale, unit) = match name {
        "pass" => ("Pass rate", Higher, Rate, ""),
        "checks_passed" => ("Checks passed", Higher, Rate, ""),
        "expectations_passed" => ("Expectations met", Higher, Rate, ""),
        "judge_score" => ("Judge score", Higher, Score10, "/10"),
        "skill_invoked" => ("Skill invoked", Higher, Rate, ""),
        "trigger_precision" => ("Trigger precision", Higher, Rate, ""),
        "trigger_recall" => ("Trigger recall", Higher, Rate, ""),
        "trigger_f1" => ("Trigger F1", Higher, Rate, ""),
        "trigger_fired" => ("Skill fired", None, Rate, ""),
        "cache_hit" => ("Cache hit", Higher, Rate, ""),
        "cost_usd" => ("Cost", Lower, Positive, "$"),
        "judge_cost_usd" => ("Judge cost", Lower, Positive, "$"),
        "wall_time_s" => ("Wall time", Lower, Positive, "s"),
        "total_tokens" => ("Total tokens", Lower, Positive, "tok"),
        "input_tokens" => ("Input tokens", Lower, Positive, "tok"),
        "output_tokens" => ("Output tokens", Lower, Positive, "tok"),
        "thinking_tokens" => ("Thinking tokens", Lower, Positive, "tok"),
        "cache_read_tokens" => ("Cache read tokens", None, Positive, "tok"),
        "cache_creation_tokens" => ("Cache write tokens", None, Positive, "tok"),
        "turns" => ("Turns", None, Positive, ""),
        "tool_calls" => ("Tool calls", None, Positive, ""),
        "tool_calls_before_final" => ("Tool calls before final", None, Positive, ""),
        "distinct_tools" => ("Distinct tools", None, Positive, ""),
        "diff_lines" => ("Diff size", None, Positive, "lines"),
        "files_touched" => ("Files touched", None, Positive, ""),
        "output_words" => ("Output words", None, Positive, "words"),
        "retries" => ("Rate-limit retries", Lower, Positive, ""),
        "permission_denials" => ("Permission denials", Lower, Positive, ""),
        // One deterministic grader's own pass rate.
        _ if name.starts_with("check.") => ("", Higher, Rate, ""),
        _ => ("", None, Positive, ""),
    };
    let label = match (label.is_empty(), name.strip_prefix("check.")) {
        (false, _) => label.to_string(),
        (true, Some(check)) => format!("Check: {check}"),
        (true, Option::None) => name.to_string(),
    };
    MetricInfo { name: name.to_string(), label, direction, scale, unit: unit.to_string() }
}

/// Metrics shown by default in reports, in display order.
pub const HEADLINE: &[&str] = &["pass", "judge_score", "expectations_passed", "checks_passed", "skill_invoked", "trigger_fired", "cost_usd", "wall_time_s", "total_tokens", "output_tokens", "turns", "tool_calls", "output_words", "diff_lines"];

pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

pub fn sd(xs: &[f64]) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64).sqrt()
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let pos = p * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

fn resample_mean(xs: &[f64], rng: &mut Rng) -> f64 {
    let n = xs.len();
    (0..n).map(|_| xs[rng.below(n)]).sum::<f64>() / n as f64
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub n: usize,
    pub mean: f64,
    pub sd: f64,
    pub min: f64,
    pub max: f64,
    /// Bootstrapped 95% CI of the mean.
    pub ci_low: f64,
    pub ci_high: f64,
}

pub fn summarize(xs: &[f64], seed: u64) -> Option<Summary> {
    if xs.is_empty() {
        return None;
    }
    let m = mean(xs);
    let (mut lo, mut hi) = (m, m);
    if xs.len() > 1 {
        let mut rng = Rng::new(seed);
        let mut boots: Vec<f64> = (0..BOOTSTRAP_ITERS).map(|_| resample_mean(xs, &mut rng)).collect();
        boots.sort_by(|a, b| a.total_cmp(b));
        lo = percentile(&boots, 0.025);
        hi = percentile(&boots, 0.975);
    }
    Some(Summary {
        n: xs.len(),
        mean: m,
        sd: sd(xs),
        min: xs.iter().cloned().fold(f64::INFINITY, f64::min),
        max: xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        ci_low: lo,
        ci_high: hi,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The CI excludes zero and the change is in the metric's good direction.
    Better,
    Worse,
    /// The CI excludes zero on a metric with no good direction.
    Higher,
    Lower,
    /// The CI spans zero and is tight enough to rule out a meaningful effect.
    Neutral,
    /// The CI spans zero and is too wide to say.
    Inconclusive,
    /// Every pairing unit sits at the metric's bound in both arms, so the
    /// data could not have shown an effect. Counts as inconclusive.
    #[serde(rename = "no_headroom")]
    NoHeadroom,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Better => "better",
            Verdict::Worse => "worse",
            Verdict::Higher => "higher",
            Verdict::Lower => "lower",
            Verdict::Neutral => "neutral",
            Verdict::Inconclusive => "inconclusive",
            Verdict::NoHeadroom => "no headroom",
        }
    }
    pub fn is_conclusive_change(&self) -> bool {
        matches!(self, Verdict::Better | Verdict::Worse | Verdict::Higher | Verdict::Lower)
    }
}

/// A paired comparison of treatment B against control A.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    /// Mean over pairing units of (mean B − mean A).
    pub effect: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    /// Effect relative to the control mean, when that is non-zero.
    pub rel: Option<f64>,
    pub base_mean: f64,
    pub treat_mean: f64,
    /// Pairing units (tasks, or task × other factor levels) with data on both sides.
    pub units: usize,
    pub n_a: usize,
    pub n_b: usize,
    pub verdict: Verdict,
    /// Pairing units left out of the estimate because both arms sit at the
    /// metric's bound. When every unit is saturated the verdict is `NoHeadroom`.
    #[serde(default)]
    pub saturated_units: usize,
}

/// How much room a set of baseline values leaves for a metric to move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Headroom {
    /// none | little | ok
    pub headroom: String,
    /// ceiling | floor, when headroom is none or little.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

impl Headroom {
    fn new(headroom: &str, at: Option<&str>) -> Headroom {
        Headroom { headroom: headroom.into(), at: at.map(|s| s.to_string()) }
    }
    pub fn ok() -> Headroom {
        Headroom::new("ok", None)
    }
    pub fn is_none(&self) -> bool {
        self.headroom == "none"
    }
    /// "none (ceiling)", "little (floor)", "ok".
    pub fn describe(&self) -> String {
        match &self.at {
            Some(at) => format!("{} ({at})", self.headroom),
            None => self.headroom.clone(),
        }
    }
}

/// Judge-score thresholds for saturation. Starting points; see
/// docs/requirements/05-headroom-check.md.
pub const SCORE_CEILING: f64 = 9.0;
pub const SCORE_FLOOR: f64 = 2.0;
const SATURATION_MIN_N: usize = 3;

/// Is a cell at the metric's bound with no variance? Rates: all 0 or all 1
/// over at least three runs. Judge scores: mean at or beyond 9 (or 2) with
/// sd below 0.5. "Little" is within one equivalence band of the bound.
/// Magnitudes such as cost have no bound and are always `ok`.
pub fn saturation(xs: &[f64], info: &MetricInfo) -> Headroom {
    if xs.len() < SATURATION_MIN_N {
        return Headroom::ok();
    }
    let (m, spread) = (mean(xs), sd(xs));
    let band = equivalence_band(info.scale, m);
    match info.scale {
        Scale::Rate => {
            if m >= 1.0 - 1e-9 {
                Headroom::new("none", Some("ceiling"))
            } else if m <= 1e-9 {
                Headroom::new("none", Some("floor"))
            } else if m >= 1.0 - band {
                Headroom::new("little", Some("ceiling"))
            } else if m <= band {
                Headroom::new("little", Some("floor"))
            } else {
                Headroom::ok()
            }
        }
        Scale::Score10 => {
            if m >= SCORE_CEILING && spread < 0.5 {
                Headroom::new("none", Some("ceiling"))
            } else if m <= SCORE_FLOOR && spread < 0.5 {
                Headroom::new("none", Some("floor"))
            } else if m >= SCORE_CEILING - band {
                Headroom::new("little", Some("ceiling"))
            } else if m <= SCORE_FLOOR + band {
                Headroom::new("little", Some("floor"))
            } else {
                Headroom::ok()
            }
        }
        Scale::Positive => Headroom::ok(),
    }
}

/// A pairing unit is saturated when the control is at the bound and the
/// treatment sits at the same bound: nothing moved, and nothing could have
/// moved in the direction the bound blocks. A treatment that left the bound
/// (a pattern that breaks a passing task) is a real effect and is kept.
fn unit_saturated(a: &[f64], b: &[f64], info: &MetricInfo) -> bool {
    let (sa, sb) = (saturation(a, info), saturation(b, info));
    sa.is_none() && sb.is_none() && sa.at == sb.at
}

/// Width of the "no meaningful effect" band around zero.
pub fn equivalence_band(scale: Scale, base_mean: f64) -> f64 {
    match scale {
        Scale::Rate => 0.1,
        Scale::Score10 => 0.5,
        Scale::Positive => (0.1 * base_mean.abs()).max(1e-9),
    }
}

/// Paired effect with a hierarchical bootstrap: resample pairing units, then
/// resample runs within each side of each unit. Units missing either side
/// are dropped. Saturated units (both arms at the metric's bound) are left
/// out of the estimate and counted; when every unit is saturated the
/// verdict is `NoHeadroom`.
pub fn paired_effect(units: &[(Vec<f64>, Vec<f64>)], info: &MetricInfo, seed: u64) -> Option<Effect> {
    let all: Vec<&(Vec<f64>, Vec<f64>)> = units.iter().filter(|(a, b)| !a.is_empty() && !b.is_empty()).collect();
    if all.is_empty() {
        return None;
    }
    let open: Vec<&(Vec<f64>, Vec<f64>)> = all.iter().copied().filter(|(a, b)| !unit_saturated(a, b, info)).collect();
    let saturated_units = all.len() - open.len();
    let no_headroom = open.is_empty();
    let units = if no_headroom { all } else { open };
    let deltas: Vec<f64> = units.iter().map(|(a, b)| mean(b) - mean(a)).collect();
    let effect = mean(&deltas);
    let base_mean = mean(&units.iter().map(|(a, _)| mean(a)).collect::<Vec<_>>());
    let treat_mean = mean(&units.iter().map(|(_, b)| mean(b)).collect::<Vec<_>>());
    let n_a: usize = units.iter().map(|(a, _)| a.len()).sum();
    let n_b: usize = units.iter().map(|(_, b)| b.len()).sum();
    let mut rng = Rng::new(seed);
    let k = units.len();
    let mut boots = Vec::with_capacity(BOOTSTRAP_ITERS);
    for _ in 0..BOOTSTRAP_ITERS {
        let mut total = 0.0;
        for _ in 0..k {
            let (a, b) = units[if k > 1 { rng.below(k) } else { 0 }];
            total += resample_mean(b, &mut rng) - resample_mean(a, &mut rng);
        }
        boots.push(total / k as f64);
    }
    boots.sort_by(|a, b| a.total_cmp(b));
    let (ci_low, ci_high) = (percentile(&boots, 0.025), percentile(&boots, 0.975));
    // One run per side gives a zero-width interval that proves nothing.
    let degenerate = n_a <= k && n_b <= k && k == 1;
    let band = equivalence_band(info.scale, base_mean);
    // A zero-width interval at zero means every run gave the same value on
    // both sides: no evidence of "no effect", only of no variation.
    let flat = (ci_high - ci_low).abs() < 1e-12 && effect.abs() < 1e-12;
    let verdict = if no_headroom {
        Verdict::NoHeadroom
    } else if degenerate || flat {
        Verdict::Inconclusive
    } else if ci_low > 0.0 || ci_high < 0.0 {
        let up = ci_low > 0.0;
        match info.direction {
            Direction::Higher => if up { Verdict::Better } else { Verdict::Worse },
            Direction::Lower => if up { Verdict::Worse } else { Verdict::Better },
            Direction::None => if up { Verdict::Higher } else { Verdict::Lower },
        }
    } else if ci_low >= -band && ci_high <= band {
        Verdict::Neutral
    } else {
        Verdict::Inconclusive
    };
    Some(Effect {
        effect,
        ci_low,
        ci_high,
        rel: (base_mean.abs() > 1e-12).then(|| effect / base_mean),
        base_mean,
        treat_mean,
        units: k,
        n_a,
        n_b,
        verdict,
        saturated_units,
    })
}

/// How much one paired effect differs from another (B's effect minus A's),
/// with a bootstrapped 95% CI. This is the interaction: "does the pattern
/// behave differently here than there?"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contrast {
    pub diff: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    /// The CI excludes zero: the two effects really differ.
    pub differs: bool,
}

pub fn effect_difference(a: &[(Vec<f64>, Vec<f64>)], b: &[(Vec<f64>, Vec<f64>)], seed: u64) -> Option<Contrast> {
    let clean = |u: &[(Vec<f64>, Vec<f64>)]| -> Vec<(Vec<f64>, Vec<f64>)> { u.iter().filter(|(x, y)| !x.is_empty() && !y.is_empty()).cloned().collect() };
    let (a, b) = (clean(a), clean(b));
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let point = |u: &[(Vec<f64>, Vec<f64>)]| mean(&u.iter().map(|(x, y)| mean(y) - mean(x)).collect::<Vec<_>>());
    let mut rng = Rng::new(seed);
    let draw = |u: &[(Vec<f64>, Vec<f64>)], rng: &mut Rng| -> f64 {
        let k = u.len();
        (0..k).map(|_| {
            let (x, y) = &u[if k > 1 { rng.below(k) } else { 0 }];
            resample_mean(y, rng) - resample_mean(x, rng)
        }).sum::<f64>() / k as f64
    };
    let mut boots: Vec<f64> = (0..BOOTSTRAP_ITERS).map(|_| draw(&b, &mut rng) - draw(&a, &mut rng)).collect();
    boots.sort_by(|x, y| x.total_cmp(y));
    let (lo, hi) = (percentile(&boots, 0.025), percentile(&boots, 0.975));
    let thin = a.iter().chain(b.iter()).all(|(x, y)| x.len() < 2 && y.len() < 2) && a.len() == 1 && b.len() == 1;
    Some(Contrast { diff: point(&b) - point(&a), ci_low: lo, ci_high: hi, differs: !thin && (lo > 0.0 || hi < 0.0) })
}

/// Fewest runs in a cell before its spread says anything about flakiness.
pub const FLAKY_MIN_N: usize = 5;

/// Is this cell's spread large enough to call the task flaky on this metric?
/// Below five runs the answer is always no: three runs that split 2 to 1 are noise.
pub fn is_flaky(xs: &[f64], scale: Scale) -> bool {
    if xs.len() < FLAKY_MIN_N {
        return false;
    }
    let m = mean(xs);
    match scale {
        Scale::Rate => (0.2..=0.8).contains(&m) && xs.iter().all(|x| *x == 0.0 || *x == 1.0),
        Scale::Score10 => sd(xs) >= 1.5,
        Scale::Positive => m.abs() > 1e-12 && sd(xs) / m.abs() >= 0.6,
    }
}

// ----------------------------------------------------------------- trigger

pub const TRIGGER_METRICS: &[&str] = &["trigger_precision", "trigger_recall", "trigger_f1"];

/// One trigger task in one variant: whether the skill should fire on it,
/// and whether it fired (1 or 0) in each run.
#[derive(Debug, Clone)]
pub struct TriggerTask {
    pub should: bool,
    pub fired: Vec<f64>,
}

/// Precision, recall or F1 over tasks, each task weighing in by its fire
/// rate. `None` when the metric is undefined: precision when nothing fired,
/// recall when no task should fire.
pub fn trigger_value(tasks: &[&TriggerTask], metric: &str) -> Option<f64> {
    let rate = |t: &&TriggerTask| mean(&t.fired);
    let tp: f64 = tasks.iter().filter(|t| t.should && !t.fired.is_empty()).map(rate).sum();
    let fp: f64 = tasks.iter().filter(|t| !t.should && !t.fired.is_empty()).map(rate).sum();
    let positives = tasks.iter().filter(|t| t.should && !t.fired.is_empty()).count() as f64;
    let precision = (tp + fp > 0.0).then(|| tp / (tp + fp));
    let recall = (positives > 0.0).then(|| tp / positives);
    match metric {
        "trigger_precision" => precision,
        "trigger_recall" => recall,
        // With nothing fired F1 is 0 when something should have, undefined otherwise.
        _ => match (precision, recall) {
            (Some(p), Some(r)) if p + r > 0.0 => Some(2.0 * p * r / (p + r)),
            (_, Some(_)) => Some(0.0),
            _ => Option::None,
        },
    }
}

/// A trigger metric for one variant with a bootstrapped 95% CI over tasks.
pub fn trigger_summary(tasks: &[TriggerTask], metric: &str, seed: u64) -> Option<Summary> {
    let all: Vec<&TriggerTask> = tasks.iter().collect();
    let point = trigger_value(&all, metric)?;
    let mut rng = Rng::new(seed);
    let mut boots: Vec<f64> = (0..BOOTSTRAP_ITERS).filter_map(|_| trigger_value(&(0..all.len()).map(|_| all[rng.below(all.len())]).collect::<Vec<_>>(), metric)).collect();
    boots.sort_by(|a, b| a.total_cmp(b));
    let (lo, hi) = if boots.is_empty() { (point, point) } else { (percentile(&boots, 0.025), percentile(&boots, 0.975)) };
    Some(Summary { n: tasks.len(), mean: point, sd: 0.0, min: lo, max: hi, ci_low: lo, ci_high: hi })
}

/// Difference in a trigger metric between two variants (B minus A), paired
/// by task: each bootstrap draw resamples tasks once and scores both arms
/// on the same draw.
pub fn trigger_effect(units: &[(TriggerTask, TriggerTask)], metric: &str, seed: u64) -> Option<Effect> {
    let info = metric_info(metric);
    let side = |pick: &dyn Fn(&(TriggerTask, TriggerTask)) -> &TriggerTask, idx: &[usize]| -> Option<f64> { trigger_value(&idx.iter().map(|i| pick(&units[*i])).collect::<Vec<_>>(), metric) };
    let every: Vec<usize> = (0..units.len()).collect();
    let (base, treat) = (side(&|u| &u.0, &every)?, side(&|u| &u.1, &every)?);
    let mut rng = Rng::new(seed);
    let mut boots = vec![];
    for _ in 0..BOOTSTRAP_ITERS {
        let idx: Vec<usize> = (0..units.len()).map(|_| rng.below(units.len())).collect();
        if let (Some(a), Some(b)) = (side(&|u| &u.0, &idx), side(&|u| &u.1, &idx)) {
            boots.push(b - a);
        }
    }
    boots.sort_by(|a, b| a.total_cmp(b));
    let effect = treat - base;
    let (ci_low, ci_high) = if boots.is_empty() { (effect, effect) } else { (percentile(&boots, 0.025), percentile(&boots, 0.975)) };
    let band = equivalence_band(info.scale, base);
    let at_bound = |v: f64| v <= 1e-9 || v >= 1.0 - 1e-9;
    let flat = (ci_high - ci_low).abs() < 1e-12 && effect.abs() < 1e-12;
    let verdict = if units.len() < 2 {
        Verdict::Inconclusive
    } else if flat && at_bound(base) && at_bound(treat) {
        Verdict::NoHeadroom
    } else if flat {
        Verdict::Inconclusive
    } else if ci_low > 0.0 {
        Verdict::Better
    } else if ci_high < 0.0 {
        Verdict::Worse
    } else if ci_low >= -band && ci_high <= band {
        Verdict::Neutral
    } else {
        Verdict::Inconclusive
    };
    Some(Effect {
        effect,
        ci_low,
        ci_high,
        rel: (base.abs() > 1e-12).then(|| effect / base),
        base_mean: base,
        treat_mean: treat,
        units: units.len(),
        n_a: units.iter().map(|u| u.0.fired.len()).sum(),
        n_b: units.iter().map(|u| u.1.fired.len()).sum(),
        verdict,
        saturated_units: 0,
    })
}

// ---------------------------------------------------------- Bradley–Terry

/// One pairwise outcome between items `i` and `j`: `score_i` is 1 for a win
/// by `i`, 0 for a loss, 0.5 for a tie.
#[derive(Debug, Clone, Copy)]
pub struct Bout {
    pub i: usize,
    pub j: usize,
    pub score_i: f64,
}

/// Bradley–Terry strengths by the MM algorithm, normalized to sum to 1.
/// A small symmetric prior keeps unbeaten or winless items finite.
pub fn bradley_terry(n: usize, bouts: &[Bout]) -> Vec<f64> {
    if n == 0 {
        return vec![];
    }
    const PRIOR: f64 = 0.25;
    let mut wins = vec![0.0; n];
    let mut games = vec![vec![0.0; n]; n];
    for b in bouts {
        if b.i == b.j || b.i >= n || b.j >= n {
            continue;
        }
        wins[b.i] += b.score_i;
        wins[b.j] += 1.0 - b.score_i;
        games[b.i][b.j] += 1.0;
        games[b.j][b.i] += 1.0;
    }
    for i in 0..n {
        for j in 0..n {
            if i != j && games[i][j] > 0.0 {
                wins[i] += PRIOR / 2.0;
                games[i][j] += PRIOR / 2.0;
            }
        }
    }
    let mut p = vec![1.0 / n as f64; n];
    for _ in 0..500 {
        let mut next = vec![0.0; n];
        for i in 0..n {
            let denom: f64 = (0..n).filter(|&j| j != i && games[i][j] > 0.0).map(|j| games[i][j] / (p[i] + p[j])).sum();
            next[i] = if denom > 0.0 { wins[i] / denom } else { p[i] };
        }
        let total: f64 = next.iter().sum();
        if total <= 0.0 {
            break;
        }
        next.iter_mut().for_each(|x| *x /= total);
        let delta: f64 = next.iter().zip(&p).map(|(a, b)| (a - b).abs()).sum();
        p = next;
        if delta < 1e-10 {
            break;
        }
    }
    p
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BtItem {
    pub strength: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub wins: f64,
    pub losses: f64,
    pub ties: f64,
}

/// Strengths with bootstrapped CIs (resampling bouts).
pub fn bradley_terry_ci(n: usize, bouts: &[Bout], seed: u64) -> Vec<BtItem> {
    let point = bradley_terry(n, bouts);
    let mut samples: Vec<Vec<f64>> = vec![Vec::new(); n];
    if !bouts.is_empty() {
        let mut rng = Rng::new(seed);
        for _ in 0..400 {
            let re: Vec<Bout> = (0..bouts.len()).map(|_| bouts[rng.below(bouts.len())]).collect();
            for (i, s) in bradley_terry(n, &re).into_iter().enumerate() {
                samples[i].push(s);
            }
        }
    }
    (0..n)
        .map(|i| {
            samples[i].sort_by(|a, b| a.total_cmp(b));
            let (lo, hi) = if samples[i].is_empty() { (point[i], point[i]) } else { (percentile(&samples[i], 0.025), percentile(&samples[i], 0.975)) };
            let mut item = BtItem { strength: point[i], ci_low: lo, ci_high: hi, wins: 0.0, losses: 0.0, ties: 0.0 };
            for b in bouts.iter().filter(|b| b.i == i || b.j == i) {
                let s = if b.i == i { b.score_i } else { 1.0 - b.score_i };
                if s > 0.5 {
                    item.wins += 1.0
                } else if s < 0.5 {
                    item.losses += 1.0
                } else {
                    item.ties += 1.0
                }
            }
            item
        })
        .collect()
}

/// Win rate of B over A with a bootstrapped CI; ties count half.
pub fn win_rate(scores_b: &[f64], seed: u64) -> Option<Summary> {
    summarize(scores_b, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(center: f64, n: usize, seed: u64) -> Vec<f64> {
        let mut r = Rng::new(seed);
        (0..n).map(|_| center + r.normal() * 0.5).collect()
    }

    #[test]
    fn detects_real_effect_and_null() {
        let info = metric_info("judge_score");
        let real: Vec<_> = (0..6).map(|t| (noisy(5.0 + t as f64 * 0.2, 5, t), noisy(7.0 + t as f64 * 0.2, 5, 100 + t))).collect();
        let e = paired_effect(&real, &info, 1).unwrap();
        assert!((e.effect - 2.0).abs() < 0.5, "{e:?}");
        assert!(e.ci_low > 0.0 && e.verdict == Verdict::Better);
        assert!(e.ci_low < e.effect && e.effect < e.ci_high);

        let null: Vec<_> = (0..8).map(|t| (noisy(6.0, 8, t), noisy(6.0, 8, 200 + t))).collect();
        let e = paired_effect(&null, &info, 1).unwrap();
        assert!(e.ci_low < 0.0 && e.ci_high > 0.0);
        assert_eq!(e.verdict, Verdict::Neutral, "{e:?}");

        let thin = vec![(vec![3.0, 9.0], vec![8.0, 2.0])];
        assert_eq!(paired_effect(&thin, &info, 1).unwrap().verdict, Verdict::Inconclusive);
    }

    #[test]
    fn direction_decides_better_or_worse() {
        let units: Vec<_> = (0..5).map(|t| (noisy(1.0, 5, t), noisy(3.0, 5, 50 + t))).collect();
        assert_eq!(paired_effect(&units, &metric_info("cost_usd"), 1).unwrap().verdict, Verdict::Worse);
        assert_eq!(paired_effect(&units, &metric_info("turns"), 1).unwrap().verdict, Verdict::Higher);
        assert!(paired_effect(&[(vec![], vec![1.0])], &metric_info("pass"), 1).is_none());
        // One run a side is never conclusive.
        assert_eq!(paired_effect(&[(vec![1.0], vec![9.0])], &metric_info("judge_score"), 1).unwrap().verdict, Verdict::Inconclusive);
    }

    #[test]
    fn contrast_finds_an_interaction() {
        // The pattern adds 2 points in group A and nothing in group B.
        let a: Vec<_> = (0..5).map(|t| (noisy(5.0, 5, t), noisy(7.0, 5, 30 + t))).collect();
        let b: Vec<_> = (0..5).map(|t| (noisy(7.0, 5, 60 + t), noisy(7.0, 5, 90 + t))).collect();
        let c = effect_difference(&a, &b, 1).unwrap();
        assert!((c.diff + 2.0).abs() < 0.6 && c.differs && c.ci_high < 0.0, "{c:?}");
        let same = effect_difference(&a, &a, 1).unwrap();
        assert!(!same.differs && same.diff.abs() < 1e-9);
        assert!(effect_difference(&a, &[], 1).is_none());
    }

    #[test]
    fn bootstrap_is_reproducible() {
        let units: Vec<_> = (0..4).map(|t| (noisy(5.0, 5, t), noisy(5.5, 5, 9 + t))).collect();
        let a = paired_effect(&units, &metric_info("judge_score"), 42).unwrap();
        let b = paired_effect(&units, &metric_info("judge_score"), 42).unwrap();
        assert_eq!((a.ci_low, a.ci_high), (b.ci_low, b.ci_high));
    }

    #[test]
    fn bradley_terry_recovers_order() {
        // True strengths 4 : 2 : 1.
        let truth = [4.0, 2.0, 1.0];
        let mut rng = Rng::new(3);
        let mut bouts = vec![];
        for _ in 0..300 {
            for (i, j) in [(0, 1), (0, 2), (1, 2)] {
                let p = truth[i] / (truth[i] + truth[j]);
                bouts.push(Bout { i, j, score_i: if rng.f64() < p { 1.0 } else { 0.0 } });
            }
        }
        let s = bradley_terry(3, &bouts);
        assert!(s[0] > s[1] && s[1] > s[2]);
        assert!((s[0] / s[2] - 4.0).abs() < 1.0, "{s:?}");
        let ci = bradley_terry_ci(3, &bouts, 1);
        assert!(ci[0].ci_low > ci[2].ci_high);
        // An unbeaten item stays finite.
        let sweep = bradley_terry(2, &[Bout { i: 0, j: 1, score_i: 1.0 }; 5]);
        assert!(sweep[0] > 0.9 && sweep[0] < 1.0);
    }

    #[test]
    fn flakiness() {
        assert!(is_flaky(&[1.0, 0.0, 1.0, 0.0, 1.0], Scale::Rate));
        assert!(!is_flaky(&[1.0, 1.0, 1.0, 1.0, 1.0], Scale::Rate));
        assert!(is_flaky(&[2.0, 8.0, 5.0, 9.0, 4.0], Scale::Score10));
        assert!(!is_flaky(&[7.0, 8.0, 7.0, 8.0, 7.0], Scale::Score10));
        // Three runs that split are too few to call.
        assert!(!is_flaky(&[1.0, 0.0, 1.0], Scale::Rate));
        assert!(!is_flaky(&[2.0, 8.0, 5.0, 9.0], Scale::Score10));
    }

    #[test]
    fn trigger_precision_recall_and_paired_effect() {
        let t = |should: bool, fired: &[f64]| TriggerTask { should, fired: fired.to_vec() };
        // 4 should-fire tasks (fires on 3), 4 should-not (fires on 1).
        let tasks = vec![t(true, &[1.0, 1.0]), t(true, &[1.0, 1.0]), t(true, &[1.0, 1.0]), t(true, &[0.0, 0.0]), t(false, &[1.0, 1.0]), t(false, &[0.0, 0.0]), t(false, &[0.0, 0.0]), t(false, &[0.0, 0.0])];
        let v = |m: &str| trigger_value(&tasks.iter().collect::<Vec<_>>(), m).unwrap();
        assert!((v("trigger_precision") - 0.75).abs() < 1e-9);
        assert!((v("trigger_recall") - 0.75).abs() < 1e-9);
        assert!((v("trigger_f1") - 0.75).abs() < 1e-9);
        let s = trigger_summary(&tasks, "trigger_recall", 1).unwrap();
        assert!(s.ci_low < 0.75 && s.ci_high >= 0.75 && s.n == 8, "{s:?}");
        // Nothing fired: precision is undefined, recall and F1 are zero.
        let silent = vec![t(true, &[0.0]), t(false, &[0.0])];
        let sv = |m: &str| trigger_value(&silent.iter().collect::<Vec<_>>(), m);
        assert_eq!((sv("trigger_precision"), sv("trigger_recall"), sv("trigger_f1")), (Option::None, Some(0.0), Some(0.0)));
        // B fires on every should task and no should-not task; A is the set above.
        let perfect: Vec<TriggerTask> = tasks.iter().map(|x| t(x.should, if x.should { &[1.0, 1.0] } else { &[0.0, 0.0] })).collect();
        let units: Vec<(TriggerTask, TriggerTask)> = tasks.iter().cloned().zip(perfect.iter().cloned()).collect();
        let e = trigger_effect(&units, "trigger_f1", 1).unwrap();
        assert!((e.effect - 0.25).abs() < 1e-9 && e.units == 8, "{e:?}");
        let same: Vec<(TriggerTask, TriggerTask)> = perfect.iter().cloned().zip(perfect.iter().cloned()).collect();
        assert_eq!(trigger_effect(&same, "trigger_f1", 1).unwrap().verdict, Verdict::NoHeadroom);
    }

    #[test]
    fn saturation_levels() {
        let (pass, score, cost) = (metric_info("pass"), metric_info("judge_score"), metric_info("cost_usd"));
        let h = |xs: &[f64], i: &MetricInfo| saturation(xs, i).describe();
        assert_eq!(h(&[1.0, 1.0, 1.0], &pass), "none (ceiling)");
        assert_eq!(h(&[0.0, 0.0, 0.0, 0.0], &pass), "none (floor)");
        assert_eq!(h(&[1.0; 2], &pass), "ok", "two runs are too few to call saturated");
        assert_eq!(h(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0], &pass), "little (ceiling)");
        assert_eq!(h(&[1.0, 0.0, 1.0], &pass), "ok");
        assert_eq!(h(&[9.0, 9.0, 9.0, 9.0, 10.0], &score), "none (ceiling)");
        assert_eq!(h(&[10.0, 8.0, 10.0], &score), "little (ceiling)", "at the bound but with spread");
        assert_eq!(h(&[9.0, 8.0, 9.0], &score), "little (ceiling)");
        assert_eq!(h(&[2.0, 2.0, 2.0, 1.0, 2.0], &score), "none (floor)");
        assert_eq!(h(&[6.0, 7.0, 6.0], &score), "ok");
        assert_eq!(h(&[0.0, 0.0, 0.0], &cost), "ok");
    }

    #[test]
    fn saturated_units_are_dropped_and_all_saturated_is_no_headroom() {
        let pass = metric_info("pass");
        // Every run passes in both arms: the data cannot show a benefit.
        let all_pass = vec![(vec![1.0; 3], vec![1.0; 3]), (vec![1.0; 5], vec![1.0; 5])];
        let e = paired_effect(&all_pass, &pass, 1).unwrap();
        assert_eq!(e.verdict, Verdict::NoHeadroom);
        assert_eq!((e.units, e.saturated_units, e.effect), (2, 2, 0.0));
        assert!(!e.verdict.is_conclusive_change());
        // Mixed: the saturated tasks are left out and counted.
        let mut mixed = all_pass.clone();
        mixed.extend((0..4).map(|t| (vec![0.0, 0.0, 1.0, 0.0, 0.0], if t % 2 == 0 { vec![1.0, 1.0, 1.0, 0.0, 1.0] } else { vec![1.0; 5] })));
        let e = paired_effect(&mixed, &pass, 1).unwrap();
        assert_eq!((e.units, e.saturated_units), (4, 2));
        assert_eq!(e.verdict, Verdict::Better, "{e:?}");
        assert!(e.effect > 0.6, "the effect is over the unsaturated tasks only: {e:?}");
        // A pattern that breaks a passing task is a real effect, not a saturated unit.
        let broke = vec![(vec![1.0; 5], vec![0.0; 5]), (vec![1.0; 5], vec![0.0, 0.0, 1.0, 0.0, 0.0])];
        let e = paired_effect(&broke, &pass, 1).unwrap();
        assert_eq!((e.verdict, e.saturated_units), (Verdict::Worse, 0));
        // Zero-width interval with several runs a side is never "no effect".
        let flat = vec![(vec![7.0; 4], vec![7.0; 4])];
        assert_eq!(paired_effect(&flat, &metric_info("judge_score"), 1).unwrap().verdict, Verdict::Inconclusive);
        let two = vec![(vec![1.0; 2], vec![1.0; 2])];
        assert_eq!(paired_effect(&two, &pass, 1).unwrap().verdict, Verdict::Inconclusive);
    }
}
