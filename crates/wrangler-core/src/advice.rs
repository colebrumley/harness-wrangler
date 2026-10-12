//! Advice: concrete, measurable suggestions derived from a report. Each
//! names the metric and carries its evidence, and the whole list exports as
//! JSON so an agent can act on it.

use crate::calibration::{Calibration, CalibrationRow};
use crate::model::Task;
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
    /// keep_pattern | no_headroom | placebo_explains | calibrate_judge | trigger_misses | trigger_overfires | trim_body | change_level | tradeoff |
    /// flaky_task | more_runs | judge_bias | reviewer_flag | failures
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

    // ---- no headroom: a task problem, not a verdict on the pattern
    // Built once and attached wherever a comparison came back saturated.
    let no_headroom = |what: &str, e: &Effect| -> (String, Value) {
        let saturated: Vec<&report::TaskBreakdown> = r.tasks.iter().filter(|t| t.headroom.is_none()).collect();
        let floor = !saturated.is_empty() && saturated.iter().all(|t| t.headroom.at.as_deref() == Some("floor"));
        // Quality and size metrics whose baseline still moves from run to run.
        let base = r.variants.first();
        let varying: Vec<String> = ["judge_score", "expectations_passed", "checks_passed", "diff_lines", "output_words", "turns"]
            .iter()
            .filter(|m| **m != pm && base.and_then(|v| v.metrics.get(**m)).map(|x| x.n >= 2 && x.sd > 0.0).unwrap_or(false))
            .map(|m| m.to_string())
            .collect();
        let state = match (pm.as_str(), floor) {
            ("pass", false) => "Every task passes without the pattern".to_string(),
            ("pass", true) => "Every task fails with and without the pattern".to_string(),
            (_, false) => format!("Every task is already at the top of {} without the pattern", pinfo.label.to_lowercase()),
            (_, true) => format!("Every task sits at the bottom of {} with and without the pattern", pinfo.label.to_lowercase()),
        };
        let fix = if floor { "Add tasks the agent can sometimes do, or switch the primary metric." } else { "Add harder tasks or switch the primary metric." };
        let alt = match (varying.is_empty(), pm.as_str()) {
            (false, _) => format!(" Metrics that still vary here: {}.", varying.join(", ")),
            (true, "pass") => " A judge score or a `diff_size` bound would still vary.".to_string(),
            _ => String::new(),
        };
        let mut ev = evidence(e, launch_id);
        ev["saturated_tasks"] = json!(saturated.iter().map(|t| json!({"task": t.task_id, "headroom": t.headroom.headroom, "at": t.headroom.at})).collect::<Vec<_>>());
        ev["metrics_that_still_vary"] = json!(varying);
        ev["suggested_metrics"] = json!(if pm == "pass" { vec!["judge_score", "diff_size bound"] } else { vec![] });
        (format!("{state}{on_model}; the experiment cannot show {what} on {}. {fix}{alt}", pinfo.label.to_lowercase()), ev)
    };

    // ---- ablation: per-section attribution
    if let Some(ab) = &r.ablation {
        let target = |s: &report::SectionAttribution| json!({"type": "section", "file": ab.source, "section": s.index + 1, "title": s.title, "lines": [s.lines.0, s.lines.1]});
        let part = if ab.mode == "description_sentences" { "description sentence" } else { "section" };
        for s in &ab.sections {
            let Some(e) = &s.effect else { continue };
            let saved = s.tokens_effect.as_ref().filter(|t| t.effect > 0.0).map(|t| format!("measured {} per run", report::fmt_delta(&stats::metric_info("total_tokens"), -t.effect))).unwrap_or_else(|| format!("~{} tokens of instructions", s.est_tokens));
            let mut ev = evidence(e, launch_id);
            ev["est_section_tokens"] = json!(s.est_tokens);
            ev["tokens_effect"] = json!(s.tokens_effect.as_ref().map(|t| t.effect));
            ev["cost_effect"] = json!(s.cost_effect.as_ref().map(|t| t.effect));
            match s.attribution.as_str() {
                "hurts" => push("review_section", 1, format!("Review or remove {part} {} \"{}\": it {}{on_model}.", s.index + 1, s.title, moves(&pinfo, e)), target(s), &pm, ev, "high"),
                "no_effect" => push("remove_section", 2, format!("Remove {part} {} \"{}\": no measurable effect on {} ({}){on_model}; saves {saved}.", s.index + 1, s.title, pinfo.label.to_lowercase(), ci(&pinfo, e)), target(s), &pm, ev, "medium"),
                "carries" => push("keep_section", 8, format!("Keep {part} {} \"{}\": it contributes {} to {}{on_model}.", s.index + 1, s.title, ci(&pinfo, e), pinfo.label.to_lowercase()), target(s), &pm, ev, "high"),
                _ => {}
            }
        }
        let file_target = json!({"type": "file", "file": ab.source});
        if let Some(e) = ab.full_effect.as_ref().filter(|e| e.verdict == Verdict::Inconclusive) {
            let need = runs_needed(e, r.runs_per_cell).map(|n| format!(" About {n} runs per cell would resolve an effect of this size (now {}).", r.runs_per_cell)).unwrap_or_default();
            push("more_runs", 2, format!("The whole file's effect on {} is inconclusive ({}){on_model}, so no section can be attributed yet.{need} Adding tasks narrows the interval faster than adding repetitions when there are only {} paired task(s).", pinfo.label.to_lowercase(), ci(&pinfo, e), e.units), file_target.clone(), &pm, evidence(e, launch_id), "low");
        }
        match ab.full_effect.as_ref().map(|e| (e, e.verdict)) {
            Some((e, Verdict::NoHeadroom)) => {
                let (action, ev) = no_headroom(&format!("a benefit from {}", ab.source), e);
                push("no_headroom", 1, action, file_target.clone(), &pm, ev, "high");
            }
            Some((e, Verdict::Neutral)) => push("retire_pattern", 1, format!("Consider retiring {}: with every section in place it has no measurable effect on {} ({}){on_model}.", ab.source, pinfo.label.to_lowercase(), ci(&pinfo, e)), file_target.clone(), &pm, evidence(e, launch_id), "medium"),
            Some((e, Verdict::Worse)) | Some((e, Verdict::Lower)) => push("retire_pattern", 1, format!("Remove {}: as a whole it {}{on_model}.", ab.source, moves(&pinfo, e)), file_target.clone(), &pm, evidence(e, launch_id), "high"),
            _ => {}
        }
        if let Some(p) = ab.placebo.as_ref().filter(|p| p.attribution == "presence") {
            if let (Some(full), Some(fvp), Some(pvn)) = (&ab.full_effect, &p.full_vs_placebo, &p.placebo_vs_none) {
                push(
                    "placebo_explains",
                    1,
                    format!("The gain comes from having instructions, not from this content; a shorter file would do. A placebo of the same length matches {} on {} (full − placebo {}) and beats no file ({}){on_model}.", ab.source, pinfo.label.to_lowercase(), ci(&pinfo, fvp), ci(&pinfo, pvn)),
                    file_target.clone(),
                    &pm,
                    json!({"attribution": "presence", "full_vs_none": evidence(full, launch_id), "full_vs_placebo": evidence(fvp, launch_id), "placebo_vs_none": evidence(pvn, launch_id), "launch_id": launch_id}),
                    "medium",
                );
            }
        }
        if let (Some(d), Some(b)) = (&ab.description_only_effect, &ab.body_effect) {
            if d.verdict == Verdict::Better && b.verdict == Verdict::Neutral {
                push("trim_body", 2, format!("The description alone delivers the gain ({}); the body adds no measurable {} beyond it ({}). Trim the body.", ci(&pinfo, d), pinfo.label.to_lowercase(), ci(&pinfo, b)), file_target.clone(), &pm, evidence(b, launch_id), "medium");
            }
        }
    }

    // ---- triggering: misses (recall) and overfires (precision)
    let describe = |variant_key: &str| -> Value {
        let skills = launch.variants.iter().find(|v| v.key == variant_key).map(|v| v.skills.clone()).unwrap_or_default();
        json!(skills.iter().map(|s| json!({"skill": s.name, "description": s.description})).collect::<Vec<_>>())
    };
    if let Some(t) = &r.trigger {
        let prompts = |ps: &[report::TriggerPrompt]| json!(ps.iter().map(|p| json!({"task": p.task_id, "prompt": p.prompt, "fire_rate": p.fire_rate, "runs": p.runs})).collect::<Vec<_>>());
        let names = |ps: &[report::TriggerPrompt]| ps.iter().map(|p| p.task_id.as_str()).collect::<Vec<_>>().join(", ");
        // The sentence whose removal raised precision the most, when a description ablation ran.
        let culprit = r.ablation.as_ref().filter(|a| a.mode == "description_sentences").and_then(|a| {
            a.sections.iter().filter_map(|s| Some((s, s.precision_effect.as_ref()?))).filter(|(_, e)| e.effect < 0.0).min_by(|x, y| x.1.effect.total_cmp(&y.1.effect)).map(|(s, e)| (s.title.clone(), e.clone()))
        });
        for v in &t.variants {
            // A variant that does not install the skill has nothing to tune.
            let Some(description) = &v.description else { continue };
            let target = json!({"type": "skill_description", "skills": t.skills, "variant": v.label, "model": v.model});
            let on = v.model.as_ref().filter(|_| !single_model).map(|m| format!(" on {m}")).unwrap_or_default();
            if let Some(recall) = v.recall.as_ref().filter(|x| !v.false_negatives.is_empty() || x.mean < 0.8) {
                push(
                    "trigger_misses",
                    2,
                    format!("Skill missed {} of {} should-fire prompt(s) in \"{}\"{on} (recall {:.0}%): {}. The description does not cover these; widen it.", v.false_negatives.len(), v.should_tasks, v.label, recall.mean * 100.0, if v.false_negatives.is_empty() { "it fired on each at least once but not reliably".to_string() } else { names(&v.false_negatives) }),
                    target.clone(),
                    "trigger_recall",
                    json!({"recall": recall.mean, "ci_low": recall.ci_low, "ci_high": recall.ci_high, "missed_prompts": prompts(&v.false_negatives), "description": description, "should_tasks": v.should_tasks, "launch_id": launch_id}),
                    if v.false_negatives.is_empty() { "medium" } else { "high" },
                );
            }
            if let Some(precision) = v.precision.as_ref().filter(|_| !v.false_positives.is_empty()) {
                let clause = culprit.as_ref().map(|(text, e)| format!(" The sentence most likely responsible: \"{}\" (removing it changes precision by {}).", crate::util::clip(text, 140), report::fmt_delta(&stats::metric_info("trigger_precision"), -e.effect))).unwrap_or_default();
                push(
                    "trigger_overfires",
                    2,
                    format!("Skill fired on {} of {} should-not prompt(s) in \"{}\"{on} (precision {:.0}%): {}. Narrow the description.{clause}", v.false_positives.len(), v.should_not_tasks, v.label, precision.mean * 100.0, names(&v.false_positives)),
                    target,
                    "trigger_precision",
                    json!({"precision": precision.mean, "ci_low": precision.ci_low, "ci_high": precision.ci_high, "fired_prompts": prompts(&v.false_positives), "description": description, "should_not_tasks": v.should_not_tasks,
                        "responsible_sentence": culprit.as_ref().map(|(text, e)| json!({"text": text, "precision_effect": e.effect, "ci_low": e.ci_low, "ci_high": e.ci_high})), "launch_id": launch_id}),
                    "high",
                );
            }
        }
    }
    // Outside a trigger experiment every task was written for the skill, so a silent skill is a miss.
    for v in r.variants.iter().filter(|v| r.trigger.is_none() && v.metrics.contains_key("skill_invoked")) {
        let silent: Vec<&str> = r
            .tasks
            .iter()
            .filter(|t| t.cells.iter().any(|c| c.variant_key == v.key && c.metrics.get("skill_invoked").map(|s| s.n >= 2 && s.mean == 0.0).unwrap_or(false)))
            .map(|t| t.task_id.as_str())
            .collect();
        let rate = v.metrics["skill_invoked"].mean;
        let prompts: Vec<Value> = silent.iter().map(|id| json!({"task": id, "prompt": launch.tasks.iter().find(|t| t.id == *id).map(|t| t.prompt.clone())})).collect();
        if !silent.is_empty() {
            push(
                "trigger_misses",
                2,
                format!("Skill never triggered on task{} {} in variant \"{}\" (invoked in {:.0}% of runs overall); the description may not match these prompts.", if silent.len() == 1 { "" } else { "s" }, silent.join(", "), v.label, rate * 100.0),
                json!({"type": "variant", "variant": v.label, "tasks": silent}),
                "skill_invoked",
                json!({"invoked_rate": rate, "runs": v.metrics["skill_invoked"].n, "tasks_never_triggered": silent, "missed_prompts": prompts, "description": describe(&v.key), "launch_id": launch_id}),
                "high",
            );
        } else if rate < 0.5 && v.metrics["skill_invoked"].n >= 4 {
            push("trigger_misses", 3, format!("Skill triggered in only {:.0}% of runs of variant \"{}\"; tighten its description.", rate * 100.0, v.label), json!({"type": "variant", "variant": v.label}), "skill_invoked", json!({"invoked_rate": rate, "runs": v.metrics["skill_invoked"].n, "description": describe(&v.key), "launch_id": launch_id}), "medium");
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
                (Verdict::NoHeadroom, _) => {
                    let (action, ev0) = no_headroom(&format!("a difference between \"{level}\" and \"{}\"", f.levels[0]), &q.effect);
                    push("no_headroom", 1, action, target, &pm, ev0, "high");
                }
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

    // ---- how far the judge can be trusted on the tasks behind each suggestion
    if let Some(cal) = &r.calibration {
        apply_calibration(&mut out, cal, &launch.tasks);
        if let Some(s) = calibrate_judge(cal, &launch.tasks, launch_id, launch.def.human_enabled()) {
            out.push(s);
        }
    }

    out.sort_by(|a, b| a.priority.cmp(&b.priority));
    for (i, s) in out.iter_mut().enumerate() {
        s.id = format!("L{launch_id}-{:02}", i + 1);
    }
    Ok(out)
}

/// Task ids a suggestion rests on, when it names them; `None` means the whole launch.
fn scope_tasks(s: &Suggestion) -> Option<Vec<String>> {
    let strings = |v: &Value| -> Vec<String> { v.as_array().into_iter().flatten().filter_map(|x| x.as_str().or(x["task"].as_str())).map(|x| x.to_string()).collect() };
    let mut ids = strings(&s.target["tasks"]);
    ids.extend(s.target["task"].as_str().map(|x| x.to_string()));
    ids.extend(strings(&s.evidence["cells"]));
    ids.sort();
    ids.dedup();
    (!ids.is_empty()).then_some(ids)
}

/// The calibration rows that cover a set of tasks: one per tag the tasks
/// carry, or the overall row for the whole launch and for untagged tasks.
fn rows_for<'a>(cal: &'a Calibration, tasks: &[Task], scope: &Option<Vec<String>>) -> Vec<&'a CalibrationRow> {
    let Some(ids) = scope else { return vec![&cal.overall] };
    let mut tags: Vec<&str> = tasks.iter().filter(|t| ids.contains(&t.id)).flat_map(|t| t.tags.iter().map(|x| x.as_str())).collect();
    tags.sort();
    tags.dedup();
    let rows: Vec<&CalibrationRow> = tags.iter().filter_map(|t| cal.tag(t)).collect();
    if rows.is_empty() {
        vec![&cal.overall]
    } else {
        rows
    }
}

