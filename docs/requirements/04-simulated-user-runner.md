# 4. Simulated-user runner

Value: high. Difficulty: high.

## Purpose

Every run is `claude -p` with one prompt and stdin closed. Patterns about interaction cannot be measured: asking a clarifying question when the task is ambiguous, proposing a plan before acting, handling a correction, stopping when the user is satisfied instead of continuing. These are a large share of what `CLAUDE.md` files and system prompt appends try to shape.

A simulated user is a second model that plays the person: it holds a persona and hidden information, answers the agent's questions, and decides when the task is done.

## Requirements

1. A task may carry a conversation block:

   ```toml
   [conversation]
   persona = "A busy maintainer who prefers small diffs and answers questions in one line."
   hidden = """
   The failing test is the unicode case. You know this but only say it if asked which test fails.
   If asked whether to also fix the hyphen collapsing, say no.
   """
   max_user_turns = 6
   model = "claude-sonnet-5-5"       # optional; default is the judge model
   effort = "low"
   done_when = "The agent reports the tests pass and asks for nothing more."
   ```

   The task's `prompt` is the opening message. The simulator sees the persona, the hidden text, the dialog so far and the agent's visible tool calls (names only), and returns `{ reply, done, satisfied, revealed_hidden }` through the same schema-validated `claude -p --json-schema` path the judges use.

2. The Claude Code runner runs the dialog in one process using stream-json input: `--input-format stream-json` with stdin held open, writing each user turn as a message when the agent's turn ends (the `result` event). The run ends when the simulator says `done`, `max_user_turns` is reached, the agent's turn ends with no question and the simulator has nothing to add, or the timeout fires. The existing single-prompt path is unchanged; stdin stays closed there.

3. Codex reports `Support::None` for conversations in this round; the loader refuses the experiment with the reason.

4. The normalized transcript marks simulated user turns with `simulated: true`. New metrics: `user_turns`, `questions_asked` (assistant turns ending in a question, plus `AskUserQuestion` tool calls), `user_satisfied` (0 or 1 from the simulator's last reply), `hidden_revealed` (how many user turns the simulator flagged as revealing hidden information unprompted, a validity check).

5. Grading. Deterministic graders see the final workspace as today. The rubric judge and the pairwise judge see the whole dialog when the task has a conversation; the Review page shows the dialog with user turns styled as such, beside the diff. `transcript_regex` and `sequence` from item 1 work over the dialog.

6. Cost. Each simulator reply is a model call; it is recorded as `simulator_cost_usd`, counted in `budget_usd`, and shown in the report beside judge cost.

7. Reproducibility. The simulator's model, effort, persona and hidden text are recorded with the run. Two runs of the same cell differ by both agent and simulator randomness; `runs_per_cell` applies as usual and the report says the comparison includes simulator variance.

8. The example project gains `tasks/coding/fix-slugify-ambiguous.toml`: the prompt says only "something is broken in slugify, fix it," the hidden text names the test, and an overlay with "ask one question before changing code when the task is underspecified" is compared against none on `pass`, `questions_asked` and `cost_usd`.

## Where it lands

- `crates/wrangler-core/src/model.rs`: `Task.conversation`.
- `crates/wrangler-core/src/runner/claude.rs`: the multi-turn loop; `runner/mod.rs` `exec` grows a variant that keeps stdin open and hands turn boundaries to a callback.
- New `crates/wrangler-core/src/simulator.rs`: the user model call, reusing the judge's `claude -p` invocation.
- `crates/wrangler-core/src/transcript.rs`: `simulated` flag, `questions_asked`.
- `crates/wrangler-core/src/graders/judge.rs`, `review.rs`, `scheduler.rs` (`shown_for`): dialog as the shown output.
- `crates/wrangler-core/src/report.rs`: new metrics and the simulator cost column.
- `apps/desktop`: dialog view in Results and Review.
- `docs/formats.md`; `docs/isolation.md` gains what the stdin mode changes.

## Acceptance

- A mock simulator (scripted replies) drives the mock runner through three turns in a unit test; the transcript carries the `simulated` flag and `user_turns = 3`.
- A live test: the ambiguous slugify task under Claude Code ends with `done = true` from the simulator, the tests pass, and `questions_asked >= 1` when the overlay asks for a question.
- A live test that the run stops at `max_user_turns` and is graded as it stands, like a timeout.
- The seatbelt profile needs no new paths for stdin mode (verified by the live test under `isolation = "seatbelt"`).

## Out of scope

- A human in the loop during a run.
- Multi-session memory. One dialog is one run.

## Open questions

- Spike first: confirm `--input-format stream-json` with `--output-format stream-json` keeps a single session alive across user messages on Claude Code 2.1.x and that a turn end is detectable from the stream. Fallback: `--resume <session-id>` per user turn with session persistence on, writing sessions under a per-run `CLAUDE_CONFIG_DIR` is not possible (keychain), so sessions would land in `~/.claude/projects` and need cleanup after the run.
- How gullible the simulator is. `hidden_revealed` is the first measure; if it runs high, the simulator prompt needs the "only when asked specifically" rule tightened and a test corpus of its own.
- Whether the simulator should see tool call names at all. Showing them makes "ran the tests" visible to the user; hiding them is more realistic for a chat user and less so for someone watching a terminal. Make it a `conversation.sees_tools` flag, default true.
