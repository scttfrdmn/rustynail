# CLI Reference

RustyNail is a single binary with 7 subcommands. Running with no subcommand is equivalent to `rustynail start`.

```
USAGE:
    rustynail [SUBCOMMAND]

SUBCOMMANDS:
    start                  Start the gateway (default)
    status [--port N]      Query a running instance
    version                Print version and build info
    config check           Load config and print summary
    config validate        Preflight checks; exits 0/1
    completions <shell>    Print shell completion script
    mcp serve              Expose tools as MCP server (stdio)
```

---

## `rustynail start`

Start the RustyNail gateway. This is the default when no subcommand is given.

```bash
rustynail start
# equivalent to:
rustynail
```

**Startup sequence:**

1. Load configuration (`CONFIG_FILE` → env vars → defaults)
2. Initialise structured logging (with optional OTel exporter)
3. Create the gateway and register configured channels
4. Start the HTTP server on `gateway.http_port` (default 8080)
5. Start the WebSocket server on `gateway.websocket_port` (default 18789)
6. Register SIGHUP handler for hot-reload (Unix only)
7. Block until Ctrl-C or SIGTERM
8. Graceful shutdown with `gateway.shutdown_timeout_seconds` timeout

**Environment variables read at startup:**

All variables listed in the [env var cheat sheet](deployment.md#environment-variables-cheat-sheet).

**SIGHUP behaviour:**

Sending SIGHUP to the running process reloads hot-reloadable config fields without restart. See [deployment.md — SIGHUP Hot-Reload](deployment.md#sighup-hot-reload).

**Graceful shutdown:**

On Ctrl-C or SIGTERM, the gateway stops accepting new messages and waits up to `shutdown_timeout_seconds` for in-flight messages to complete. A warning is logged if the timeout is exceeded.

---

## `rustynail status [--port N]`

Query a running RustyNail instance over HTTP and print the JSON status response.

```bash
rustynail status
rustynail status --port 9090
```

**Options:**

| Flag | Default | Description |
|------|---------|-------------|
| `--port N` | `8080` | HTTP port of the running instance |

**Exit codes:**

| Code | Meaning |
|------|---------|
| `0` | Connected successfully; status printed |
| `1` | Could not connect to the instance |

**Output:** Formatted JSON from `GET /status`. Example:

```json
{
  "status": "running",
  "version": "0.15.0",
  "uptime_seconds": 3600,
  "channels": [
    {"name": "discord-main", "status": "healthy"}
  ],
  "active_users": 5
}
```

---

## `rustynail version`

Print version and build information.

```bash
rustynail version
```

**Output:**

```
rustynail 0.15.0
repository: https://github.com/scttfrdmn/rustynail
license:    Apache-2.0
```

---

## `rustynail config check`

Load the configuration and print a human-readable summary. Does not start the server.

```bash
rustynail config check
CONFIG_FILE=production.yaml rustynail config check
```

**What it prints:**

```
Configuration OK
  HTTP port:        8080
  WebSocket port:   18789
  Log level:        info
  LLM provider:     anthropic
  LLM model:        claude-3-5-sonnet-20241022
  Memory backend:   redis
  Tools enabled:    true
  Summarization:    enabled (trigger_at=40, keep_recent=10)
  OTel endpoint:    http://otel-collector:4317
  Dashboard auth:   enabled
  Channels:         discord, slack (webhook), telegram (long-poll)
  Gateway auth:     enabled
  Skills:           enabled (2 paths, 4 skills loaded)
  WS origins:       https://myapp.example.com
  Shutdown timeout: 30s
  Cron jobs:        2
  PDF tool:         enabled
  Image tool:       disabled
  Quarry:           enabled (every run refused: no verifier — see config validate)
```

The `Quarry:` line reports the **effect on a run**, not the value of a setting —
"verified" and "every run refused" are both `verification.enabled: true`, and a line
that echoed the setting would print the same word for both.

**No side effects** — this command only reads config and exits. Safe to run in CI.

---

## `rustynail config validate`

Run preflight validation checks and exit with a clear pass/fail report.

```bash
rustynail config validate
CONFIG_FILE=production.yaml rustynail config validate
echo $?   # 0 = all passed, 1 = one or more failed
```

**Checks performed:**

| Check | Pass condition | Output |
|-------|---------------|--------|
| Config loads | YAML parses without error | `[✓] Config loaded (config.yaml)` |
| API key present | `agents.api_key` is non-empty | `[✓] API key present (anthropic)` |
| Redis URL set | When `memory.backend = "redis"` | `[✓] Memory backend: redis (url set)` |
| SQLite path set | When `memory.backend = "sqlite"` | `[✓] Memory backend: sqlite (path set)` |
| Postgres URL set | When `memory.backend = "postgres"` | `[✓] Memory backend: postgres (url set)` |
| Quarry verification | Quarry is off, verification is deliberately off, or a verifier is installed with an identity configured | `[✓] Quarry: disabled (…)` |

**`[!]` is a warning, not a failure.** It marks a state an operator chose
deliberately — `verification.enabled: false`, or `allow_writable_binary` — that is
worth restating but does not change the exit code. Three of the four quarry states
are neither pass nor fail, so a bare `[✓]`/`[✗]` pair could not report them.

**Exit codes:**

| Code | Meaning |
|------|---------|
| `0` | All checks passed |
| `1` | One or more checks failed |

**Example output (all pass):**

```
[✓] Config loaded (config.yaml)
[✓] API key present (anthropic)
[✓] Memory backend: redis (url set)
[✓] Quarry: disabled (no binary is spawned, nothing to verify)
[✓] All checks passed.
```

**Example output (failure):**

```
[✓] Config loaded (env vars)
[✗] API key missing (agents.api_key / ANTHROPIC_API_KEY)
[✗] Memory backend is sqlite but memory.sqlite_path is not set

2 check(s) failed.
```

**Example output (quarry on, verification unsatisfiable):**

```
[✓] Config loaded (config.yaml)
[✓] API key present (anthropic)
[✓] Memory backend: inmemory
[✗] Quarry: verification is enabled but no verifier is installed
      Every quarry run will be REFUSED (mechanism_unavailable).
      The cosign mechanism is tracked as #103 and is not implemented
      yet. Until it lands, either leave quarry disabled or set
      quarry.verification.enabled: false to run unverified.

1 check(s) failed.
```

This is the **shipped default** when `quarry.enabled: true` — the cosign mechanism
([#103](https://github.com/scttfrdmn/rustynail/issues/103)) is not implemented, and
verification fails closed rather than waving runs through. See
[deployment.md](deployment.md#deploying-quarry-alongside-the-gateway).

**Example output (quarry on, verification deliberately off):**

```
[!] Quarry: signature verification DISABLED (quarry.verification.enabled = false)
      Runs will execute unverified. The capability manifest is still
      checked, but from an unsigned sidecar that proves nothing about
      provenance. Development only — do not ship this.
[✓] All checks passed.
```

Useful in CI/CD pipelines before deploying: add `rustynail config validate` as a pre-deploy step.

---

## `rustynail completions <shell>`

Print a shell completion script for the given shell to stdout.

```bash
rustynail completions bash
rustynail completions zsh
rustynail completions fish
rustynail completions powershell
rustynail completions elvish
```

**Supported shells:** `bash`, `zsh`, `fish`, `powershell`, `elvish`

**Installation:**

```bash
# Bash
rustynail completions bash >> ~/.bash_completion
# or
rustynail completions bash > /etc/bash_completion.d/rustynail

# Zsh
rustynail completions zsh > "${fpath[1]}/_rustynail"
# or add to ~/.zshrc:
source <(rustynail completions zsh)

# Fish
rustynail completions fish > ~/.config/fish/completions/rustynail.fish

# PowerShell (add to $PROFILE)
rustynail completions powershell | Out-String | Invoke-Expression
```

---

## `rustynail mcp serve`

Expose RustyNail's registered tools as an MCP (Model Context Protocol) server over stdio.

```bash
rustynail mcp serve
```

Pipe this into Claude Code or any MCP-compatible client:

```bash
# In Claude Code's settings (claude_desktop_config.json):
{
  "mcpServers": {
    "rustynail": {
      "command": "/usr/local/bin/rustynail",
      "args": ["mcp", "serve"]
    }
  }
}
```

**Tools exposed** (depends on config):

- `calculator` — always available
- `formatter` — always available
- `filesystem` — when `tools.enabled = true` and `tools.filesystem_root` is set
- `web_search` — when `tools.enabled = true` and `tools.web_search_api_key` is set
- `calendar` — when `tools.enabled = true`

**Transport:** stdio (stdin/stdout JSON-RPC). Log output is written to stderr to keep the transport clean.

**Notes:**

- The server reads config from `CONFIG_FILE` / env vars at startup.
- Tools are registered based on the current config — no `tools.enabled = false` tools will appear.
- Compatible with Claude Code, Cursor, and any MCP-compliant client.
