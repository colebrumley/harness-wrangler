# Harness Wrangler: Requirements and Design

Status: requirements settled, design proposed, no code written. This document is the handoff to the builders.

Author of requirements: Cole Brumley. Design discussion captured 2026-10-11.

## 1. Purpose

Harness Wrangler runs controlled experiments over coding-agent harness patterns and reports which patterns work, how much, and where they fail.

A "pattern" is anything that shapes agent behavior through instructions and configuration: skills (`SKILL.md`), `CLAUDE.md` files, hooks, subagent definitions, settings, system prompt appends, effort level, and the model itself.

The motivating problem: when a new model ships (Opus 5.5 was the trigger), many existing skills and harness pieces become partly or wholly obsolete, and there is no way to see which ones, how much, or which sections are dead weight. The tool must answer questions like:

- "Is my `reader-first-writing` skill still improving output on Opus 5.5? By how much? Which sections carry the effect?"
- "Does this skill work better at medium or high effort?"
- "Does this skill behave differently under Claude Code vs Codex?"
- "Which of my tracked skills regressed when the new model landed?"

## 2. Non-goals

- Permission and safety gating are out of scope. The tool measures what the model thinks and does given instructions, not whether a permission mode blocks it. Runs use bypass permissions inside an isolation sandbox.
- No sharing, sync, or multi-user features in the first round. Single user, local machine.
- No budget enforcement in the first round. Budgets are a later, optional feature. Per-run timeouts are in scope from the start.
- No deterministic transcript-harvesting pipeline. An agent that knows the task format can write tasks from transcripts; the tool only needs a clear, documented task format.
- Docker is not a dependency. It may become an optional isolation backend later.

## 3. Core concepts

| Concept | Definition |
|---|---|
| Experiment | A named, versioned definition: task set, factors and their levels, runner, graders, repetitions, timeout. Stored as a file in a git repo the user owns. |
| Factor | One axis being varied. Kinds: overlay, model, effort, runner, system prompt append, arbitrary runner knob. |
| Variant | One combination of factor levels. The cross product of all factor levels yields the cells. |
| Overlay | A directory of files mounted into the run workspace: `.claude/skills/*`, `CLAUDE.md`, `.claude/settings.json`, `.claude/agents/*`, hooks. The empty overlay is the baseline. |
| Task | A prompt plus a fixture plus graders plus metadata. Hand-written, or written by an agent. |
| Fixture | The starting workspace state for a task: a directory, a git repo at a ref, or empty. |
| Cell | Task × Variant. |
| Run | One execution of a cell. N runs per cell. Produces a transcript, a workspace diff, output files, runner metrics, and grader scores. |
| Grader | A function from a run to one or more scores. Tiers: deterministic, LLM judge, human. |
| Metric | A named numeric per run. Built-in: pass, judge score, cost, tokens in/out, cache hit, turns, tool calls by name, wall time, diff size, files touched. User-defined metrics come from graders. |
| Annotation | Human feedback on a run: inline comments anchored to a line or highlight range, emoji reactions, pairwise preference. |
| Tracked suite | A named set of experiments pinned as a regression suite, with saved per-model baselines. |

## 4. Architecture

Three deliverables, one Rust workspace:

1. `wrangler-core`: library. Experiment parsing, scheduling, runners, isolation, graders, SQLite store, statistics, ablation generation, report model. No UI dependency.
2. `wrangler` CLI: thin wrapper over core. Everything the GUI can do, scriptable. Needed so agents and CI can drive the tool.
3. Desktop app: Tauri 2 with a TypeScript frontend. Rust backend calls core directly. The GUI exists for the things text cannot do well: charts, run comparison, blind review with inline annotation, the regression dashboard.

Decision rationale: Cole asked for effectiveness over purity. Tauri gives a Rust core with real charting and a review UI that supports text selection and inline comments. A pure-Rust GUI (egui, iced) is weaker at exactly the reporting surfaces this tool is about.

Storage: experiment definitions and task files live as TOML and Markdown in a user-owned git repo. Runs, transcripts, metrics, annotations, and baselines live in a local SQLite database. Large blobs (transcripts, workspace snapshots) go on disk under a runs directory, referenced from SQLite.

Target platform: macOS. Nothing should preclude Linux later.

## 5. Runner interface

```rust
trait AgentRunner {
    fn id(&self) -> &str;                       // "claude-code", "codex", ...
    fn capabilities(&self) -> Capabilities;     // which factor kinds it can honor
    fn materialize(&self, ws: &Workspace, variant: &Variant) -> Result<()>;
    fn run(&self, ws: &Workspace, task: &Task, variant: &Variant, limits: &Limits)
        -> Result<RunRecord>;
}
```

`RunRecord` must carry a normalized transcript (list of turns with role, text, tool calls, tool results), raw runner output, exit status, timing, and a `RunnerMetrics` map. Runners must declare which factors they cannot honor so the scheduler can reject or warn at experiment load.

### 5.1 Claude Code runner (first runner)

Invocation shape, validated by spike on 2026-10-11 with Claude Code 2.1.296:

