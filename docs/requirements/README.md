# Requirements: the next round

Status: requirements settled, nothing built. Each file is a handoff in the style of [DESIGN.md](../../DESIGN.md).

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
