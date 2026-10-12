# 3. Task harvesting

Value: high. Difficulty: medium-high.

## Purpose

Tasks are the bottleneck. The example project holds two writing tasks and one coding task. Writing tasks are cheap to write; coding tasks with a fixture and tests are where the effort goes, and they are what every pattern beyond writing needs. DESIGN.md §2 made transcript harvesting a non-goal on the grounds that an agent can write tasks from transcripts. That is true for the prompt and the expectations and false for the fixture: snapshotting the repository as it was when the session started is tedious and error-prone by hand.

This item does the deterministic part in the tool and leaves the judgment to the user or an agent.

## Requirements

1. `wrangler import session <session.jsonl | session-id> [--turn N]` reads a Claude Code session file and writes a draft task:
   - `prompt` is the first user message, or the user message at turn N.
   - `fixture = { dir = "fixtures/<id>" }` is the repository state at that turn. See requirement 3.
   - `expected_output` and `expectations` are left empty unless `--draft-expectations` is given, in which case the judge model drafts them from the rest of the session (what the agent did, what the user accepted) and marks the task `draft`.
   - `[graders] deterministic` proposes `command` graders for any test command the session ran (recognized by a small pattern list: `pytest`, `cargo test`, `npm test`, `go test`, `unittest`) and a `diff_size` bound at twice the session's own diff.
   - `tags` gets `harvested` and the session's project name.
   - A `source` comment records the session id, turn and date.

2. `wrangler import run <run-id>` does the same from a wrangler run: the prompt and fixture are the task's own, and the run's output becomes a reference the judge can use (`reference_output`, a new optional task field shown to the rubric judge).

3. Fixture capture, in order of preference:
   - The session's `cwd` is a git repository and the session records `HEAD` at start: `git worktree`-free export of that commit into `fixtures/<id>/` (`git archive`), plus the uncommitted working tree if the session shows edits to files not in that commit; the user is told which.
   - Otherwise a copy of the directory as it is now, with gitignored files excluded and a warning that this is the current state, not the state at session start.
   - Files matching a default secret list (`.env*`, `*.pem`, `*_key*`, `id_rsa*`, `credentials*`) are never copied; the names are printed. `--include <glob>` overrides per file.
   - Above 50 MB the import stops and says so; `--max-mb` raises the cap.

4. `wrangler task check <task> [--reps 3]` runs the task on the baseline (no overlay, the experiment's default model) and prints pass rate, judge score if a rubric is given, variance and the headroom verdict from item 5. New tasks go through this before they join an experiment; the Start page lists it as a step.

5. Harvested tasks are ordinary tasks. Nothing downstream treats them differently except the `draft` tag, which `wrangler validate` lists so the user remembers to review them.

## Where it lands

- New `crates/wrangler-core/src/harvest.rs`: session parsing, fixture export, grader proposal. `interop.rs` keeps the skill-creator import.
- `crates/wrangler-core/src/model.rs`: `Task.reference_output`.
- `crates/wrangler-core/src/graders/judge.rs`: show the reference when present.
- `crates/wrangler-cli`: `import session`, `import run`, `task check`.
- `apps/desktop`: Start page step; a "Harvest" action on a run's page.
- `docs/formats.md`: the task fields; a new `docs/harvesting.md` for the session format assumptions.

## Acceptance

- Unit tests over a recorded session fixture (checked in under `crates/wrangler-core/tests/fixtures/session.jsonl`, scrubbed): prompt extraction by turn, test-command recognition, secret exclusion, size cap.
- A test that exports a fixture from a temp git repository at a given commit and verifies the working-tree edits are reported.
- End to end on the example project: import a run from the terse-coding launch, run `task check`, and the task joins an experiment without edits.

## Out of scope

- Harvesting from Codex sessions. Add when the Codex session format has been looked at.
- Automatic expectation drafting without the flag. The user must ask for model-written expectations.

## Open questions

- The Claude Code session JSONL format is not documented and changes between versions. The spike reads sessions from 2.1.x and lists the fields relied on; the parser fails loudly on an unknown shape rather than guessing.
- Whether `HEAD` at session start is recorded anywhere in the session file. If not, the import can look for the first `git` command output in the transcript that reveals it, and otherwise falls back to the current state.
