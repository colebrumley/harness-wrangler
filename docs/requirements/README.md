# Requirements: the next round

Status: all eight built, 2026-10-11. Each file is a handoff in the style of [DESIGN.md](../../DESIGN.md); "What was built" below records where each landed, what the spikes found, and where the build departs from the text.

The first round measures one shape of question: does a block of instruction text change the final output of a single-prompt run, and which sections carry the change. The items below widen that to triggering, process, interaction, tools, and sturdier verdicts.

Order is by value first, then by difficulty within the same value. Value is how many new kinds of pattern become measurable, or how many existing verdicts become trustworthy. Difficulty is the change to this repo as it stands at commit `19cffa1`.

| # | Requirement | Value | Difficulty | Opens up |
|---|---|---|---|---|
| 1 | [Transcript graders](01-transcript-graders.md) | high | low | Process rules in `CLAUDE.md`, hooks, tool discipline |
| 2 | [Trigger corpus](02-trigger-corpus.md) | high | low | Skill descriptions, false positives, routing |
| 3 | [Task harvesting](03-task-harvesting.md) | high | medium-high | A task set wide enough to evaluate anything beyond writing |
| 4 | [Simulated-user runner](04-simulated-user-runner.md) | high | high | Clarifying questions, plan mode, corrections, follow-ups |
| 5 | [Headroom check](05-headroom-check.md) | medium | low | Stops "retire" verdicts on saturated tasks; cheaper launches |
| 6 | [Placebo overlays](06-placebo-overlays.md) | medium | low | Separates "any instructions" from "these instructions" |
| 7 | [Judge calibration](07-judge-calibration.md) | medium | low-medium | Says when the judge can be trusted per task category |
| 8 | [MCP factor](08-mcp-factor.md) | medium-low | medium | MCP servers, tool sets, settings that gate tools |

Items 1, 5 and 6 improve launches that already exist. Item 4 is the one that moves the tool into a new class of patterns. Each file ends with acceptance tests and open questions; a spike listed under open questions is part of the work, not optional.

## What was built

| # | Where to read about it | Spikes and open questions | Departures from the text |
|---|---|---|---|
| 1 | [formats.md](../formats.md), "Deterministic graders" | Hooks are in the stream with `--include-hook-events` (`system` / `hook_response`, carrying event, matcher, exit code, output), so no wrapper was needed. `input` stays a regex over the serialized JSON. | In `tool_call`, `name` is the tool, so the grader's own name is `label`. Each deterministic check is also a metric, `check.<name>`, which is how `tests-run-first` shows on its own. On Haiku it is tied at 100% with and without the terse overlay. |
| 2 | [formats.md](../formats.md), "Trigger experiments" | A skill loaded by description match shows up as a `Skill` tool call (the existing live test asserts it). | The example `rfw-trigger` has one overlay level, since a baseline without the skill cannot fire. Reaching a task's own `max_turns` is a normal end, not a timeout. Live on Haiku and Sonnet the skill fired on about one should-fire prompt in six and on no near miss. |
| 3 | [harvesting.md](../harvesting.md) | The session file does not record `HEAD`. The commit is a hash printed by the session's own git commands, else the last commit before the session's first timestamp, and the import says which. | `task check` runs a one-variant experiment made from the task, which is the baseline-only path with nothing left to resume. |
| 4 | [formats.md](../formats.md), "Conversation tasks"; [isolation.md](../isolation.md) | One process holds one session across stdin messages; a `result` event ends each turn; cost is a running total and tokens are per turn. No `--resume` fallback. `sees_tools` exists, default true. | `questions_asked` counts a message that hands the turn back with a question in its closing paragraph, since agents often follow the question with a line of context. In the live `ask-first` run the soft "when underspecified" rule rarely made Haiku ask; the live test uses a firm rule. |
| 5 | README, "How results are computed" | Judge thresholds are 9 and 2, as given. | A task is left out of a comparison when both arms sit at the same bound, not when the baseline alone does: a pattern that breaks a passing task is an effect and must stay visible. |
| 6 | [formats.md](../formats.md), "Placebo overlays" | `wrangler placebo <file>` covers `system_prompt_append` and `CLAUDE.md`. | The placebo variant carries only `SKILL.md`, not the skill's reference files. |
| 7 | [formats.md](../formats.md), "Judge calibration" | "Both bad" is excluded from agreement and counted on its own. | A suggestion about named tasks is held to those tasks' tags; one about the whole launch, to the overall figure. A judge with no pairwise votes is held to how its rubric scores track human picks. |
| 8 | [formats.md](../formats.md), "Tools and MCP servers"; [isolation.md](../isolation.md) | A workspace `.mcp.json` does not load under `--strict-mcp-config`, so the runner passes an overlay's file with `--mcp-config`. `npx` needs `~/.npm` writable and the npm registry reachable. | The profile does not restrict reads, so no read paths are added; `localhost:<port>` entries are reached directly, not through the proxy. |

