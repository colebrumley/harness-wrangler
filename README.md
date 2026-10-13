# Harness Wrangler

Controlled experiments over coding-agent harness patterns. It measures whether skills, `CLAUDE.md` files, hooks, system prompt appends, effort levels and models actually help, by how much, and which parts carry the effect.

It runs each task under each variant several times, grades every run, and reports the difference with a confidence interval. Not every skill can be tested this way, so read the next section before you write tasks for one.

## What it can test

A pattern is testable here when all four hold:

1. **Its effect shows inside one session**: in the final message, a file, the workspace diff or the transcript.
2. **The result can be checked**: by a rule, a test command, or a judge working from a rubric and the expectations you wrote.
3. **The baseline leaves room.** If the agent already gets the task right without the pattern, no variant can show a gain. The report calls this "no headroom".
4. **A run is cheap enough to repeat.** The default is five runs per cell, and a comparison needs several tasks.

### Good fits

| Kind of pattern | Example | How to test it |
|---|---|---|
| Shapes a written result | A writing or formatting skill | `word_count` and `regex` graders, plus a rubric judge with pairwise comparison |
| Changes code with a testable outcome | A bugfix or migration procedure | A `command` grader that runs the tests, and `diff_size` |
| Prescribes a process | "Run the tests before finishing", a pipeline with fixed phases | `sequence`, `tool_call` and `transcript_regex` graders over the transcript |
| Earns its value by firing at the right moment | Any skill with a description | A trigger experiment; `wrangler trigger-set` drafts the prompts |
| Changes how the agent deals with the user | "Ask one question before changing code" | A conversation task with a simulated user |
| Gives the agent a tool | An MCP server | An `mcp` factor |
| Is a setting, not a skill | `CLAUDE.md`, a system prompt append, effort, model | The matching factor; `wrangler ablate` finds which sections carry the effect |

### Fits that take work

- **Review and analysis skills** (code review, critique, security audit). The result is a judgment, so you have to build the ground truth: fixtures with planted defects and safe-looking decoys, each one verified. Small fixtures saturate. In one project a plain single pass found every planted defect in fixtures of up to 200 lines, which left a multi-agent review skill nothing to win. Run `wrangler run <experiment> --baseline-only` before paying for the variants.
- **Multi-agent and long-running skills.** They can be tested, but each run costs more, so you get fewer of them and wider intervals. In the same project the multi-agent skill cost $1.83 a run against $0.18 for the single pass. Set `budget_usd`.
- **Skills judged on taste** (tone, naming, what to leave out). The judge is a model. Vote on at least eight pairs with `wrangler review <launch> --for calibration` before you trust its scores; below 70% agreement the advice says so and lowers its confidence.
- **Skills that fire only sometimes.** An experiment on the skill's effect mixes "did it fire" with "did it help". Measure firing with a trigger experiment. To measure the effect alone, invoke the skill by name in the task prompt.
- **Small effects.** A difference inside the "no effect" band (10 points for rates, 0.5 for judge scores) needs many tasks to separate from noise.

### Poor fits

- **Skills that act outside the workspace**: deploy, open a pull request, post to Slack, file a ticket, send mail. Graders see the workspace, the final message and the transcript, and nothing else. Runs use `bypassPermissions` and repeat, so do not point one at a real account. Test such a skill against a local fake, or not here.
- **Skills whose payoff spans sessions**: memory, notes, anything that learns a person's preferences. Each run is one session in a fresh workspace.
- **Skills that need your machine or your accounts**: a browser, a desktop app, your credentials, your user-level settings. A run starts from the fixture and the overlay only.
- **Skills whose result has to be looked at**: UI design, slides, charts. The judge reads the final message or one output file, not a rendered page. You can check that the result builds, not that it looks right.
- **Permission and safety behaviour.** Runs bypass permissions inside a sandbox, so the tool measures what the agent does given instructions, not what a permission mode would block.

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

In the app, the Start page lists the steps from an empty project to acting on results and says which is next. "Ask the agent" opens a terminal beside the UI. It runs Claude Code in the project folder, briefed on this tool's file formats and commands, with the bundled `wrangler` CLI on its PATH.

