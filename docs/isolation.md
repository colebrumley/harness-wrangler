# Isolation: what is enforced and what was measured

Measured on 2026-10-11 with Claude Code 2.1.296 and codex-cli 0.162.0. Each claim below has a test in `crates/wrangler-core/tests/live_runners.rs`; rerun them after a runner upgrade.

## Workspace

Every run gets a fresh directory under the system temp dir (`$TMPDIR/wrangler-ws/`), never inside the project. The fixture is copied in with APFS clones (`cp -cR`), the overlay is materialized, and everything is committed as `wrangler-baseline`. The post-run diff is against that commit, so it shows only the agent's work.

Workspaces live outside the project on purpose: agent harnesses walk up parent directories for instruction files, and the project's own files must not leak into a run.

## Claude Code flags

```
claude -p --setting-sources project --strict-mcp-config --no-session-persistence \
  --permission-mode bypassPermissions --model <m> --effort <e> \
  --output-format stream-json --verbose -- "<prompt>"
```

| Question from DESIGN.md §16 | Answer | How it was checked |
|---|---|---|
| Does `--setting-sources project` hide user skills, plugins, `CLAUDE.md`? | Yes. | The run's `init` event lists only built-in plugins and no plugin-namespaced skills; the overlay skill is listed. |
| Does it suppress user-level hooks? | Yes. | A `UserPromptSubmit` and a `SessionStart` hook in a temporary `HOME` fire without the flag and do not fire with it. Hooks run before any API call, so this test needs no login and spends nothing. |
| Does it suppress MCP servers? | No. claude.ai connectors still load. | The spike's `init` event listed six connectors. `--strict-mcp-config` removes them; the test asserts `mcp_servers` is empty. |
| Is `--max-turns` honored? | Yes, though it is not in `--help`. | A run stopped with `error_max_turns`. Such runs are graded as they stand and counted with timeouts. |

Stdin is closed for every run; otherwise `claude -p` waits three seconds for piped input.

## Seatbelt

`isolation = "seatbelt"` wraps the run in `sandbox-exec` with writes denied everywhere except the workspace and the paths Claude Code itself needs.

Method: run under a profile that allows writes to the workspace only, with `--debug-file` inside the workspace, and collect every `EPERM`. The kernel's own sandbox denial log is not readable without elevated privileges on current macOS, so the runner's debug log is the source.

| Path | Why Claude Code writes there |
|---|---|
| `~/.claude/` | `shell-snapshots/`, `session-env/`, `sessions/`, per-project memory. Without `shell-snapshots` the Bash tool cannot start. |
| `~/.claude.json*` | Global state file with `.tmp.<pid>` and `.lock` siblings. |
| `~/.local/state/claude/` | Version lock files. |
| `/private/tmp/claude-<uid>/` | The Bash tool's scratchpad and fswatch probes. |

Auth works with no extra allowance: reading the keychain goes through a system service, not a file write. A harmless side effect: `ps` cannot be spawned inside the sandbox (it is setuid).

### Network

Seatbelt filters by address, not hostname. So `network = "anthropic"` denies all direct outbound traffic and points the run at a local CONNECT proxy that tunnels only to `anthropic.com`, `claude.ai` and `claude.com` (and subdomains). Blocked hosts are recorded as run events. In the live test Claude Code's own telemetry to Datadog was blocked and the run was unaffected; `curl https://example.com` from the agent was blocked.

`network = "full"` leaves the network alone.

## Docker

`isolation = "docker"` is optional; nothing else depends on Docker. The runner CLI runs inside `docker run --rm` from `[runner] docker_image`, with only the workspace mounted, at the same path as on the host. Fixture copy, overlay, baseline commit, diff and graders all stay on the host. The container is removed after the run even if the client was killed.

A subscription login lives in the macOS keychain and does not exist in a container, so the runner must authenticate from an environment variable. `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN` and `OPENAI_API_KEY` are passed by name when set; `docker_env` replaces that list. Values are never written to the recorded command line. `network = "anthropic"` is not applied here; use `docker_args = ["--network", ...]`.

Tested with a stand-in runner in `alpine`: it ran on Linux in the mounted workspace, could not see host paths outside it, and left no container behind. It has not been run with a real Claude Code image, because this machine has no API key.

## Budget

`budget_usd` caps one launch's agent plus judge cost. The cap is checked before each run starts, so the overshoot is at most the runs already in flight. A launch that reaches it stops with status `budget`: finished runs are kept, the judge phase does not run, and `wrangler resume <launch> --budget <usd>` continues. Runners that report no dollar cost (Codex) do not count toward it.

## Codex

Each run gets its own `CODEX_HOME` holding only a symlink to your `auth.json`, so your `~/.codex/AGENTS.md`, config and skills stay out. It is deleted after the run.

Codex does not read Claude Code's layout, so the overlay is translated: `CLAUDE.md` becomes `AGENTS.md`, `.claude/skills/*` are installed as Codex skills, and a system prompt append is added to `AGENTS.md`. Settings, hooks and agent definitions in an overlay are ignored. The experiment loader warns about each of these.

Codex applies its own seatbelt sandbox to model-run commands, and a nested `sandbox-exec` does not work, so wrangler uses Codex's `--sandbox workspace-write` at every isolation level. `[runner] codex_sandbox` overrides the mode.

## Judges

Judges run through Claude Code (`claude -p --tools "" --json-schema ...`) in an empty temp directory with the same isolation flags. There was no API key on the build machine, and this route keeps judging on the subscription while still returning schema-validated structured output. Judge model, effort and rubric are recorded with every grade.

## Rate limits

A rejected run (HTTP 429 or 529, or a usage-limit message) is retried with exponential backoff starting at 30 seconds, up to `max_retries`. Each rejection and retry is stored as a run event and counted in the report. The `wall_time_s` metric is the agent's own time and excludes backoff waits.
