//! Human review: the blind pairwise queue, votes, and annotation digests.
//!
//! The queue asks about each task once before it repeats any, and within a
//! task serves the pair that most reduces uncertainty in the current
//! comparison, so the reviewer sees fewer items. Variant identity is never
//! in a queue item; it is revealed only by the vote response.

use crate::model::FactorKind;
use crate::scheduler::shown_for;
use crate::store::{Annotation, Judgment, Launch, RunRow, Store};
use crate::util::hash_u64;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Side {
    pub run_id: i64,
    pub output: String,
    pub diff: String,
    pub output_is_file: bool,
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub launch_id: i64,
    pub task_id: String,
    pub task_prompt: String,
    pub expected_output: String,
    pub output_file: Option<String>,
    /// Pairs of this task the reviewer has already voted on. Above zero, the
    /// prompt is one they have seen, with two results they have not.
    pub task_votes: usize,
    pub left: Side,
    pub right: Side,
    /// Why this pair was chosen, without naming variants:
    /// undecided_comparison | task_unreviewed | judge_inconsistent
    pub why: Vec<String>,
    pub progress: Progress,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Progress {
    pub votes: usize,
    pub skipped: usize,
    /// Unreviewed pairs in comparisons that are still undecided.
    pub useful_remaining: usize,
    pub total_remaining: usize,
    /// Size of the starter set: one pair per task, the most informative
    /// one. Everything beyond it repeats a task and is optional.
    pub recommended: usize,
    pub recommended_left: usize,
}

/// The most pairs to ask for up front. The starter set is one pair per
/// task, so a second look at the same prompt is always the reviewer's choice.
pub const STARTER_SET: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reveal {
    pub judgment_id: i64,
    pub left_variant: String,
    pub right_variant: String,
    /// The LLM judge's verdict on this same pair, as left | right | tie.
    pub judge: Option<String>,
    pub judge_consistent: Option<bool>,
    pub judge_reason: Option<String>,
}

struct Candidate {
    task: String,
    a: i64,
    b: i64,
    vi: usize,
    vj: usize,
    /// Human votes and skips already recorded on this task, any variants.
    task_votes: usize,
    task_skips: usize,
}

struct Queue {
    candidates: Vec<(Candidate, f64, Vec<String>)>,
    progress: Progress,
}

/// How unsettled a two-way comparison is, in (0, 1]: 1 with no evidence or
/// an even split, falling toward 0 as one side pulls clear.
fn uncertainty(wins_i: f64, wins_j: f64) -> f64 {
    let n = wins_i + wins_j;
    let p = (wins_j + 1.0) / (n + 2.0);
    let sd = (p * (1.0 - p) / (n + 3.0)).sqrt();
    let z = (p - 0.5).abs() / sd;
    (-z * z / 2.0).exp()
}

fn build_queue(launch: &Launch, runs: &[RunRow], judgments: &[Judgment]) -> Queue {
    let variants = &launch.variants;
    let vidx: HashMap<i64, usize> = runs.iter().filter_map(|r| Some((r.id, variants.iter().position(|v| v.key == r.variant_key)?))).collect();
    let by_id: HashMap<i64, &RunRow> = runs.iter().map(|r| (r.id, r)).collect();
    let pair_key = |a: i64, b: i64| if a <= b { (a, b) } else { (b, a) };

    // Evidence per variant pair: human votes count fully, judge votes half.
    let mut wins: HashMap<(usize, usize), (f64, f64)> = HashMap::new();
    let mut human_done: HashMap<(i64, i64), ()> = HashMap::new();
    let mut task_votes: HashMap<(String, usize, usize), f64> = HashMap::new();
    let mut voted_on_task: HashMap<String, usize> = HashMap::new();
    let mut skipped_on_task: HashMap<String, usize> = HashMap::new();
    let mut judge_inconsistent: HashMap<(i64, i64), ()> = HashMap::new();
    let mut progress = Progress::default();
    for j in judgments {
        let (Some(&ia), Some(&ib)) = (vidx.get(&j.run_a), vidx.get(&j.run_b)) else { continue };
        if j.source == "human" {
            human_done.insert(pair_key(j.run_a, j.run_b), ());
            if j.winner == "skip" {
                progress.skipped += 1;
                *skipped_on_task.entry(j.task_id.clone()).or_default() += 1;
                continue;
            }
            progress.votes += 1;
            *voted_on_task.entry(j.task_id.clone()).or_default() += 1;
            *task_votes.entry((j.task_id.clone(), ia.min(ib), ia.max(ib))).or_default() += 1.0;
        } else if j.consistent == Some(false) {
            judge_inconsistent.insert(pair_key(j.run_a, j.run_b), ());
        }
        let weight = if j.source == "human" { 1.0 } else { 0.5 };
        let (lo, hi, lo_is_a) = if ia < ib { (ia, ib, true) } else { (ib, ia, false) };
        let e = wins.entry((lo, hi)).or_default();
        match (j.winner.as_str(), lo_is_a) {
            ("a", true) | ("b", false) => e.0 += weight,
            ("b", true) | ("a", false) => e.1 += weight,
            ("tie", _) => {
                e.0 += weight / 2.0;
                e.1 += weight / 2.0;
            }
            _ => {}
        }
    }

    let mut candidates = vec![];
    for t in launch.tasks.iter().filter(|t| t.graders.human != Some(false)) {
        for i in 0..variants.len() {
            for j in (i + 1)..variants.len() {
                for rep in 0..launch.def.runs_per_cell {
                    let find = |key: &str| runs.iter().find(|r| r.task_id == t.id && r.variant_key == key && r.rep == rep && r.is_complete());
                    let (Some(ra), Some(rb)) = (find(&variants[i].key), find(&variants[j].key)) else { continue };
                    if human_done.contains_key(&pair_key(ra.id, rb.id)) {
                        continue;
                    }
                    // Identical outputs carry no preference information.
                    if by_id[&ra.id].output_text == by_id[&rb.id].output_text {
                        continue;
                    }
                    let (wi, wj) = wins.get(&(i, j)).copied().unwrap_or_default();
                    let u = uncertainty(wi, wj);
                    let seen = task_votes.get(&(t.id.clone(), i, j)).copied().unwrap_or(0.0);
                    let inconsistent = judge_inconsistent.contains_key(&pair_key(ra.id, rb.id));
                    // Prefer clean comparisons: two variants that differ in one
                    // factor, and most of all in the pattern under test (same
                    // model, with and without the skill). A pair that differs
                    // in several factors says little about any one of them.
                    let differing: Vec<FactorKind> = variants[i].levels.iter().zip(&variants[j].levels).filter(|(a, b)| a.1.label != b.1.label).map(|(a, _)| a.1.kind).collect();
                    let focus = match differing.as_slice() {
                        [FactorKind::Overlay] | [FactorKind::SystemPromptAppend] => 2.0,
                        [_] => 1.0,
                        _ => 0.25,
                    };
                    let priority = u * focus * (1.0 / (1.0 + seen)) * if inconsistent { 1.5 } else { 1.0 };
                    let mut why = vec![];
                    if u > 0.15 {
                        why.push("undecided_comparison".to_string());
                    }
                    if seen == 0.0 {
                        why.push("task_unreviewed".to_string());
                    }
                    if inconsistent {
                        why.push("judge_inconsistent".to_string());
                    }
                    if u > 0.15 {
                        progress.useful_remaining += 1;
                    }
                    progress.total_remaining += 1;
                    let on_task = |m: &HashMap<String, usize>| m.get(&t.id).copied().unwrap_or(0);
                    candidates.push((Candidate { task: t.id.clone(), a: ra.id, b: rb.id, vi: i, vj: j, task_votes: on_task(&voted_on_task), task_skips: on_task(&skipped_on_task) }, priority, why));
                }
            }
        }
    }
    // The starter set is one vote per task. A second vote on a task does not
    // stand in for a task that has none.
    let reviewed = launch.tasks.iter().filter(|t| voted_on_task.contains_key(&t.id)).count();
    let mut unreviewed: Vec<&str> = candidates.iter().filter(|c| c.0.task_votes == 0).map(|c| c.0.task.as_str()).collect();
    unreviewed.sort();
    unreviewed.dedup();
    progress.recommended = STARTER_SET.min(reviewed + unreviewed.len());
    progress.recommended_left = progress.recommended.saturating_sub(reviewed).min(unreviewed.len());
    Queue { candidates, progress }
}

pub fn progress(store: &Store, launch_id: i64) -> Result<Progress> {
    let launch = store.launch(launch_id)?;
    Ok(build_queue(&launch, &store.runs(launch_id)?, &store.judgments(launch_id, None)?).progress)
}

/// The next pair to review, or `None` when the queue is empty.
pub fn next(store: &Store, launch_id: i64) -> Result<Option<ReviewItem>> {
    let launch = store.launch(launch_id)?;
    let runs = store.runs(launch_id)?;
    let queue = build_queue(&launch, &runs, &store.judgments(launch_id, None)?);
    // Tasks with no vote come first, and among those the ones not yet skipped,
    // so no prompt comes round again while another is still waiting.
    let best = queue.candidates.iter().max_by(|x, y| {
        (y.0.task_votes, y.0.task_skips)
            .cmp(&(x.0.task_votes, x.0.task_skips))
            .then_with(|| x.1.total_cmp(&y.1))
            .then_with(|| hash_u64(&format!("{}|{}", x.0.a, x.0.b)).cmp(&hash_u64(&format!("{}|{}", y.0.a, y.0.b))))
    });
    let Some((cand, _, why)) = best else { return Ok(None) };
    let _ = (cand.vi, cand.vj);
    let task = launch.tasks.iter().find(|t| t.id == cand.task).unwrap();
    let side = |id: i64| -> Result<Side> {
        let run = runs.iter().find(|r| r.id == id).unwrap();
        let shown = shown_for(run, task);
        Ok(Side { run_id: id, output: shown.output, diff: shown.diff, output_is_file: shown.output_is_file, annotations: store.annotations_for_run(id)? })
    };
    // Which run goes left is a coin flip fixed per pair, independent of variant order.
    let flip = hash_u64(&format!("side|{}|{}", cand.a, cand.b)) % 2 == 1;
    let (l, r) = if flip { (cand.b, cand.a) } else { (cand.a, cand.b) };
    Ok(Some(ReviewItem {
        launch_id,
        task_id: task.id.clone(),
        task_prompt: task.prompt.clone(),
        expected_output: task.expected_output.clone(),
        output_file: task.output_file.clone(),
        task_votes: cand.task_votes,
        left: side(l)?,
        right: side(r)?,
        why: why.clone(),
        progress: queue.progress,
    }))
}

/// Record a vote (`left`, `right`, `tie`, `both_bad`, `skip`) and reveal the variants.
pub fn vote(store: &Store, launch_id: i64, left_run: i64, right_run: i64, choice: &str, reason: &str) -> Result<Reveal> {
    let winner = match choice {
        "left" => "a",
        "right" => "b",
        "tie" => "tie",
        "both_bad" => "both_bad",
        "skip" => "skip",
        other => bail!("unknown vote `{other}`"),
    };
    let launch = store.launch(launch_id)?;
    let (l, r) = (store.run(left_run)?, store.run(right_run)?);
    if l.launch_id != launch_id || r.launch_id != launch_id || l.task_id != r.task_id {
        bail!("runs {left_run} and {right_run} are not a pair in launch {launch_id}");
    }
    // One human vote per pair: a repeated vote replaces the earlier one.
    for old in store.judgments(launch_id, Some("human"))? {
        if (old.run_a, old.run_b) == (left_run, right_run) || (old.run_a, old.run_b) == (right_run, left_run) {
            store.delete_judgment(old.id)?;
        }
    }
    let judgment_id = store.add_judgment(&Judgment {
        id: 0,
        launch_id,
        task_id: l.task_id.clone(),
        run_a: left_run,
        run_b: right_run,
        source: "human".into(),
        winner: winner.into(),
        reason: reason.trim().to_string(),
        consistent: None,
        detail: json!({}),
        created_at: String::new(),
    })?;
    let label = |key: &str| launch.variants.iter().find(|v| v.key == key).map(|v| v.label.clone()).unwrap_or_else(|| key.to_string());
    let judge = store.judgments(launch_id, Some("judge"))?.into_iter().find(|j| (j.run_a, j.run_b) == (left_run, right_run) || (j.run_a, j.run_b) == (right_run, left_run));
    let judge_side = judge.as_ref().map(|j| {
        let left_is_a = j.run_a == left_run;
        match (j.winner.as_str(), left_is_a) {
            ("a", true) | ("b", false) => "left",
            ("b", true) | ("a", false) => "right",
            _ => "tie",
        }
        .to_string()
    });
    Ok(Reveal {
        judgment_id,
        left_variant: label(&l.variant_key),
        right_variant: label(&r.variant_key),
        judge: judge_side,
        judge_consistent: judge.as_ref().and_then(|j| j.consistent),
        judge_reason: judge.map(|j| j.reason),
    })
}

// ------------------------------------------------------------ annotations

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaggedPassage {
    pub quote: String,
    /// Reviewed runs of this variant in which the passage was annotated.
    pub runs: usize,
    pub comments: Vec<String>,
    pub emoji: BTreeMap<String, usize>,
    pub run_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantAnnotations {
    pub variant: String,
    pub variant_key: String,
    /// Runs of this variant that carry at least one annotation.
    pub annotated_runs: usize,
    /// Runs of this variant a human has voted on.
    pub reviewed_runs: usize,
    pub emoji: BTreeMap<String, usize>,
    pub passages: Vec<FlaggedPassage>,
    pub annotations: Vec<serde_json::Value>,
}

fn normalize_quote(q: &str) -> String {
    q.split_whitespace().collect::<Vec<_>>().join(" ").trim_matches(|c: char| c.is_ascii_punctuation()).to_lowercase()
}

/// Structured view of all human annotations in a launch, grouped by variant,
/// so an agent improving a skill can read what reviewers flagged and how often.
pub fn annotation_digest(store: &Store, launch_id: i64) -> Result<Vec<VariantAnnotations>> {
    let launch = store.launch(launch_id)?;
    let runs = store.runs(launch_id)?;
    let anns = store.annotations_for_launch(launch_id)?;
    let judgments = store.judgments(launch_id, Some("human"))?;
    let by_id: HashMap<i64, &RunRow> = runs.iter().map(|r| (r.id, r)).collect();
    let mut out = vec![];
    for v in &launch.variants {
        let mine: Vec<&Annotation> = anns.iter().filter(|a| by_id.get(&a.run_id).map(|r| r.variant_key == v.key).unwrap_or(false)).collect();
        let mut reviewed: Vec<i64> = judgments
            .iter()
            .filter(|j| j.winner != "skip")
            .flat_map(|j| [j.run_a, j.run_b])
            .filter(|id| by_id.get(id).map(|r| r.variant_key == v.key).unwrap_or(false))
            .collect();
        reviewed.sort();
        reviewed.dedup();
        let mut annotated: Vec<i64> = mine.iter().map(|a| a.run_id).collect();
        annotated.sort();
        annotated.dedup();
        let mut emoji: BTreeMap<String, usize> = BTreeMap::new();
        let mut groups: BTreeMap<String, FlaggedPassage> = BTreeMap::new();
        for a in &mine {
            if a.kind == "emoji" {
                *emoji.entry(a.body.clone()).or_default() += 1;
            }
            let key = normalize_quote(&a.quote);
            if key.is_empty() {
                continue;
            }
            let g = groups.entry(key).or_insert_with(|| FlaggedPassage { quote: a.quote.trim().to_string(), runs: 0, comments: vec![], emoji: BTreeMap::new(), run_ids: vec![] });
            if !g.run_ids.contains(&a.run_id) {
                g.run_ids.push(a.run_id);
                g.runs += 1;
            }
            if a.kind == "emoji" {
                *g.emoji.entry(a.body.clone()).or_default() += 1;
            } else if !a.body.trim().is_empty() {
                g.comments.push(a.body.clone());
            }
        }
        let mut passages: Vec<FlaggedPassage> = groups.into_values().collect();
        passages.sort_by(|a, b| b.runs.cmp(&a.runs).then(a.quote.cmp(&b.quote)));
        out.push(VariantAnnotations {
            variant: v.label.clone(),
            variant_key: v.key.clone(),
            annotated_runs: annotated.len(),
            reviewed_runs: reviewed.len().max(annotated.len()),
            emoji,
            passages,
            annotations: mine
                .iter()
                .map(|a| json!({"run_id": a.run_id, "task_id": by_id[&a.run_id].task_id, "kind": a.kind, "target": a.target, "lines": [a.line_start, a.line_end], "quote": a.quote, "body": a.body}))
                .collect(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertainty_falls_with_evidence() {
        assert!((uncertainty(0.0, 0.0) - 1.0).abs() < 1e-9);
        assert!(uncertainty(3.0, 3.0) > 0.99);
        assert!(uncertainty(1.0, 3.0) > uncertainty(1.0, 9.0));
        assert!(uncertainty(0.0, 12.0) < 0.01);
    }
}