The first time the app reads a project under Documents or Desktop, macOS asks whether to allow access. Choose Allow.

## A real experiment

`examples/experiments/rfw-writing.toml` asks whether `reader-first-writing` improves two writing tasks on Haiku, scored by a Sonnet judge. It makes 20 agent runs and about 30 judge calls.

```bash
cd examples && ../target/release/wrangler run rfw-writing
```

The first real run found that the skill was never invoked on these tasks under Haiku, and the advice said so.

## Recipes

### Does a skill still earn its keep on a new model?

Create a suite and take a baseline on the current model:

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

### Does a skill behave differently across models?

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

The report shows the skill's effect within each model. For each later model it also shows how far that effect is from the first model's, with a 95% interval. A difference whose interval excludes zero is marked "differs"; otherwise the models are "not distinguishable" on this data.

### Does a pattern change how the agent deals with the user?

`examples/experiments/ask-first.toml` compares no instructions against a one-line "ask one question before changing code when the task is underspecified", on a deliberately vague bugfix prompt. A simulated user (a second model with a persona and hidden information) answers the agent's questions and decides when the task is done. The experiment measures `questions_asked`, `user_satisfied` and what the question costs. See "Conversation tasks" in [docs/formats.md](docs/formats.md).

### Does a tool help?

`examples/experiments/mcp-fs.toml` runs the slugify task with and without a filesystem MCP server (`npx -y @modelcontextprotocol/server-filesystem`), under seatbelt with the npm registry allowed. An `mcp` factor's levels are MCP config files. Exactly the level's servers load, and their calls are counted as `mcp.<server>.<tool>`. See "Tools and MCP servers" in [docs/formats.md](docs/formats.md).

## CLI

Run commands inside a project folder, or pass `-C <dir>`. Every command takes `--json`. A launch is one execution of an experiment; commands that take `<launch>` accept its number, or the experiment name for its latest launch.

| Command | Does |
|---|---|
| `wrangler init [dir]` | Create a project layout. |
| `wrangler doctor` | Check the runner CLIs are installed. |
| `wrangler list experiments\|tasks\|overlays\|launches\|suites` | List things. |
| `wrangler validate <experiment>` | Check a definition and show its cells. Runs nothing. |
| `wrangler run <experiment> [--reps N] [--concurrency N] [--no-judge] [--budget USD] [--baseline-only]` | Run every cell, grade, judge, print the report. Ctrl-C stops cleanly. Baseline runs go first; `--baseline-only` stops after them and prints per-task headroom. |
| `wrangler resume <launch> [--retry-failed] [--budget USD]` | Finish an interrupted launch, or run the variants of one left at its baseline. |
| `wrangler judge <launch>` | Run or finish the LLM judge phase. |
| `wrangler status <launch>` | Per-cell progress and cost. |
| `wrangler report <launch\|experiment> [--metric m]` | Variants, factor effects with 95% CIs, per-task table, pairwise, ablation. |
| `wrangler runs <launch>` / `wrangler show <run> [--output\|--diff\|--transcript]` | Drill into runs. |
| `wrangler ablate <skill-or-CLAUDE.md> [--description-only] [--depth N]` | Generate leave-one-section-out overlays and their experiment. `--description-sentences` ablates the description one sentence at a time instead, for trigger experiments. `--placebo` adds a matched-length placebo variant, to tell "these instructions" from "any instructions". |
| `wrangler placebo <file> [-o out]` | Write a placebo of any Markdown or text file. |
| `wrangler trigger-set <skill> [--n 20] [--model m]` | Draft should-fire and near-miss should-not prompts from a skill's description, as draft tasks to review. |
| `wrangler suite create\|run\|status\|diff` | Tracked suites, per-model baselines, new-model diff. |
| `wrangler advice <launch\|experiment>` | Suggestions with evidence. `--json` is the export for agents. |
| `wrangler annotations <launch>` | Human review comments and reactions, grouped by variant. |
| `wrangler review <launch> [--for calibration] [--vote left\|right\|tie\|both_bad\|skip --left <run> --right <run>]` | Blind pairwise review from the terminal: print the next pair, or record a vote. |
| `wrangler calibration [--judge m] [--rubric r] [--tag t]` | How often the judge agrees with human votes, pooled over the project by judge model, rubric and task tag. |
| `wrangler import session <session.jsonl\|id> [--turn N] [--draft-expectations]` | Draft a task from a Claude Code session: its prompt, the repository as it was, and the tests it ran. See [docs/harvesting.md](docs/harvesting.md). |
| `wrangler import run <run>` | Draft a task from a run, with the run's output as the judge's reference. |
| `wrangler task check <task> [--reps 3] [--experiment e]` | Run one task on the baseline and print its pass rate, spread and headroom, before it joins an experiment. |
| `wrangler import skill-creator <evals.json>` / `wrangler export benchmark <launch>` | Interop with the skill-creator eval format. |
| `wrangler serve [--port 4477]` | The JSON API and web UI on localhost. |

