# File formats

A project is a folder, usually a git repo you own:

```
wrangler.toml          marker; optional data_dir = "..."
experiments/<name>.toml
tasks/**/<id>.toml
overlays/<name>/       plain directories; overlays/none is the empty baseline
fixtures/<name>/
rubrics/<name>.md
suites/<name>.toml
.wrangler/             database and run artifacts (gitignored)
```

All paths inside files are relative to the project root.

## Experiment

```toml
name = "rfw-effort"
description = "Does rfw help, and does effort matter?"
runs_per_cell = 5            # default 5
timeout_secs = 900           # per run, default 900
isolation = "seatbelt"       # "workspace" (default), "seatbelt", or "docker"
network = "anthropic"        # seatbelt only: "full" (default) or "anthropic"
primary_metric = "judge_score"   # optional; default judge_score, then pass
budget_usd = 25.0            # optional cap for one launch, agent + judge cost
keep_workspaces = false      # keep each run's temp workspace on disk

[runner]
kind = "claude-code"         # "claude-code", "codex", "mock"
concurrency = 3              # runs at a time, default 3
model = "claude-sonnet-5-5"  # default when no model factor
effort = "medium"            # default when no effort factor
max_turns = 40               # optional
plugin_dirs = ["plugins/x"]  # passed with --plugin-dir, never installed
max_retries = 4              # rate-limit retries per run
docker_image = "my/claude:1" # isolation = "docker": image with the runner CLI
docker_env = ["ANTHROPIC_API_KEY"]   # host env vars passed in by name
docker_args = ["--network", "none"]  # extra `docker run` arguments

[[factors]]
name = "overlay"
kind = "overlay"
levels = ["overlays/none", "overlays/rfw-v1.1.0"]

[[factors]]
name = "effort"
kind = "effort"
levels = ["medium", "high"]

[tasks]
include = ["tasks/writing/*.toml"]
exclude = []

[graders.default]
judge = { model = "claude-opus-5-5", rubric = "rubrics/reader-first.md", pairwise = true }
human = { enabled = true }
deterministic = []           # graders applied to every task
```

The first level of each factor is its baseline. Variants are the cross product of all levels.

### Factor kinds

| `kind` | Levels are | Notes |
|---|---|---|
| `overlay` | overlay directories, or skill directories | An overlay directory is laid out like a project (`.claude/skills/...`, `CLAUDE.md`). A directory with `SKILL.md` at its root is installed as a skill directly, so a level can point at a skill where it lives in your repo (`plugins/x/skills/my-skill`). `overlays/none` or `""` is the empty baseline. Several overlay factors stack in order. |
| `model` | model ids | |
| `effort` | `low`, `medium`, `high`, `xhigh`, `max` | |
| `runner` | runner ids | A level may be a table carrying that runner's model: `{ value = "codex", model = "gpt-5.5", effort = "low" }`. |
| `system_prompt_append` | text, or `file:path` | `""` means no append. |
| `knob` | values for a CLI flag or env var | Needs `flag = "--some-flag"` or `env = "NAME"`. `on`/`off` toggles a bare flag. |

A level can be written as a table to give it a label: `{ value = "overlays/rfw-v1.2-draft", label = "draft" }`.

### Judge options

`judge = { model, effort, rubric, rubric_scoring = true, pairwise = false, pairwise_mode, pairwise_reps, transcript }`

- `rubric_scoring` scores each run 1 to 10 and checks each task expectation, with quoted evidence.
- `pairwise` compares runs of the same task blind, in both orders.
- `pairwise_mode`: `all` variant pairs, or `baseline` (each variant against the first). Default is `all` up to four variants.
- `pairwise_reps`: repetitions paired per task and variant pair. Default `min(runs_per_cell, 3)`.
- `transcript = "tools"` also shows the judge the process: one line per tool call (name and a short input, no results), in order. Off by default; when on, the trace is stored with each judgment.

## Task