/// Suggestions that rest on the judge are only as good as the judge. Where
/// its agreement with human votes is below 0.7, or there are too few votes
/// to say, each such suggestion drops one confidence step and says why. A
/// suggestion about named tasks is held to the calibration of those tasks'
/// tags; one about the whole launch, to the overall figure.
pub fn apply_calibration(suggestions: &mut [Suggestion], cal: &Calibration, tasks: &[Task]) {
    for s in suggestions.iter_mut().filter(|s| s.metric == "judge_score" || s.metric == "expectations_passed") {
        let scope = scope_tasks(s);
        let untrusted: Vec<&CalibrationRow> = rows_for(cal, tasks, &scope).into_iter().filter(|r| !r.trusted()).collect();
        let Some(worst) = untrusted.iter().min_by(|a, b| a.agreement.unwrap_or(-1.0).total_cmp(&b.agreement.unwrap_or(-1.0))) else { continue };
        s.confidence = match s.confidence.as_str() {
            "high" => "medium",
            _ => "low",
        }
        .into();
        let agreement = worst.agreement.or(worst.rubric_agreement).map(|a| format!("agreement {:.0}%", a * 100.0)).unwrap_or_else(|| "no agreement figure".into());
        let n = worst.pairs.max(worst.rubric_pairs);
        let more: usize = untrusted.iter().map(|r| r.pairs_needed).sum();
        let ask = if more > 0 { format!("review {more} more pair{}", if more == 1 { "" } else { "s" }) } else { "review more pairs, or switch to a stronger judge model".to_string() };
        let which = if worst.scope == "all" { String::new() } else { format!(" tagged `{}`", untrusted.iter().map(|r| r.scope.as_str()).collect::<Vec<_>>().join("`, `")) };
        s.action.push_str(&format!(" The judge is not calibrated on these tasks{which} ({agreement} on {n} pair{}); {ask}.", if n == 1 { "" } else { "s" }));
        s.evidence["judge_calibration"] = json!({"status": worst.status, "agreement": worst.agreement, "rubric_agreement": worst.rubric_agreement, "pairs": n, "pairs_needed": more, "untrusted": untrusted.iter().map(|r| r.scope.clone()).collect::<Vec<_>>()});
    }
}

