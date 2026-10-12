# 8. MCP factor

Value: medium-low. Difficulty: medium.

## Purpose

Some patterns give the agent capabilities rather than instructions: an MCP server, a tool allowlist, a settings file that gates tools. `--strict-mcp-config` hides every MCP server for isolation, and there is no way to put one back for a variant. The `knob` factor can already pass `--tools` or `--disallowedTools`, which is undocumented.

Fewer patterns need this than the items above, and the sandbox makes it the most involved of the small items.

## Requirements

1. A factor kind `mcp` whose levels are MCP config files relative to the project (`""` means none). The Claude Code runner passes `--mcp-config <abs path>` and keeps `--strict-mcp-config`, so exactly that config loads. Several `mcp` factors or levels in a table form stack: `{ value = "mcp/filesystem.json", label = "fs" }`.

2. Overlays may carry `.mcp.json` at their root. If the spike shows project-level `.mcp.json` loads under `--setting-sources project` with `--strict-mcp-config`, the overlay route is documented as the primary one and the factor kind is sugar over it; if not, the runner merges an overlay's `.mcp.json` into the `--mcp-config` list.

3. Codex: the level is translated into `[mcp_servers]` in the per-run `CODEX_HOME/config.toml` with `Support::Approximate` and a loader warning; stdio servers only.

4. Tool names from MCP servers normalize to `mcp.<server>.<tool>` in both runners (item 1 does this; this item depends on it). Metrics `mcp.<server>.<tool>` count calls; `tool_call` graders match them.

5. Sandbox. A stdio server spawns inside the run's seatbelt profile; the profile must allow reading the server's binary and its runtime (node, python, uv) and any paths the config names. The runner reads the config, resolves the `command` on PATH, and adds those paths read-only; writes stay confined to the workspace. HTTP servers on localhost are blocked by `network = "anthropic"`; a new form `network = { allow = ["anthropic", "localhost:8080"] }` extends the proxy's allowlist. Under Docker the server must exist in the image; the loader warns.

6. `docs/formats.md` documents the factor kind, the overlay `.mcp.json`, the `knob` route for `--tools` and `--disallowedTools`, and the network form. The example project gains `experiments/mcp-fs.toml`: the slugify task with and without a filesystem MCP server, graded on `pass`, `cost_usd` and `mcp.filesystem.*` usage, using a server installable with one `npx` command.

## Where it lands

- `crates/wrangler-core/src/model.rs`: `FactorKind::Mcp`, `Variant::mcp_configs()`, `NetworkPolicy::Allow`.
- `crates/wrangler-core/src/runner/claude.rs`: the flag; `codex.rs`: the config translation.
- `crates/wrangler-core/src/isolation.rs`: profile paths from the config; proxy allowlist.
- `crates/wrangler-core/src/workspace.rs`: overlay `.mcp.json` handling if needed.
- `docs/formats.md`, `docs/isolation.md`.

## Acceptance

- Unit test: a variant with an `mcp` level produces the `--mcp-config` argument with an absolute path and keeps `--strict-mcp-config`.
- Live test: the `init` event's `mcp_servers` lists exactly the configured server under seatbelt, the server answers one call, and `mcp.<server>.<tool>` is recorded. A second live test shows the server absent in the `""` level.
- Live test under `network = { allow = [...] }`: an HTTP server on localhost is reachable and `example.com` is still blocked.

## Open questions

- Whether `.mcp.json` in the workspace loads under `--setting-sources project`. Measure, as `docs/isolation.md` did for the other sources, and record the answer there.
- Whether the server's own writes (caches, logs) need profile paths. Collect `EPERM`s the same way the Claude Code paths were found.
