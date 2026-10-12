# Harvesting tasks

Tasks are the bottleneck. Writing tasks are cheap to write; coding tasks with a fixture and tests are where the effort goes. Harvesting does the deterministic part and leaves the judgment to you or an agent: it takes the prompt, snapshots the repository as it was, proposes graders from what the session ran, and writes a draft.

## From a Claude Code session

```bash
wrangler import session <session.jsonl | session-id> [--turn N] [--draft-expectations] [--include <glob>] [--max-mb N] [--id name]
```

A session id is looked up under `~/.claude/projects/*/<id>.jsonl`. The command writes `tasks/harvested/<id>.toml` and `fixtures/<id>/` and prints what it did.

| Field | Comes from |
|---|---|
| `prompt` | the first user message, or the user message at `--turn N` |
| `fixture` | the repository at the session's start (see below) |
| `[graders] deterministic` | a `command` grader for each test command the session ran after the prompt (recognized by `pytest`, `cargo test`, `npm test`, `go test`, `unittest`), and a `diff_size` bound at twice the session's own change |
| `expected_output`, `expectations` | empty, unless `--draft-expectations` asks a model to draft them from the rest of the session |
| `tags` | `harvested`, `draft`, and the project's directory name |
| the comment at the top | session id, turn, date and Claude Code version |

Harvested tasks are ordinary tasks. Nothing treats them differently except the `draft` tag, which `wrangler validate` lists so you remember to review them. Before a new task joins an experiment, run it on the baseline:

```bash
wrangler task check <task> [--reps 3] [--experiment <name>] [--model m] [--judge-model m] [--rubric r]
```

It runs the task with no overlay, three times by default, and prints the pass rate, the judge score when a judge is given, the spread, and the headroom verdict: a task the baseline passes every time cannot show a benefit. `--experiment` borrows that experiment's runner, model, isolation and judge.

### The fixture

In order of preference:

1. **A commit.** The repository is exported at the commit the session started from with `git archive`: no `.git`, no untracked files, no gitignored files. The session file does not record `HEAD`, so the commit is found one of two ways: a hash printed by the session's own first `git rev-parse HEAD` or `git log`, or else the last commit before the session's first timestamp. The second is a guess that is wrong when the session started from uncommitted work, and the import says which was used.
2. **Files that were uncommitted at the start.** If the session edited (not created) a file that the commit does not have, that file existed uncommitted when the session began. Its content then is gone, so the fixture gets it as it is now, and the import names each such file for you to check.
3. **The current state.** With no git repository or no usable commit, the directory is copied as it is now, without gitignored files, with a warning: if the session's work is still in the tree, the task is already done in the fixture.

Files matching `.env*`, `*.pem`, `*_key*`, `id_rsa*` or `credentials*` are never copied; their names are printed. `--include <glob>` lets a named file through (`--include .env.example`). Above 50 MB the import stops before copying anything; `--max-mb` raises the cap.

`--turn N` with N above 1 takes the prompt from a later message, but the fixture is still the repository before the session: the work the agent did in earlier turns is not in it. The import warns about this.

### Session format assumptions

The session JSONL is not a documented format and changes between versions. These are the fields relied on, read from sessions written by Claude Code 2.1.295 and 2.1.296. Anything else is ignored; a line that breaks one of these is an error naming the line, not a guess.

- One JSON object per line, each with a string `type`.
- `type: "user"` and `type: "assistant"` entries carry `message.content`: a string, or a list of blocks.
- On those entries: `cwd` (the working directory), `sessionId`, `version`, `gitBranch`, `timestamp`, `isSidechain` (true inside a subagent; such entries are skipped), `isMeta` (true for text the harness injected; skipped as prompts).
- Assistant blocks: `{type: "text", text}` and `{type: "tool_use", name, input}`. `Bash` calls carry `input.command`; `Edit` carries `file_path`, `old_string`, `new_string`; `Write` carries `file_path`, `content`.
- User blocks: `{type: "text", text}` and `{type: "tool_result", content}` where content is a string or a list of text blocks.
- A user entry is a prompt when it has text, is not meta, and does not begin with a harness wrapper (`<command-name>`, `<local-command-...>`, `<task-notification>`). `<system-reminder>` blocks are stripped from it.
- Other entry types (`attachment`, `system`, `file-history-snapshot`, `queue-operation`, `last-prompt`, and whatever comes next) are skipped.

Not in the file: the commit at session start, and the content of files before the session edited them. Those two gaps are why the fixture rules above exist.

## From a wrangler run

```bash
wrangler import run <run-id> [--id name]
```

The prompt, fixture and graders are the run's own task's. The run's output becomes `reference_output`, a task field the rubric judge is shown as one result that was accepted for this task. The pairwise judge is not shown it. The run page in the app has a "Harvest as a task" button that does the same.

## Not harvested

- Codex sessions. Their format has not been looked at.
- Expectations without the flag. Model-written expectations are only drafted when you ask.
