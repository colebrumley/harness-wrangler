//! Advice: concrete, measurable suggestions derived from a report. Each
//! names the metric and carries its evidence, and the whole list exports as
//! JSON so an agent can act on it.

use crate::report::{self, Comparison, Report};
use crate::review;
use crate::stats::{self, Effect, Verdict};
use crate::store::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: String,
    /// remove_section | review_section | keep_section | retire_pattern | drop_pattern |
    /// keep_pattern | fix_trigger | trim_body | change_level | tradeoff | flaky_task |
    /// more_runs | judge_bias | reviewer_flag | failures
    pub kind: String,
    /// 1 is most urgent.
    pub priority: u32,
    /// The suggestion as one imperative sentence with its numbers.
    pub action: String,
    pub target: Value,
    pub metric: String,
    pub evidence: Value,
    /// high: CI excludes zero. medium: CI inside the equivalence band. low: heuristic.
    pub confidence: String,
}

fn evidence(e: &Effect, launch_id: i64) -> Value {
    json!({
        "effect": e.effect, "ci_low": e.ci_low, "ci_high": e.ci_high, "relative": e.rel,
        "baseline_mean": e.base_mean, "treatment_mean": e.treat_mean,
        "paired_units": e.units, "runs_baseline": e.n_a, "runs_treatment": e.n_b,
        "verdict": e.verdict.as_str(), "launch_id": launch_id,
    })
}

fn ci(info: &stats::MetricInfo, e: &Effect) -> String {
    format!("{} [{}, {}]", report::fmt_delta(info, e.effect), report::fmt_delta(info, e.ci_low), report::fmt_delta(info, e.ci_high))
}

/// "lowers judge score by 0.94 (−0.94 [−1.44, −0.44])"
fn moves(info: &stats::MetricInfo, e: &Effect) -> String {
    let verb = if e.effect < 0.0 { "lowers" } else { "raises" };
    format!("{verb} {} ({})", info.label.to_lowercase(), ci(info, e))
}

fn pct(x: Option<f64>) -> String {
    x.map(|r| format!(" ({:+.0}%)", r * 100.0)).unwrap_or_default()
}

/// Runs needed per cell to resolve an effect of the observed size, from the
/// current CI half-width (which shrinks with the square root of n).
fn runs_needed(e: &Effect, current: u32) -> Option<u32> {
    let half = (e.ci_high - e.ci_low) / 2.0;
    if e.effect.abs() < 1e-9 || half <= 0.0 {
        return None;
    }
    let factor = (half / e.effect.abs()).powi(2);
    let n = (current as f64 * factor).ceil();
    (n > current as f64 && n <= 200.0).then_some(n as u32)
}

