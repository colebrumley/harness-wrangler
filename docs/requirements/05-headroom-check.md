# 5. Headroom check

Value: medium. Difficulty: low.

## Purpose

The terse-coding launch came back as "consider retiring this pattern" with an interval of exactly zero. Every run in both arms passed. The task had no headroom, so the experiment could not show a benefit, and the advice turned a task problem into a verdict on the pattern. The same launch flagged a task as flaky on three runs, which is noise.

The tool should notice saturation, say so, and let the user find out before spending on variants.

## Requirements

1. Saturation per task and metric. A task is saturated for a metric in a launch when the baseline cell is at the metric's bound with no variance: pass rate 0 or 1 with `n >= 3`, judge score mean `>= 9` or `<= 2` with `sd < 0.5`. The report marks each task row `headroom = "none" | "little" | "ok"` with the direction (ceiling or floor). "Little" is within one equivalence band of the bound.

2. Verdicts respect it. `paired_effect` takes the set of saturated units; when every unit is saturated the verdict is a new `Verdict::NoHeadroom`, which `as_str` renders as "no headroom" and which counts as inconclusive everywhere a verdict is consumed (`is_conclusive_change` false; suite diffs treat it as "not measured"). When some units are saturated the effect is computed over the rest and the report says how many were dropped. An interval of width zero with `n > 1` on both sides is never "no effect".

3. Advice. `retire_pattern` and `remove_section` require at least one unsaturated task. Otherwise a `no_headroom` suggestion, priority 1: "Every task passes without the pattern on this model; the experiment cannot show a benefit. Add harder tasks or switch the primary metric." For `pass`, the evidence lists the tasks and suggests `judge_score` or a `diff_size` bound as metrics that still vary. `flaky_task` requires `n >= 5`; below that the report notes "too few runs to judge flakiness".

4. Baseline first. The scheduler orders the pending queue so every baseline-variant run precedes every other run (stable within a variant). `wrangler run <exp> --baseline-only` creates the launch, runs only the baseline cells, prints per-task headroom and variance, and leaves the launch in status `baseline` so `wrangler resume` finishes it. The desktop Start page offers this as the step between "add tasks" and "launch".

5. `wrangler task check` from item 3 reuses the baseline-only path on a single task.

6. `README.md` "How results are computed" gains a line on headroom; `docs/formats.md` does not change.

## Where it lands

- `crates/wrangler-core/src/stats.rs`: `Verdict::NoHeadroom`, `saturation(xs, info) -> Headroom`.
- `crates/wrangler-core/src/report.rs`: task headroom field, dropped-unit counts, zero-width interval guard.
- `crates/wrangler-core/src/advice.rs`: the gating and `no_headroom`.
- `crates/wrangler-core/src/scheduler.rs`: queue order, `baseline_only` in `RunOptions`, the `baseline` launch status.
- `crates/wrangler-cli`: the flag; `status` shows the baseline phase.
- `apps/desktop`: Start page step; headroom column on the per-task table.

## Acceptance

- Unit test: all-pass data in both arms yields `NoHeadroom`, not `Neutral`; mixed saturated and unsaturated tasks drop the saturated ones and report the count.
- Re-running `wrangler advice 9` on the example project's database gives `no_headroom`, not `retire_pattern`. Launch 8's `flaky_task` disappears at `n = 3`.
- Scheduler test with the mock runner: the first `runs_per_cell × tasks` runs to finish all belong to the baseline variant; `--baseline-only` leaves the rest pending with status `baseline`.

## Open questions

- Thresholds for judge scores. 9 and 2 are starting points; revisit after item 7 shows how judges actually spread.