```toml
id = "incident-summary"
prompt = "Read INCIDENT.md and write a summary for the on-call channel. Save it as SUMMARY.md."
# prompt_file = "tasks/prompts/long.md"   # instead of prompt
fixture = { dir = "fixtures/incident-report" }
# fixture = { repo = "fixtures/some-repo", ref = "v1.2.0" }   # git clone + checkout
output_file = "SUMMARY.md"   # what judges and reviewers read; default is the final message
expected_output = "A short, plain summary under 150 words leading with impact."
expectations = ["Leads with user impact", "Under 150 words", "No hedging phrases"]
tags = ["writing"]
timeout_secs = 600           # overrides the experiment
max_turns = 2                # overrides [runner] max_turns; reaching a task's own cap is a normal end, not a timeout
trigger = "should"           # trigger experiments: "should" or "should_not" (see Trigger experiments)

[graders]
judge = true                 # false skips the LLM judge for this task
human = true                 # false keeps it out of the review queue
deterministic = [
  { kind = "word_count", path = "SUMMARY.md", max = 150 },
]
```

### Deterministic graders

| `kind` | Fields | Passes when |
|---|---|---|
| `command` | `cmd`, `expect_exit = 0`, `timeout_secs` | the shell command, run in the workspace, exits as expected |
| `file_exists` | `path` | the file exists |
| `regex` | `pattern`, `path`, `should_match = true` | the pattern matches the file (or the output when `path` is absent) |
| `diff_size` | `min`, `max` | changed lines (added + removed) are within bounds |
| `word_count` | `path`, `min`, `max` | word count is within bounds |
| `json_schema` | `path`, `schema` or `schema_file` | the JSON validates |
| `skill_invoked` | `skill`, `expect = true` | the skill was (or was not) invoked |
| `tool_call` | `name` (string or list), `input`, `min = 1`, `max`, `subagents = "include"`, `label` | the count of matching tool calls is within bounds |
| `sequence` | `steps = [{ name, input }, ...]`, `before_final = true` | each step matches a tool call, in order, each after the previous; with `before_final`, all before the last assistant message |
| `transcript_regex` | `pattern`, `scope = "assistant"`, `should_match = true` | the pattern matches the chosen text |
| `hook_fired` | `event`, `min = 1`, `max` | the count of hook executions for that event is within bounds |

The last four read the normalized transcript, so they grade the process and not the result: "ran the tests before finishing", "read the file before editing it", "never called `rm`".