/// How many pairs to review so every tag reaches the minimum.
fn calibrate_judge(cal: &Calibration, tasks: &[Task], launch_id: i64, human_review: bool) -> Option<Suggestion> {
    let tagged = tasks.iter().any(|t| !t.tags.is_empty());
    let rows: Vec<&CalibrationRow> = if tagged { cal.by_tag.iter().collect() } else { vec![&cal.overall] };
    // Tags no human has voted on yet have no row; they need the full minimum.
    let mut missing: Vec<String> = tasks.iter().flat_map(|t| t.tags.iter().cloned()).filter(|t| cal.tag(t).is_none()).collect();
    missing.sort();
    missing.dedup();
    let short: Vec<&&CalibrationRow> = rows.iter().filter(|r| r.status == "not_enough").collect();
    let low: Vec<&&CalibrationRow> = rows.iter().filter(|r| r.status == "low").collect();
    let needed: usize = short.iter().map(|r| r.pairs_needed).sum::<usize>() + if tagged { missing.len() * crate::calibration::MIN_PAIRS } else { 0 };
    if needed == 0 && low.is_empty() {
        return None;
    }
    let per = |r: &&&CalibrationRow| if r.scope == "all" { format!("{} more", r.pairs_needed) } else { format!("{} for `{}`", r.pairs_needed, r.scope) };
    let mut parts: Vec<String> = short.iter().map(per).collect();
    parts.extend(missing.iter().filter(|_| tagged).map(|t| format!("{} for `{t}`", crate::calibration::MIN_PAIRS)));
    let mut action = if needed > 0 {
        format!("Review {needed} more pair{} so the judge can be checked against you ({}); eight per tag is the minimum.", if needed == 1 { "" } else { "s" }, parts.join(", "))
    } else {
        String::new()
    };
    for r in &low {
        let where_ = if r.scope == "all" { "on this launch".to_string() } else { format!("on `{}` tasks", r.scope) };
        action.push_str(&format!("{}The judge agrees with you on only {:.0}% of {} pairs {where_}; rely on human review there, or try a stronger judge model.", if action.is_empty() { "" } else { " " }, r.agreement.or(r.rubric_agreement).unwrap_or(0.0) * 100.0, r.pairs.max(r.rubric_pairs)));
    }
    if needed > 0 {
        action.push_str(if human_review { " `wrangler review <launch> --for calibration` (or the Review page's calibration mode) serves the pairs that count." } else { " Turn on human review for this experiment, then use the Review page's calibration mode." });
    }
    Some(Suggestion {
        id: String::new(),
        kind: "calibrate_judge".into(),
        priority: 2,
        action,
        target: json!({"type": "judge", "model": cal.judge_model, "rubric": cal.rubric}),
        metric: "judge_agreement".into(),
        evidence: json!({"overall": cal.overall, "by_tag": cal.by_tag, "tags_without_votes": missing, "pairs_needed": needed, "launch_id": launch_id}),
        confidence: "high".into(),
    })
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
