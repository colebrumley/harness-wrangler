# 1. Transcript graders

Value: high. Difficulty: low.

## Purpose

Many patterns shape the process, not the output. "Run the tests before you finish." "Read a file before editing it." "Never touch `tests/`." A `PreToolUse` hook that blocks `rm -rf`. Today none of these is measurable: graders read workspace files, the final message and the diff, and the only transcript-aware grader is `skill_invoked`. The terse-coding experiment carries a "run the project's tests" line whose effect cannot be seen, which is one reason it came back as no effect.

The transcript is already normalized for every runner (`transcript.json`, one shape for Claude Code and Codex). This item adds graders that read it.

## Requirements

1. Three new deterministic grader kinds, in `DetSpec` (`model.rs`) and `graders/deterministic.rs`:

   | `kind` | Fields | Passes when |
   |---|---|---|
   | `tool_call` | `name` (string or list), `input` (regex over the JSON-serialized input, optional), `min = 1`, `max`, `subagents = "include"` | the count of matching tool calls is within bounds |
   | `sequence` | `steps = [{ name, input }, ...]`, `before_final = true` | each step matches a tool call, in order, each after the previous; with `before_final`, all before the last assistant message |
   | `transcript_regex` | `pattern`, `scope = "assistant" \| "tool_results" \| "thinking" \| "all"`, `should_match = true` | the pattern matches the chosen text |

   `name` accepts a list so one grader covers both runners: `name = ["Bash", "shell"]`. `subagents` is `include`, `exclude` or `only`.

2. Each grader records what it matched in `detail_json`: the matching call ids and a short rendering of their inputs, so a failed `sequence` shows which step was missing and the Results page can link to the turn.

3. Hook events are captured. The Claude Code runner records every hook execution it can see in the stream as a `RunEvent` of kind `hook` carrying the hook event name, matcher, exit code and output, and as a metric `hook.<Event>` (count). A `hook_fired` grader (`event`, `min = 1`, `max`) passes on the count. Codex reports `Support::None` for hooks already; the grader fails with "runner does not report hooks" rather than passing vacuously.

4. New metrics, computed for every run: `tool_calls_before_final` and `distinct_tools`. `tool.<Name>` stays as is. MCP tool names are normalized to `mcp.<server>.<tool>` in both runners (Codex already does this; Claude Code emits `mcp__server__tool`).

5. The judge can be shown the process. `judge = { transcript = "tools" }` adds a compact tool-call trace (name and a one-line input summary per call, no results) to the rubric and pairwise prompts. Default stays off; the trace is recorded in the judgment detail when on.

6. `docs/formats.md` gains the three graders and the hook metric. `examples/tasks/coding/fix-slugify.toml` gains `{ kind = "sequence", steps = [{ name = ["Bash", "shell"], input = "unittest|pytest" }], before_final = true, name = "tests-run-first" }`, which makes the terse overlay's second line measurable.

## Where it lands

- `crates/wrangler-core/src/model.rs`: `DetSpec` variants, `JudgeCfg.transcript`.
- `crates/wrangler-core/src/graders/deterministic.rs`: the three graders. `GradeInput` already carries the transcript.
- `crates/wrangler-core/src/transcript.rs`: `tool_calls_with_turn()` so a grader knows ordering relative to the final message; MCP name normalization.
- `crates/wrangler-core/src/runner/claude.rs`: hook events from stream-json; `hook.<Event>` metrics.
- `crates/wrangler-core/src/graders/judge.rs`: optional trace in prompts.
- `apps/desktop`: the run page shows matched calls for a grade when the detail has call ids.

## Acceptance

- Unit tests against the `SAMPLE` transcript in `runner/claude.rs` and a hand-built two-runner transcript: `tool_call` with a list name counts Bash and shell together; `sequence` fails when steps are out of order and when a step comes after the final message; `transcript_regex` with `scope = "thinking"` only sees thinking.
- A live test (gated by `WRANGLER_LIVE`) with an overlay that installs a `PreToolUse` hook on Bash asserts `hook.PreToolUse >= 1` and that the hook event carries the exit code.
- Re-running terse-coding on the example project reports `tests-run-first` as a separate pass metric with a value that differs between the two overlays or is honestly tied.

## Out of scope

Grading what a hook *prevented*: permission and safety gating stays out (DESIGN.md §2). A hook's effect on the output is measured the usual way.

## Open questions

- What stream-json emits for hooks. The spike must list the event types with Claude Code 2.1.x; if hooks are invisible in the stream, fall back to a hook wrapper the runner injects into the overlay's `settings.json` that writes a line to `<run_dir>/hooks.jsonl`.
- Whether `input` should be a regex over serialized JSON or a field path plus regex. Start with serialized JSON; add `field = "command"` if the regexes get ugly.
