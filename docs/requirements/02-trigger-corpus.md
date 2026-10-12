# 2. Trigger corpus

Value: high. Difficulty: low.

## Purpose

For most skills the value is deciding to fire at the right moment, and a skill that fires on unrelated prompts is a cost on every run. The tool measures only whether a skill fired on prompts written for it. The rfw-writing launch found a skill that never fired on Haiku, which is the easy direction. There is no measurement of false positives and no corpus to run them over.

Triggering is decided from the skill's description and the prompt, before the body is read. So trigger experiments are cheap: stop after the first turn or two, no fixture, no judge.

## Requirements

1. A task can carry an expected trigger: `trigger = "should"` or `trigger = "should_not"`. An experiment with `[trigger] skill = "<name>"` (or `skills = [...]`) turns every such task into a `skill_invoked` grade for that skill with the matching `expect`, without the task author writing the grader.

2. Tasks can cap turns: `max_turns` on a task overrides `[runner] max_turns`. A trigger experiment template sets `max_turns = 2` and no judge. Negative-trigger tasks need no fixture; the empty workspace is the default already.

3. The report gets a trigger section per variant: `trigger_precision`, `trigger_recall`, `trigger_f1` with bootstrap intervals over tasks, the list of false negatives (should-fire tasks where it never fired) and false positives (should-not tasks where it fired), and a per-model breakdown when a model factor is present. `trigger_f1` is a valid `primary_metric`.

4. Advice splits `fix_trigger` into `trigger_misses` (recall low: lists the missed prompts and the description) and `trigger_overfires` (precision low: lists the prompts that fired and the clause in the description most likely responsible, when a description ablation ran). Both carry the description text in the evidence so an agent can rewrite it.

5. Description ablation at sentence depth. `wrangler ablate <skill> --description-sentences` generates one variant per sentence removed from the frontmatter description, with the body intact. The ablation report attributes precision and recall to sentences the way it attributes judge score to sections today.

6. A corpus generator: `wrangler trigger-set <skill> [--n 20] [--model m]` drafts should-fire and should-not-fire prompts from the description using the judge model, writes them under `tasks/trigger/<skill>/` with `draft` in their tags, and prints them for review. Should-not prompts must be near misses (same domain, wrong moment), not random; the generator prompt says so and the user edits the result.

7. The example project gains `experiments/rfw-trigger.toml` and `tasks/trigger/reader-first-writing/` with at least six should-fire and six should-not prompts, written by hand.

## Where it lands

- `crates/wrangler-core/src/model.rs`: `Task.trigger`, `Task.max_turns`, `Experiment.trigger`.
- `crates/wrangler-core/src/project.rs`: expansion of `trigger` into graders at load.
- `crates/wrangler-core/src/runner/claude.rs`, `codex.rs`: per-task `max_turns`.
- `crates/wrangler-core/src/report.rs`, `stats.rs`: precision and recall with intervals (rates over tasks, paired by task across variants).
- `crates/wrangler-core/src/advice.rs`: the two new kinds.
- `crates/wrangler-core/src/ablation.rs`: sentence split of the description; `AblationMeta` records the mode.
- `crates/wrangler-cli`: `trigger-set`, `ablate --description-sentences`.
- `docs/formats.md`.

## Acceptance

- Unit test: an experiment with `[trigger]` and tasks marked `should` and `should_not` expands to the right `skill_invoked` graders; a run that fires on a `should_not` task counts as a false positive in the report.
- Unit test: description sentence split round-trips frontmatter and leaves the body byte-identical.
- The mock runner gets a trigger rule (fires when the prompt contains a word from the description) so the example `rfw-trigger` experiment produces non-trivial precision and recall without spending tokens.
- A live run of `rfw-trigger` on Haiku and Sonnet reports recall per model, and the advice names the prompts that missed.

## Open questions

- `skills_invoked()` in `transcript.rs` detects the `Skill` tool call and reads of `SKILL.md`. Confirm that a skill loaded by description match on Claude Code 2.1.x shows up as one of these in the stream and not only as a system message; the live test for item 1 can check this at the same time.
- Whether a fired skill on a should-not prompt should also be judged for harm (did the output get worse). Not in this item; a normal experiment on those tasks answers it.