- A tool `name` is exact, or a prefix when it ends in `*` (`mcp.filesystem.*`). A list covers both runners with one grader: `name = ["Bash", "shell"]`. In `tool_call`, `name` is the tool, so the grader's own name is `label`.
- `input` is a regex over the call's input serialized as JSON: `input = "unittest|pytest"` matches a Bash call whose command runs either.
- `subagents` is `include`, `exclude` or `only`.
- `transcript_regex` scopes: `assistant` (the agent's messages), `tool_results`, `thinking`, or `all` (those three plus user turns).
- `hook_fired` counts hook executions by event name (`PreToolUse`, `PostToolUse`, `Stop`, ...). Claude Code reports hooks; Codex and the mock runner do not, and there the grader fails with "runner does not report hooks" instead of passing with nothing to count.
- Each of these records what it matched in the grade's detail (call ids, the turn of each, a one-line input), and a failed `sequence` names the missing step and why: never called, out of order, or only after the final message. The run page links each matched call to its turn.

```toml
deterministic = [
  { kind = "sequence", steps = [{ name = ["Bash", "shell"], input = "unittest|pytest" }], before_final = true, name = "tests-run-first" },
  { kind = "tool_call", name = "Edit", input = "tests/", max = 0, min = 0, label = "tests-not-edited" },
  { kind = "hook_fired", event = "PreToolUse" },
]
```

Every grader takes an optional `name`. Give a fixture a `.gitignore` for build debris (`__pycache__/`, `node_modules/`): ignored files stay out of the diff, `diff_size` and `files_touched`.

Every grader takes an optional `name`. The baseline commit is tagged `wrangler-baseline`, so a command grader can check `git diff --quiet wrangler-baseline -- tests`.

## Judge calibration

The pairwise judge checks itself for position bias, but only a person can say whether it picks what a person would. Every human vote in Review lands on a pair, and where the judge voted on the same pair the two are compared.

The report's calibration section gives, overall and per task tag: the number of pairs with both votes, raw agreement, Cohen's kappa with a bootstrap interval, agreement restricted to pairs where the judge was consistent across orders, and how often the higher rubric score went to the result the person picked (which says whether the 1 to 10 scores track preference, or only the pairwise votes do). Human ties and skips are left out; "both bad" votes are counted on their own, since a high rate says something about the task. Fewer than eight pairs reads "not enough human votes to calibrate".

- `wrangler review <launch> --for calibration` (and the Review page's "Calibrate the judge" mode) serves pairs that have a judge vote, spread over tasks and tags, the judge's shakiest calls first: pairs where it disagreed with itself, then pairs whose rubric scores are close. It never offers an unjudged pair while a judged one remains.
- `wrangler calibration [--judge m] [--rubric r] [--tag t]` pools every judgment in the project's database by judge model and rubric and prints the same table.
- Advice uses it. Where agreement is below 70%, or there are too few votes, every suggestion that rests on `judge_score` drops one confidence step and says so; a suggestion about named tasks is held to those tasks' tags, one about the whole launch to the overall figure. `calibrate_judge` says how many more pairs reach eight per tag.
- A suite stores the calibration beside each baseline, so a diff on a judge-scored suite says whether the judge was trusted when each baseline was taken.

## Placebo overlays

A measured gain from a skill or a `CLAUDE.md` may come from having instructions at all. `wrangler ablate <source> --placebo` adds a variant named `placebo` that tells the two apart: for a skill, the frontmatter is kept byte for byte (so it triggers the same way) and the body is replaced; for a `CLAUDE.md`, the whole file is replaced. The replacement has the same Markdown shape (a heading for each heading, a list item for each list item, a paragraph for each paragraph) and an estimated token count within ten percent of the original's.

What a placebo contains: sentences that restate what an agent does anyway, such as "A request arrives as text." and "Tools are available during the session." The built-in corpus is `crates/wrangler-core/src/placebo/corpus.md`. A sentence belongs there only if deleting it could not plausibly change an agent's output, which rules out anything about quality, length, tone, care, checking, testing, planning, asking, or order of work; a test holds the corpus to a list of banned words. The corpus is finite, so a long file's placebo repeats sentences, never two in a row. `--placebo-file <path>` substitutes your own sentences, one per line.

The ablation report then shows `full vs placebo` and `placebo vs none` beside `full vs none`, and attributes the whole file:

| Attribution | When |
|---|---|
| `content` | the full file beats the placebo |
| `presence` | the placebo matches the full file and beats no file: the gain comes from having instructions, and advice says a shorter file would do (`placebo_explains`) |
| `neither` | nothing beats no file |
| `inconclusive` | the intervals do not separate these |

A placebo is also just an overlay. It is written under `overlays/ablation/<experiment>/placebo/` and can be a level of any overlay factor. `wrangler placebo <file> [-o out]` writes a placebo of any Markdown or text file, for use as an overlay's `CLAUDE.md` or as a `file:` level of a `system_prompt_append` factor.

## Trigger experiments

For most skills the value is firing at the right moment, and firing on unrelated prompts is a cost on every run. A trigger experiment measures both directions. Triggering is decided from the description and the prompt before the body is read, so these runs stop after a turn or two and need no fixture and no judge.

```toml
name = "rfw-trigger"
runs_per_cell = 3
primary_metric = "trigger_f1"

[runner]
kind = "claude-code"
max_turns = 2

[trigger]
skill = "reader-first-writing"      # or skills = ["a", "b"]

[[factors]]
name = "overlay"
kind = "overlay"
levels = ["overlays/rfw-v1.1.0"]     # one level: a baseline without the skill can never fire

[[factors]]
name = "model"
kind = "model"
levels = ["claude-haiku-5-5", "claude-sonnet-5-5"]

[tasks]
include = ["tasks/trigger/reader-first-writing/*.toml"]
```

A trigger task is a prompt and an expectation, nothing else:

```toml
id = "rfw-should-not-rename-function"
prompt = "Rename the function send_emails to dispatch_notifications everywhere in this repo."
trigger = "should_not"
max_turns = 2
```

Two turns is enough when the prompt can be acted on at once. A prompt that sends the agent exploring first ("explain how to run the tests in this repo") can spend both turns on `ls` and never reach the point where it would load the skill; that reads as a miss. Keep trigger prompts self-contained, or raise `max_turns` on the tasks that need it.

`[trigger]` turns every task that carries `trigger` into a `skill_invoked` check for the named skill, so `pass` on a trigger task means "fired when it should, or stayed quiet when it should not". Should-not prompts are only worth having when they are near misses: the same domain and vocabulary as the description, at the wrong moment for the skill.

The report gains a trigger section per variant: `trigger_precision`, `trigger_recall` and `trigger_f1` with bootstrap intervals over tasks, the should-fire prompts where the skill never fired, the should-not prompts where it did, and a row per model when there is a model factor. All three are valid `primary_metric` values; comparisons between variants are paired by task. Per run, `trigger_fired` is 1 or 0.

- `wrangler trigger-set <skill> [--n 20] [--model m]` drafts prompts of both kinds from the description and writes them under `tasks/trigger/<skill>/` tagged `draft`. Review them before use; `wrangler validate` lists tasks still tagged `draft`.
- `wrangler ablate <skill> --description-sentences` writes one variant per sentence removed from the description, with the body byte-identical, and the report attributes precision and recall to each sentence. Advice then names the sentence most likely behind an overfire.

## Metrics

Per run, when the runner provides them: `pass` (all deterministic graders passed), `checks_passed`, `judge_score`, `expectations_passed`, `check.<grader>` (each deterministic grader's own pass rate), `skill_invoked`, `trigger_fired`, `cost_usd`, `judge_cost_usd`, `wall_time_s`, `total_tokens`, `input_tokens`, `output_tokens`, `thinking_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `cache_hit`, `turns`, `tool_calls`, `tool.<Name>`, `mcp.<server>.<tool>`, `tool_calls_before_final`, `distinct_tools`, `hook.<Event>`, `diff_lines`, `diff_added`, `diff_removed`, `files_touched`, `output_words`, `retries`, `permission_denials`, `subagents_spawned`.

## Suite

```toml
name = "writing-skills"
primary_metric = "judge_score"
experiments = ["rfw-writing"]
```

Each experiment needs an overlay factor whose first level is the no-pattern baseline. A suite run pins every experiment to one model: any `model` factor is replaced by that model.

## Run artifacts

`.wrangler/runs/<launch>/<run>/` holds `raw.jsonl` (the runner's own output), `transcript.json` (normalized), `diff.patch`, `final.txt`, `meta.json`, `stderr.txt`, `files/` (files the agent touched), and `sandbox.sb` when sandboxed.

The normalized transcript is the same for every runner:

```json
{ "turns": [ { "role": "assistant", "text": "", "thinking": "", "subagent": false,
               "tool_calls": [ { "id": "", "name": "", "input": {} } ],
               "tool_results": [ { "tool_use_id": "", "content": "", "is_error": false } ] } ] }
```

Codex shell commands appear as `shell` tool calls and file changes as `apply_patch`. MCP tools are named `mcp.<server>.<tool>` under both runners (Claude Code's own `mcp__server__tool` is rewritten).

`hook.<Event>` counts the hook executions Claude Code reported for that event. Each is also a run event of kind `hook` carrying the event name, matcher, exit code and output; `wrangler show <run>` lists them.
