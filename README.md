# Harness Wrangler

Controlled experiments over coding-agent harness patterns. It measures whether skills, `CLAUDE.md` files, hooks, system prompt appends, effort levels and models actually help, by how much, and which parts carry the effect.

[DESIGN.md](DESIGN.md) holds the requirements. This README is how to use what was built from it.

## What is here

| Path | What |
|---|---|
| `crates/wrangler-core` | The library: parsing, scheduling, runners, isolation, graders, SQLite store, statistics, ablation, suites, advice. |
| `crates/wrangler-cli` | The `wrangler` CLI. Text for people, `--json` for agents and CI. |
| `apps/desktop` | The desktop app: Tauri 2 shell plus a TypeScript frontend. |
| `examples/` | A ready project: tasks, overlays (including a copy of `reader-first-writing` 1.1.0), experiments, and suites. |
| `docs/` | [File formats](docs/formats.md), [isolation and what was measured](docs/isolation.md). |

## Build

Needs Rust and Node. Claude Code must be installed and logged in for real runs; Codex is optional.

```bash
cargo build --release
```

```bash
cd apps/desktop && npm install && npx tauri build
```

The CLI lands at `target/release/wrangler` and the app at `target/release/bundle/macos/Harness Wrangler.app`.

## Try it without spending tokens

The `mock` runner and `mock` judge fake the agent and have a known answer, so you can see every screen first.

```bash
cd examples && ../target/release/wrangler run mock-demo
```

```bash
cd examples && ../target/release/wrangler advice mock-demo
```

Then open the app and choose the `examples` folder, or serve the same UI to a browser:

```bash
cd examples && ../target/release/wrangler serve
```

In the app, the Start page lists the steps from an empty project to acting on results, marks what is done, and says what is next. "Ask the agent" opens a terminal beside the UI running Claude Code in the project folder, briefed on this tool's file formats and commands, with the bundled `wrangler` CLI on its PATH; drag its left edge to resize it.

The app's File menu has Open Project, New Project, Open Recent and Close Project. The first time it reads a project under Documents or Desktop, macOS asks whether to allow access; choose Allow.

For scripted checks of the app itself, `WRANGLER_PICK_FOLDER=<dir>` makes the folder buttons use that path instead of the system picker, and `WRANGLER_UI_SCRIPT="Switch project|Create project in a folder"` presses the named buttons in order and writes what each screen showed to `WRANGLER_UI_REPORT`.

## A real experiment

`examples/experiments/rfw-writing.toml` asks whether `reader-first-writing` improves two writing tasks on Haiku, scored by a Sonnet judge. It makes 20 agent runs and about 30 judge calls.

```bash
cd examples && ../target/release/wrangler run rfw-writing
```

The first real run of it during the build found the skill was never invoked on these tasks under Haiku, and the advice said so. That is the kind of answer the tool is for.

## CLI

Run commands inside a project folder, or pass `-C <dir>`. Every command takes `--json`.

| Command | Does |
|---|---|
| `wrangler init [dir]` | Create a project layout. |
| `wrangler doctor` | Check the runner CLIs are installed. |
| `wrangler list experiments\|tasks\|overlays\|launches\|suites` | List things. |
| `wrangler validate <experiment>` | Check a definition and show its cells. Runs nothing. |
| `wrangler run <experiment> [--reps N] [--concurrency N] [--no-judge] [--budget USD]` | Run every cell, grade, judge, print the report. Ctrl-C stops cleanly. |
| `wrangler resume <launch> [--retry-failed] [--budget USD]` | Finish an interrupted launch. |
| `wrangler judge <launch>` | Run or finish the LLM judge phase. |
| `wrangler status <launch>` | Per-cell progress and cost. |
| `wrangler report <launch\|experiment> [--metric m]` | Variants, factor effects with 95% CIs, per-task table, pairwise, ablation. |
| `wrangler runs <launch>` / `wrangler show <run> [--output\|--diff\|--transcript]` | Drill into runs. |
| `wrangler ablate <skill-or-CLAUDE.md> [--description-only] [--depth N]` | Generate leave-one-section-out overlays and their experiment. |
| `wrangler suite create\|run\|status\|diff` | Tracked suites, per-model baselines, new-model diff. |
| `wrangler advice <launch\|experiment>` | Suggestions with evidence. `--json` is the export for agents. |
| `wrangler annotations <launch>` | Human review comments and reactions, grouped by variant. |
| `wrangler import skill-creator <evals.json>` / `wrangler export benchmark <launch>` | Interop with the skill-creator eval format. |
| `wrangler serve [--port 4477]` | The JSON API and web UI on localhost. |

A launch is one execution of an experiment. Commands that take `<launch>` accept its number, or the experiment name for its latest launch.

## The new-model workflow

```bash
wrangler suite create writing-skills --experiments rfw-writing --metric judge_score
```

```bash
wrangler suite run writing-skills --model claude-sonnet-5-5
```

When a new model ships, run the suite on it and read the diff:

```bash
wrangler suite run writing-skills --model claude-opus-5-5
```

```bash
wrangler suite diff writing-skills --model claude-opus-5-5
```

The diff lists patterns that regressed first, then ones the new model made redundant, then ones still earning their keep.

## Does a skill behave differently across models?

Cross the skill with a model factor in one experiment:

```toml
[[factors]]
name = "skill"
kind = "overlay"
levels = ["overlays/none", "plugins/reader-first-writing/skills/reader-first-writing"]

[[factors]]
name = "model"
kind = "model"
levels = ["claude-opus-4-8", "claude-opus-5", "claude-opus-5-5"]
```

The report then shows the skill's effect within each model, and for each later model how far its effect is from the first model's, with a 95% interval. A difference whose interval excludes zero is marked "differs"; otherwise the models are "not distinguishable" on this data.

## How results are computed

- Each cell (task × variant) runs N times; the default is 5.
- Comparisons are paired by task, and by the other factors when there are several. The effect is the mean of per-pair differences.
- Confidence intervals are 95% hierarchical bootstraps: resample the pairs, then the runs within each side. Resampling is seeded, so a report is reproducible.
- A comparison whose interval spans zero is "inconclusive", or "no effect" when the whole interval sits inside a small band (10 points for rates, 0.5 for judge scores, 10% of baseline for cost and time).
- Pairwise judging runs both orders. A win counts only when both orders agree; the disagreement rate is the position-bias check.
- With more than two variants, pairwise votes are aggregated with a Bradley–Terry model.

## Tests

```bash
cargo test
```

The live tests make real model calls and are off by default:

```bash
WRANGLER_LIVE=1 WRANGLER_LIVE_CODEX=1 cargo test -p wrangler-core --test live_runners -- --nocapture
```

## Known limits

- The Codex runner runs under Codex's own `workspace-write` sandbox by default and reports tokens but not dollar cost. Set `[runner] codex_sandbox` to change the sandbox mode.
- `isolation = "docker"` needs an image with the runner CLI and a token in the environment (`claude setup-token` gives one for a subscription); the keychain login cannot enter a container.
- `budget_usd` caps a launch's cost; Codex runs report no cost and do not count toward it.
- Runs use `bypassPermissions`. Use `isolation = "seatbelt"` for fixtures you do not trust.