```
claude -p --setting-sources project \
  --no-session-persistence \
  --permission-mode bypassPermissions \
  --model <model> --effort <level> \
  --output-format stream-json --verbose \
  [--append-system-prompt <text>] \
  [--plugin-dir <dir>] \
  [--max-turns N] \
  "<task prompt>"
```

Key facts established by the spike:

- `--setting-sources project` hides all user-level config. In a clean tmp directory the agent reported only built-in skills, no plugins, no `CLAUDE.md`. The control run without the flag showed all of Cole's user plugins and skills. This is the isolation mechanism for overlays.
- The overlay is materialized into the workspace as project-level config: `./.claude/skills/<name>/SKILL.md`, `./CLAUDE.md`, `./.claude/settings.json`, `./.claude/agents/`. Project-level discovery picks it up.
- Plugins under test should be passed with `--plugin-dir` rather than installed.
- A fresh `CLAUDE_CONFIG_DIR` is not logged in on a Max subscription (keychain-bound). Do not use that route.
- `--bare` never reads OAuth. Not usable on a Max subscription. Do not use.
- The `--output-format json` result carries `total_cost_usd`, `usage` (input, output, cache read, cache creation, thinking tokens), `modelUsage` per model, `num_turns`, `duration_ms`, `duration_api_ms`, `permission_denials`, `subagent_stats`. Harvest all of it. Use `stream-json` for the full transcript and derive the same summary from the final result event.
- Cole runs on a Max subscription. Parallelism should be configurable and default conservatively (2 to 4 concurrent runs). Rate-limit responses must be detected and retried with backoff, and recorded as a run event, not silently absorbed into timing.

### 5.2 Later runners

Codex CLI and a raw Agent SDK loop are the expected next runners. A skill-under-Claude vs skill-under-Codex comparison is an in-scope experiment, so `runner` is a first-class factor.

## 6. Isolation ladder

Every run gets a fresh workspace: fixture copied into a temp directory, `git init` or checkout, overlay materialized. Workspace is snapshotted after the run for diffing. Levels:

1. `workspace`: temp dir copy only. Default for trusted fixtures.
2. `seatbelt`: macOS `sandbox-exec` profile. Writes confined to the workspace and runner cache paths. Network allowed to Anthropic endpoints only, or fully allowed, per experiment. This is the native macOS primitive and what Claude Code itself uses.
3. `docker`: optional later backend. Must not be required for anything else to work.

Chroot was considered and rejected: it needs root on macOS and breaks keychain-backed auth.

## 7. Grading

All three tiers are first-class and selectable per task. A task can attach multiple graders.

### 7.1 Deterministic

Command exit code (tests), file exists, regex over output or files, diff size bounds, word count bounds, JSON schema validation of structured output, "skill was invoked" (detect skill tool call or `SKILL.md` read in transcript). Fast, free, run first.

### 7.2 LLM judge

Rubric judge: scores one run against a list of expectations, returns pass/fail with cited evidence per expectation, plus an overall score. Pairwise judge: blind A/B between two runs of the same task, order randomized, position-bias check by also running swapped order. Judge model and effort are configurable and recorded. Judges run through the same runner infrastructure or the API directly.

### 7.3 Human review

Blind pairwise is the primary human task. Design requirements from Cole:

- Minimize human work. Never present whole transcripts. Show the task prompt, the final output or the diff, and a collapsed path to the transcript. Side by side, labels hidden until the vote is cast.
- Inline comments anchored to a line or a highlighted range of the output or diff.
- Emoji reactions on lines or ranges as a cheap signal.
- Prefer A, prefer B, tie, both bad. Optional one-line reason.
- Keyboard-first: vote, next, skip.
- A review queue: the tool picks the pairs that most reduce uncertainty in the current comparison, so the human reviews fewer items.

Annotations are stored structurally so an agent can later read them when improving the skill ("humans flagged these sentences as hedging in 4 of 6 runs").

## 8. Analysis and statistics

- Default N of 5 runs per cell. Configurable per experiment.
- Primary comparison is paired by task: for each task, compare variant metric distributions, then aggregate.
- Report effect size with bootstrapped confidence intervals, not just means. Mark comparisons whose CI spans zero as inconclusive.
- Pairwise human and judge votes are aggregated with a Bradley-Terry model when there are more than two variants.
- Variance per cell is reported. High-variance tasks are flagged as flaky or model-sensitive.
- Cost and latency are always reported alongside quality so tradeoffs are visible.
- Position-bias check for pairwise judges is reported.

## 9. Ablation

Given a skill or `CLAUDE.md`, the tool auto-generates variants:

- No-skill baseline.
- Full skill.
- Leave-one-section-out: split at Markdown headings (configurable depth), one variant per removed section.
- Optional: description-only (frontmatter only, body removed) to measure triggering vs content.

The report attributes effect to sections: "section 3 carries most of the effect, section 5 has no measurable effect, section 2 hurts on this model." Attribution is the delta between full and leave-one-out, with CIs. This is the feature that answers "where is it failing."

## 10. Model regression tracking

First-class feature. A tracked suite is a set of experiments with a chosen primary metric. For each model the suite has been run on, the tool stores a baseline result set.