pub fn build(store: &Store, launch_id: i64, metric: Option<&str>) -> Result<Vec<Suggestion>> {
    let r = report::build(store, launch_id, metric)?;
    let launch = store.launch(launch_id)?;
    let pm = r.primary_metric.clone();
    let pinfo = stats::metric_info(&pm);
    let cost_info = stats::metric_info("cost_usd");
    let model = r.variants.first().and_then(|v| v.model.clone()).map(|m| format!(" on {m}")).unwrap_or_default();
    let single_model = r.factors.iter().all(|f| f.kind != "model");
    let on_model = if single_model { model.as_str() } else { "" };
    let mut out: Vec<Suggestion> = vec![];
    let mut push = |kind: &str, priority: u32, action: String, target: Value, metric: &str, evidence: Value, confidence: &str| {
        out.push(Suggestion { id: String::new(), kind: kind.into(), priority, action, target, metric: metric.into(), evidence, confidence: confidence.into() });
    };

    // ---- ablation: per-section attribution
    if let Some(ab) = &r.ablation {
        let target = |s: &report::SectionAttribution| json!({"type": "section", "file": ab.source, "section": s.index + 1, "title": s.title, "lines": [s.lines.0, s.lines.1]});
        for s in &ab.sections {
            let Some(e) = &s.effect else { continue };
            let saved = s.tokens_effect.as_ref().filter(|t| t.effect > 0.0).map(|t| format!("measured {} per run", report::fmt_delta(&stats::metric_info("total_tokens"), -t.effect))).unwrap_or_else(|| format!("~{} tokens of instructions", s.est_tokens));
            let mut ev = evidence(e, launch_id);
            ev["est_section_tokens"] = json!(s.est_tokens);
            ev["tokens_effect"] = json!(s.tokens_effect.as_ref().map(|t| t.effect));
            ev["cost_effect"] = json!(s.cost_effect.as_ref().map(|t| t.effect));
            match s.attribution.as_str() {
                "hurts" => push("review_section", 1, format!("Review or remove section {} \"{}\": it {}{on_model}.", s.index + 1, s.title, moves(&pinfo, e)), target(s), &pm, ev, "high"),
                "no_effect" => push("remove_section", 2, format!("Remove section {} \"{}\": no measurable effect on {} ({}){on_model}; saves {saved}.", s.index + 1, s.title, pinfo.label.to_lowercase(), ci(&pinfo, e)), target(s), &pm, ev, "medium"),
                "carries" => push("keep_section", 8, format!("Keep section {} \"{}\": it contributes {} to {}{on_model}.", s.index + 1, s.title, ci(&pinfo, e), pinfo.label.to_lowercase()), target(s), &pm, ev, "high"),
                _ => {}
            }
        }
        let file_target = json!({"type": "file", "file": ab.source});
        match ab.full_effect.as_ref().map(|e| (e, e.verdict)) {
            Some((e, Verdict::Neutral)) => push("retire_pattern", 1, format!("Consider retiring {}: with every section in place it has no measurable effect on {} ({}){on_model}.", ab.source, pinfo.label.to_lowercase(), ci(&pinfo, e)), file_target.clone(), &pm, evidence(e, launch_id), "medium"),
            Some((e, Verdict::Worse)) | Some((e, Verdict::Lower)) => push("retire_pattern", 1, format!("Remove {}: as a whole it {}{on_model}.", ab.source, moves(&pinfo, e)), file_target.clone(), &pm, evidence(e, launch_id), "high"),
            _ => {}
        }
        if let (Some(d), Some(b)) = (&ab.description_only_effect, &ab.body_effect) {
            if d.verdict == Verdict::Better && b.verdict == Verdict::Neutral {
                push("trim_body", 2, format!("The description alone delivers the gain ({}); the body adds no measurable {} beyond it ({}). Trim the body.", ci(&pinfo, d), pinfo.label.to_lowercase(), ci(&pinfo, b)), file_target.clone(), &pm, evidence(b, launch_id), "medium");
            }
        }
    }

    // ---- skills that do not trigger
    for v in r.variants.iter().filter(|v| v.metrics.contains_key("skill_invoked")) {
        let silent: Vec<&str> = r
            .tasks
            .iter()
            .filter(|t| t.cells.iter().any(|c| c.variant_key == v.key && c.metrics.get("skill_invoked").map(|s| s.n >= 2 && s.mean == 0.0).unwrap_or(false)))
            .map(|t| t.task_id.as_str())
            .collect();
        let rate = v.metrics["skill_invoked"].mean;
        if !silent.is_empty() {
            push(
                "fix_trigger",
                2,
                format!("Skill never triggered on task{} {} in variant \"{}\" (invoked in {:.0}% of runs overall); the description may not match these prompts.", if silent.len() == 1 { "" } else { "s" }, silent.join(", "), v.label, rate * 100.0),
                json!({"type": "variant", "variant": v.label, "tasks": silent}),
                "skill_invoked",
                json!({"invoked_rate": rate, "runs": v.metrics["skill_invoked"].n, "tasks_never_triggered": silent, "launch_id": launch_id}),
                "high",
            );
        } else if rate < 0.5 && v.metrics["skill_invoked"].n >= 4 {
            push("fix_trigger", 3, format!("Skill triggered in only {:.0}% of runs of variant \"{}\"; tighten its description.", rate * 100.0, v.label), json!({"type": "variant", "variant": v.label}), "skill_invoked", json!({"invoked_rate": rate, "runs": v.metrics["skill_invoked"].n, "launch_id": launch_id}), "medium");
        }
    }

    // ---- factor levels (ablation overlays are covered above)
    let find = |f: &str, level: &str, metric: &str| -> Option<&Comparison> { r.comparisons.iter().find(|c| c.kind == "factor" && c.factor.as_deref() == Some(f) && c.level == level && c.metric == metric) };
    for f in &r.factors {
        if r.ablation.is_some() && launch.def.ablation.as_ref().map(|a| a.factor == f.name).unwrap_or(false) {
            continue;
        }
        let pattern = f.kind == "overlay" || f.kind == "system_prompt_append";
        for level in f.levels.iter().skip(1) {
            let Some(q) = find(&f.name, level, &pm) else { continue };
            let cost = find(&f.name, level, "cost_usd");
            let cost_txt = cost.map(|c| format!("cost {}{}", report::fmt_delta(&cost_info, c.effect.effect), pct(c.effect.rel))).unwrap_or_else(|| "cost not reported".into());
            let target = json!({"type": "factor_level", "factor": f.name, "kind": f.kind, "level": level, "baseline": f.levels[0]});
            let mut ev = evidence(&q.effect, launch_id);
            ev["cost_effect"] = json!(cost.map(|c| c.effect.effect));
            ev["cost_relative"] = json!(cost.and_then(|c| c.effect.rel));
            let cost_v = cost.map(|c| c.effect.verdict);
            let label = pinfo.label.to_lowercase();
            match (q.effect.verdict, pattern) {
                (Verdict::Worse, true) => push("drop_pattern", 1, format!("Drop \"{level}\": it {} against \"{}\"; {cost_txt}.", moves(&pinfo, &q.effect), f.levels[0]), target, &pm, ev, "high"),
                (Verdict::Neutral, true) => push("retire_pattern", 2, format!("\"{level}\" has no measurable effect on {label} ({}) against \"{}\"; {cost_txt}. Consider retiring it.", ci(&pinfo, &q.effect), f.levels[0]), target, &pm, ev, "medium"),
                (Verdict::Better, true) => push("keep_pattern", 8, format!("Keep \"{level}\": it improves {label} by {} against \"{}\"; {cost_txt}.", ci(&pinfo, &q.effect), f.levels[0]), target, &pm, ev, "high"),
                (Verdict::Worse, false) => push("change_level", 2, format!("Prefer {} = \"{}\" over \"{level}\": \"{level}\" {}; {cost_txt}.", f.name, f.levels[0], moves(&pinfo, &q.effect)), target, &pm, ev, "high"),
                (Verdict::Neutral, false) if cost_v == Some(Verdict::Worse) => push("change_level", 2, format!("Use {} = \"{}\" instead of \"{level}\": no measurable {label} gain ({}) and {cost_txt}.", f.name, f.levels[0], ci(&pinfo, &q.effect)), target, &pm, ev, "medium"),
                (Verdict::Neutral, false) if cost_v == Some(Verdict::Better) => push("change_level", 3, format!("Switch {} to \"{level}\": {label} is unchanged ({}) and {cost_txt}.", f.name, ci(&pinfo, &q.effect)), target, &pm, ev, "medium"),
                (Verdict::Better, false) => push("tradeoff", 5, format!("{} = \"{level}\" improves {label} by {} over \"{}\"; {cost_txt}.", f.name, ci(&pinfo, &q.effect), f.levels[0]), target, &pm, ev, "high"),
                (Verdict::Inconclusive, _) => {
                    if let Some(n) = runs_needed(&q.effect, r.runs_per_cell) {
                        push("more_runs", 6, format!("\"{level}\" vs \"{}\" is inconclusive on {label} ({}); about {n} runs per cell would resolve an effect of this size (now {}).", f.levels[0], ci(&pinfo, &q.effect), r.runs_per_cell), target, &pm, ev, "low");
                    }
                }
                _ => {}
            }
        }
    }

    // ---- flaky tasks
    let mut flaky_tasks: Vec<&str> = r.flaky.iter().map(|f| f.task_id.as_str()).collect();
    flaky_tasks.sort();
    flaky_tasks.dedup();
    if let Some(worst) = r.flaky.iter().max_by(|a, b| a.sd.total_cmp(&b.sd)) {
        let cells: Vec<Value> = r.flaky.iter().map(|f| json!({"task": f.task_id, "variant": f.variant, "metric": f.metric, "mean": f.mean, "sd": f.sd, "n": f.n})).collect();
        let i = stats::metric_info(&worst.metric);
        push(
            "flaky_task",
            6,
            format!(
                "{} flaky in {} cell(s): for example {} on {} averages {} with sd {} in \"{}\" (n={}). Tighten the task or raise runs_per_cell above {}.",
                if flaky_tasks.len() == 1 { format!("Task {} is", flaky_tasks[0]) } else { format!("Tasks {} are", flaky_tasks.join(", ")) },
                cells.len(),
                i.label.to_lowercase(),
                worst.task_id,
                report::fmt_value(&i, worst.mean),
                report::fmt_value(&i, worst.sd),
                worst.variant,
                worst.n,
                r.runs_per_cell
            ),
            json!({"type": "tasks", "tasks": flaky_tasks}),
            &worst.metric,
            json!({"cells": cells, "launch_id": launch_id}),
            "low",
        );
    }

    // ---- judge reliability
    if let Some(pb) = r.pairwise.as_ref().and_then(|p| p.position_bias.as_ref()) {
        if pb.pairs >= 6 && (pb.inconsistent_rate > 0.3 || (pb.decided_picks >= 10 && (pb.first_position_rate - 0.5).abs() > 0.2)) {
            push(
                "judge_bias",
                3,
                format!("The pairwise judge is order-sensitive: {:.0}% of {} pairs flipped when the order was swapped, and the first-listed result won {:.0}% of decided picks. Lean on human review or a stronger judge model.", pb.inconsistent_rate * 100.0, pb.pairs, pb.first_position_rate * 100.0),
                json!({"type": "judge", "model": launch.def.judge().map(|j| j.model)}),
                "position_bias",
                serde_json::to_value(pb)?,
                "high",
            );
        }
    }

    // ---- what reviewers flagged repeatedly
    for va in review::annotation_digest(store, launch_id)? {
        for p in va.passages.iter().filter(|p| p.runs >= 2).take(5) {
            let note = p.comments.first().map(|c| format!(": \"{c}\"")).unwrap_or_default();
            push(
                "reviewer_flag",
                4,
                format!("Reviewers flagged \"{}\" in {} of {} reviewed runs of \"{}\"{note}.", crate::util::truncate_middle(&p.quote, 120), p.runs, va.reviewed_runs, va.variant),
                json!({"type": "variant", "variant": va.variant}),
                "annotations",
                json!({"quote": p.quote, "runs": p.runs, "reviewed_runs": va.reviewed_runs, "comments": p.comments, "emoji": p.emoji, "run_ids": p.run_ids, "launch_id": launch_id}),
                "low",
            );
        }
    }

    // ---- failures
    for v in &r.variants {
        let bad = v.counts.failed + v.counts.timeout;
        if bad > 0 && bad * 4 >= v.counts.total {
            push("failures", 2, format!("{} of {} runs of \"{}\" failed or timed out; results for it are thin. Check `wrangler runs {launch_id} --status failed`.", bad, v.counts.total, v.label), json!({"type": "variant", "variant": v.label}), "status", json!({"failed": v.counts.failed, "timeout": v.counts.timeout, "total": v.counts.total, "launch_id": launch_id}), "high");
        }
    }

    out.sort_by(|a, b| a.priority.cmp(&b.priority));
    for (i, s) in out.iter_mut().enumerate() {
        s.id = format!("L{launch_id}-{:02}", i + 1);
    }
    Ok(out)
}

pub fn render_text(r: &Report, suggestions: &[Suggestion]) -> String {
    let mut s = format!("advice for {} launch #{} (primary metric: {})\n", r.experiment, r.launch_id, r.primary_metric);
    if suggestions.is_empty() {
        s.push_str("no suggestions: nothing in this launch is conclusive enough to act on.\n");
    }
    for g in suggestions {
        s.push_str(&format!("\n{}  [{} · {} confidence]\n  {}\n", g.id, g.kind, g.confidence, g.action));
    }
    s
}