## How results are computed

- Each cell (task × variant) runs N times; the default is 5.
- Comparisons are paired by task, and by the other factors when there are several. The effect is the mean of per-pair differences.
- Confidence intervals are 95% hierarchical bootstraps: resample the pairs, then the runs within each side. Resampling is seeded, so a report is reproducible.
- A comparison whose interval spans zero is "inconclusive", or "no effect" when the whole interval sits inside a small band (10 points for rates, 0.5 for judge scores, 10% of baseline for cost and time).
- Headroom: a task whose baseline is at the metric's bound with no variance (every run passes, or judge scores of 9 and up with almost no spread) cannot show a difference. Comparisons leave such a task out when the other variant sits at the same bound, and say how many were left out. When every task is saturated the verdict is "no headroom", which counts as inconclusive, never as "no effect".
- A cell needs five runs before its spread is called flaky.
- Pairwise judging runs both orders. A win counts only when both orders agree; the disagreement rate is the position-bias check.
- With more than two variants, pairwise votes are aggregated with a Bradley–Terry model.

## What is in this repository

| Path | What |
|---|---|
| `crates/wrangler-core` | The library: parsing, scheduling, runners, isolation, graders, SQLite store, statistics, ablation, suites, advice. |
| `crates/wrangler-cli` | The `wrangler` CLI. Text for people, `--json` for agents and CI. |
| `apps/desktop` | The desktop app: Tauri 2 shell plus a TypeScript frontend. |
| `examples/` | A ready project: tasks, overlays (including a copy of `reader-first-writing` 1.1.0), experiments, and suites. |
| `docs/` | [File formats](docs/formats.md), [isolation and what was measured](docs/isolation.md), [harvesting tasks](docs/harvesting.md), [requirements for the next round](docs/requirements/README.md). |
| [DESIGN.md](DESIGN.md) | The requirements this was built from. |

## Tests

```bash
cargo test
```

The live tests make real model calls and are off by default:

```bash
WRANGLER_LIVE=1 WRANGLER_LIVE_CODEX=1 cargo test -p wrangler-core --test live_runners -- --nocapture
```

To script the app itself, set `WRANGLER_PICK_FOLDER=<dir>` so the folder buttons use that path instead of the system picker. `WRANGLER_UI_SCRIPT="Switch project|Create project in a folder"` presses the named buttons in order and writes what each screen showed to `WRANGLER_UI_REPORT`.

## Known limits

- The Codex runner runs under Codex's own `workspace-write` sandbox by default and reports tokens but not dollar cost. Set `[runner] codex_sandbox` to change the sandbox mode.
- `isolation = "docker"` needs an image with the runner CLI and a token in the environment (`claude setup-token` gives one for a subscription); the keychain login cannot enter a container.
- `budget_usd` caps a launch's cost; Codex runs report no cost and do not count toward it.
- Runs use `bypassPermissions`. Use `isolation = "seatbelt"` for fixtures you do not trust.
- Conversation tasks (a simulated user) run under Claude Code and the mock runner only, and not under Docker.
- An MCP server launched with something other than `npx` or `uvx` may need its cache directory added to the seatbelt profile; see docs/isolation.md.
