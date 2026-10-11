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
| `overlay` | overlay directories | `overlays/none` or `""` is the empty baseline. Several overlay factors stack in order. |
| `model` | model ids | |
| `effort` | `low`, `medium`, `high`, `xhigh`, `max` | |
| `runner` | runner ids | A level may be a table carrying that runner's model: `{ value = "codex", model = "gpt-5.5", effort = "low" }`. |
| `system_prompt_append` | text, or `file:path` | `""` means no append. |
| `knob` | values for a CLI flag or env var | Needs `flag = "--some-flag"` or `env = "NAME"`. `on`/`off` toggles a bare flag. |

A level can be written as a table to give it a label: `{ value = "overlays/rfw-v1.2-draft", label = "draft" }`.

### Judge options

`judge = { model, effort, rubric, rubric_scoring = true, pairwise = false, pairwise_mode, pairwise_reps }`

- `rubric_scoring` scores each run 1 to 10 and checks each task expectation, with quoted evidence.
- `pairwise` compares runs of the same task blind, in both orders.
- `pairwise_mode`: `all` variant pairs, or `baseline` (each variant against the first). Default is `all` up to four variants.
- `pairwise_reps`: repetitions paired per task and variant pair. Default `min(runs_per_cell, 3)`.

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

Every grader takes an optional `name`. Give a fixture a `.gitignore` for build debris (`__pycache__/`, `node_modules/`): ignored files stay out of the diff, `diff_size` and `files_touched`.

Every grader takes an optional `name`. The baseline commit is tagged `wrangler-baseline`, so a command grader can check `git diff --quiet wrangler-baseline -- tests`.

## Metrics

Per run, when the runner provides them: `pass` (all deterministic graders passed), `checks_passed`, `judge_score`, `expectations_passed`, `skill_invoked`, `cost_usd`, `judge_cost_usd`, `wall_time_s`, `total_tokens`, `input_tokens`, `output_tokens`, `thinking_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `cache_hit`, `turns`, `tool_calls`, `tool.<Name>`, `diff_lines`, `diff_added`, `diff_removed`, `files_touched`, `output_words`, `retries`, `permission_denials`, `subagents_spawned`.

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

Codex shell commands appear as `shell` tool calls and file changes as `apply_patch`.