Dashboard shows, per tracked skill or pattern, per model: effect vs no-skill baseline, change vs previous model, cost delta, and a verdict (helping, neutral, harming, inconclusive).

"New model" workflow: select the new model, the tool reruns the suite, and shows deltas against the stored baseline for the previous model. Output is a prioritized list: patterns that regressed, patterns now redundant, patterns still earning their keep.

## 11. Advice

The report layer should end with concrete, measurable suggestions, not prose: "Remove section 5 (no effect, saves 1.2k tokens per run)." "Section 2 lowers judge score by 0.4 on Opus 5.5; review it." "Skill never triggered on tasks 3 and 7; description may not match." Each suggestion names the metric and the evidence. Suggestions should be exportable as structured JSON so an agent can act on them.

## 12. File formats

Experiment (`experiments/<name>.toml`), sketch:

```toml
name = "reader-first-writing-effort"
description = "Does rfw help on Opus 5.5, and does effort matter?"
runs_per_cell = 5
timeout_secs = 900
isolation = "seatbelt"

[runner]
kind = "claude-code"
concurrency = 3

[[factors]]
name = "overlay"
kind = "overlay"
levels = ["overlays/none", "overlays/rfw-v1.1.0", "overlays/rfw-v1.2-draft"]

[[factors]]
name = "model"
kind = "model"
levels = ["claude-opus-5-5", "claude-sonnet-5-5"]

[[factors]]
name = "effort"
kind = "effort"
levels = ["medium", "high"]

[tasks]
include = ["tasks/writing/*.toml"]

[graders.default]
judge = { model = "claude-opus-5-5", rubric = "rubrics/reader-first.md", pairwise = true }
human = { enabled = true }
```

Task (`tasks/writing/incident-summary.toml`), sketch:

```toml
id = "incident-summary"
prompt = "Summarize the attached incident report for the on-call channel."
fixture = { dir = "fixtures/incident-report" }
expected_output = "A short, plain summary under 150 words leading with impact and current status."
expectations = [
  "Leads with user impact",
  "Under 150 words",
  "No hedging phrases",
]
tags = ["writing", "summary"]

[graders]
deterministic = [{ kind = "word_count", path = "SUMMARY.md", max = 150 }]
```

Overlays are plain directories. An overlay named `none` is the empty baseline.

## 13. Interop

Import `skill-creator` eval format (`evals/evals.json` with `prompt`, `expected_output`, `files`, `expectations`) into tasks. Export results in a shape compatible with its `benchmark.json` viewer where cheap. Watch `claude plugin eval` as another potential import source. Treat other frameworks as import formats, not dependencies.

## 14. GUI surfaces

1. Experiments: list, create, edit factors and tasks, launch, watch progress with per-cell status and live cost.
2. Results: factor comparison charts with CIs, per-task breakdown, cost and latency alongside quality, drill into any run's transcript and diff.
3. Review: the blind pairwise queue described in section 7.3.
4. Ablation: section attribution view for a skill.
5. Regression: tracked suites, per-model baselines, new-model diff.
6. Advice: the suggestion list with evidence links.

The CLI must cover 1, 2, 4, 5, and 6 in text and JSON. Only 3 is GUI-only.

## 15. Suggested build order

1. Core types, experiment and task parsing, SQLite store.
2. Claude Code runner with workspace isolation and stream-json transcript capture. Verify the overlay isolation claim from the spike in an automated test.
3. Deterministic graders and runner metrics. CLI `run` and `report` with text and JSON output.
4. Statistics: bootstrapped CIs, paired comparison.
5. LLM judge, rubric and pairwise, with position-bias check.
6. Tauri app: experiments and results views.
7. Human review view with inline annotations and emoji.
8. Ablation generation and attribution report.
9. Tracked suites and regression dashboard.
10. Advice layer.
11. Seatbelt isolation. Codex runner.

## 16. Open questions for the builders

- Seatbelt profile details for Claude Code: which paths outside the workspace it needs writable (its own cache, session state when persistence is off). Measure, do not guess.
- Whether `--setting-sources project` also suppresses user-level hooks and MCP servers. The spike checked skills, plugins, and `CLAUDE.md` only.
- Whether judges should run via Claude Code or the API directly. The API is cleaner for structured output. Claude Code keeps everything on the Max subscription. Cole has no budget limit for now, so pick for correctness.
- Transcript normalization across runners: define the common shape before writing the second runner.
- How to snapshot large fixture repos efficiently. Start with copy; consider APFS clones (`cp -c`) since the target is macOS.

## 17. Spike evidence

Run on 2026-10-11, Claude Code 2.1.296, haiku, in an empty temp git repo.

With `--setting-sources project`, agent reported skills: only the built-in set (dataviz, artifact-design, code-review, simplify, loop, schedule, claude-api, init, security-review, and so on). Plugins: none. CLAUDE.md: false.

Without the flag (control), agent reported Cole's installed plugins (critique, frontend-design, reader-first-writing, anthropic-skills, playwright) and all their skills.

With `CLAUDE_CONFIG_DIR` pointed at an empty directory: `Not logged in · Please run /login`.
