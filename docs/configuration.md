# Configuration Reference

RustyNail is configured through a combination of a YAML file and environment variables.

## Loading Order

Settings are resolved in this priority order (highest wins):

1. **Environment variables** — always override everything else
2. **`CONFIG_FILE`** — path to a YAML config file (e.g. `CONFIG_FILE=config.yaml`)
3. **Built-in defaults** — sensible values are applied when nothing is set

Only `ANTHROPIC_API_KEY` (or the equivalent for your chosen provider) is required. All other settings have defaults.

## `gateway.*`

Controls the HTTP server, security, and message pipeline behaviour.

| Field | Type | Env var | Default | Description |
|-------|------|---------|---------|-------------|
| `http_port` | `u16` | — | `8080` | HTTP server port |
| `websocket_port` | `u16` | — | `18789` | WebSocket server port (dashboard + webchat) |
| `log_level` | `String` | `RUST_LOG` | `"info"` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `api_token` | `Option<String>` | `GATEWAY_API_TOKEN` | `None` | Bearer token for API auth. Empty = auth disabled |
| `max_body_bytes` | `usize` | `GATEWAY_MAX_BODY_BYTES` | `1048576` (1 MiB) | Maximum request body size. Returns 413 when exceeded |
| `request_timeout_seconds` | `u64` | `GATEWAY_REQUEST_TIMEOUT_SECONDS` | `30` | Handler timeout in seconds. Returns 408 when exceeded |
| `allowed_ws_origins` | `Vec<String>` | `GATEWAY_ALLOWED_WS_ORIGINS` (comma-sep) | `[]` (allow all) | Allowed WebSocket upgrade origins. Empty = allow all |
| `shutdown_timeout_seconds` | `u64` | `GATEWAY_SHUTDOWN_TIMEOUT_SECONDS` | `30` | Graceful shutdown timeout in seconds |
| `chunking_enabled` | `bool` | `GATEWAY_CHUNKING_ENABLED` | `false` | Split long responses at platform character limits |
| `chunking_limits` | `HashMap<String,usize>` | — (file only) | built-in per-platform | Per-channel-id character limits; built-ins: `discord`→2000, `slack`→4000, `teams`→1024, `telegram`→4096, `whatsapp`→4096 |
| `formatting_enabled` | `bool` | `GATEWAY_FORMATTING_ENABLED` | `true` | Convert markdown to platform-native syntax |
| `auto_route_attachments` | `bool` | `GATEWAY_AUTO_ROUTE_ATTACHMENTS` | `false` | Prepend PDF/image context to agent prompt |
| `deduplication.enabled` | `bool` | `GATEWAY_DEDUP_ENABLED` | `false` | Drop duplicate `(user_id, content)` pairs |
| `deduplication.window_size` | `usize` | `GATEWAY_DEDUP_WINDOW_SIZE` | `256` | Ring buffer size for deduplication hashes |

### `gateway.rate_limit.*`

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `enabled` | `RATE_LIMIT_ENABLED` | `false` | Enable per-user sliding-window rate limiting |
| `messages_per_window` | `RATE_LIMIT_MESSAGES` | `20` | Maximum messages allowed per window |
| `window_seconds` | `RATE_LIMIT_WINDOW_SECONDS` | `60` | Window size in seconds |

**Example:**

```yaml
gateway:
  http_port: 8080
  websocket_port: 18789
  log_level: info
  api_token: ${GATEWAY_API_TOKEN}
  max_body_bytes: 2097152     # 2 MiB
  request_timeout_seconds: 30
  shutdown_timeout_seconds: 30
  allowed_ws_origins:
    - https://myapp.example.com
  chunking_enabled: true
  formatting_enabled: true
  auto_route_attachments: true
  deduplication:
    enabled: true
    window_size: 512
  rate_limit:
    enabled: true
    messages_per_window: 20
    window_seconds: 60
```

## `channels.*`

Each channel is optional. Absent or `enabled: false` means the channel is inactive.

### Discord

```yaml
channels:
  discord:
    enabled: true
    auth:
      token: ${DISCORD_BOT_TOKEN}
```

| Field | Env var | Required | Description |
|-------|---------|----------|-------------|
| `enabled` | — | — | Activate this channel |
| `auth.token` | `DISCORD_BOT_TOKEN` | yes | Bot token from Discord Developer Portal |

### WhatsApp

```yaml
channels:
  whatsapp:
    enabled: true
    phone_number_id: "1234567890"
    access_token: ${WHATSAPP_ACCESS_TOKEN}
    verify_token: ${WHATSAPP_VERIFY_TOKEN}
```

| Field | Env var | Required | Description |
|-------|---------|----------|-------------|
| `phone_number_id` | `WHATSAPP_PHONE_NUMBER_ID` | yes | Meta Cloud API phone number ID |
| `access_token` | `WHATSAPP_ACCESS_TOKEN` | yes | Meta System User access token |
| `verify_token` | `WHATSAPP_VERIFY_TOKEN` | yes | Webhook verification token |

Webhook URL: `POST /webhooks/whatsapp`

### Telegram

```yaml
channels:
  telegram:
    enabled: true
    bot_token: ${TELEGRAM_BOT_TOKEN}
    webhook_secret: ${TELEGRAM_WEBHOOK_SECRET}
    mode: webhook   # or: longpoll
```

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `bot_token` | `TELEGRAM_BOT_TOKEN` | — | BotFather token |
| `webhook_secret` | `TELEGRAM_WEBHOOK_SECRET` | — | Secret token sent in `X-Telegram-Bot-Api-Secret-Token` header |
| `mode` | `TELEGRAM_MODE` | `"webhook"` | `"webhook"` or `"longpoll"` |

Webhook URL (webhook mode): `POST /webhooks/telegram`

### Slack

```yaml
channels:
  slack:
    enabled: true
    bot_token: ${SLACK_BOT_TOKEN}
    signing_secret: ${SLACK_SIGNING_SECRET}
    mode: webhook   # or: socket
    app_token: ${SLACK_APP_TOKEN}  # required for socket mode
```

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `bot_token` | `SLACK_BOT_TOKEN` | — | Bot token (`xoxb-`) |
| `signing_secret` | `SLACK_SIGNING_SECRET` | — | Used to verify HMAC-SHA256 request signatures |
| `mode` | `SLACK_MODE` | `"webhook"` | `"webhook"` or `"socket"` |
| `app_token` | `SLACK_APP_TOKEN` | — | App-level token (`xapp-`), required for socket mode |

Webhook URL (webhook mode): `POST /webhooks/slack`

### SMS / Twilio

```yaml
channels:
  sms:
    enabled: true
    auth:
      account_sid: ${TWILIO_ACCOUNT_SID}
      auth_token: ${TWILIO_AUTH_TOKEN}
      from_number: "+15551234567"
```

Webhook URL: `POST /webhooks/sms`

### Microsoft Teams

```yaml
channels:
  teams:
    enabled: true
    auth:
      app_id: ${TEAMS_APP_ID}
      app_password: ${TEAMS_APP_PASSWORD}
      hmac_secret: ${TEAMS_HMAC_SECRET}   # optional
```

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `auth.app_id` | `TEAMS_APP_ID` | — | Bot Framework app registration ID |
| `auth.app_password` | `TEAMS_APP_PASSWORD` | — | Bot Framework app password |
| `auth.hmac_secret` | `TEAMS_HMAC_SECRET` | `""` | Optional HMAC-SHA256 validation secret. Empty = skip |

Webhook URL: `POST /channels/teams/messages`

### Email

```yaml
channels:
  email:
    enabled: true
    imap:
      host: imap.gmail.com
      port: 993
      username: ${EMAIL_USERNAME}
      password: ${EMAIL_PASSWORD}
      inbox: INBOX
    smtp:
      host: smtp.gmail.com
      port: 587
      username: ${EMAIL_USERNAME}
      password: ${EMAIL_PASSWORD}
      from_address: bot@example.com
```

| Field | Env var | Default |
|-------|---------|---------|
| `imap.host` | `EMAIL_IMAP_HOST` | — |
| `imap.port` | `EMAIL_IMAP_PORT` | `993` |
| `imap.username` | `EMAIL_USERNAME` | — |
| `imap.password` | `EMAIL_PASSWORD` | — |
| `imap.inbox` | `EMAIL_INBOX` | `"INBOX"` |
| `smtp.host` | `EMAIL_SMTP_HOST` | — |
| `smtp.port` | `EMAIL_SMTP_PORT` | `587` |
| `smtp.from_address` | `EMAIL_FROM_ADDRESS` | — |

### Webchat

```yaml
channels:
  webchat:
    enabled: true
    allowed_origins:
      - https://myapp.example.com
    welcome_message: "Hello! How can I help?"
```

| Field | Env var | Default |
|-------|---------|---------|
| `allowed_origins` | `WEBCHAT_ALLOWED_ORIGINS` (comma-sep) | `[]` |
| `welcome_message` | `WEBCHAT_WELCOME_MESSAGE` | — |

WebSocket: `GET /channels/webchat/ws?session_id=<uuid>`
Widget: `GET /channels/webchat/widget.js`

### Generic Webhook

```yaml
channels:
  webhook:
    enabled: true
    endpoints:
      - path: my-system
        user_id: webhook-user
        secret: ${MY_WEBHOOK_SECRET}
        extract_text: "$.message.text"
```

| Field | Required | Description |
|-------|----------|-------------|
| `path` | yes | Path segment → `POST /webhooks/<path>` |
| `user_id` | yes | User ID for all messages from this endpoint |
| `secret` | no | HMAC-SHA256 verification secret |
| `extract_text` | no | JSONPath to extract message text. Falls back to full body |

### Test Channel

```yaml
channels:
  test_channel: true
```

Enables `POST /test/send` (inject a message) and `GET /test/responses` (drain pending responses). Used by the zero-credential test harness. Never enable in production.

## `agents.*`

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `llm_provider` | `LLM_PROVIDER` | `"anthropic"` | Provider: `anthropic`, `openai`, `ollama`, `gemini`, `bedrock`, `litellm`, `openai-compat`, `stub` |
| `llm_model` | — | `"claude-3-5-sonnet-20241022"` | Model name for the primary provider |
| `api_key` | `ANTHROPIC_API_KEY` | — | API key for the primary provider |
| `api_base` | `ANTHROPIC_API_BASE` | — | Override API base URL (e.g. for a proxy) |
| `max_history` | — | `20` | Conversation history window per user |
| `temperature` | — | `0.7` | LLM temperature |
| `aws_region` | `AWS_REGION` | `"us-east-1"` | AWS region (Bedrock only) |
| `planning_enabled` | `AGENTS_PLANNING_ENABLED` | `false` | Enable `/plan <task>` routing to planning agent |
| `retry.enabled` | `AGENTS_RETRY_ENABLED` | `true` | LLM retry with exponential backoff |
| `retry.max_attempts` | `AGENTS_RETRY_MAX_ATTEMPTS` | `3` | Maximum attempts including the first |
| `retry.base_delay_ms` | `AGENTS_RETRY_BASE_DELAY_MS` | `100` | Base delay in ms for first retry |
| `retry.jitter_enabled` | `AGENT_RETRY_JITTER_ENABLED` | `false` | Apply ±20% jitter to backoff delays |

### Provider quick-reference

| `llm_provider` | Required env var | Notes |
|----------------|-----------------|-------|
| `anthropic` | `ANTHROPIC_API_KEY` | Default |
| `openai` | `OPENAI_API_KEY` | |
| `ollama` | — | Set `api_base` to Ollama URL |
| `gemini` | `GEMINI_API_KEY` | |
| `bedrock` | AWS credentials | Set `aws_region` |
| `litellm` | provider-specific | Set `api_base` to LiteLLM proxy URL |
| `openai-compat` | provider-specific | Set `api_base` to compatible endpoint |
| `stub` | — | Echo mode; for tests only |

### Fallback providers (file-only)

```yaml
agents:
  llm_provider: anthropic
  api_key: ${ANTHROPIC_API_KEY}
  fallback_providers:
    - provider: openai
      model: gpt-4o
      api_key: ${OPENAI_API_KEY}
    - provider: ollama
      model: llama3
      api_base: http://localhost:11434
      api_key: ""
```

Fallbacks are tried in order on capacity/overload errors (HTTP 500, 503, "overloaded", "model not found"). Rate-limit errors (429) are not forwarded.

## `memory.*`

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `backend` | `MEMORY_BACKEND` | `"inmemory"` | `inmemory`, `redis`, `sqlite`, `postgres`, `vector` |
| `redis_url` | `REDIS_URL` | — | Required when `backend = "redis"` |
| `redis_ttl_seconds` | `REDIS_TTL_SECONDS` | `0` (no expiry) | Redis key TTL |
| `sqlite_path` | `SQLITE_PATH` | — | Required when `backend = "sqlite"` |
| `postgres_url` | `DATABASE_URL` | — | Required when `backend = "postgres"` |
| `vector_store` | — | `"memory"` | `"memory"` or `"qdrant"` |
| `vector_store_url` | — | — | Qdrant URL (when `vector_store = "qdrant"`) |
| `embedding_provider` | — | `"simple"` | Embedding provider (`"simple"` = deterministic n-gram) |
| `embedding_model` | — | `""` | Model name (provider-specific; ignored for `"simple"`) |
| `vector_decay_half_life_seconds` | `VECTOR_DECAY_HALF_LIFE_SECONDS` | `3600.0` | Half-life for temporal decay scoring in vector memory |

### Redis

```yaml
memory:
  backend: redis
  redis_url: redis://localhost:6379
  redis_ttl_seconds: 86400   # 24 hours
```

### SQLite

```yaml
memory:
  backend: sqlite
  sqlite_path: /data/rustynail.db
```

### Postgres

```yaml
memory:
  backend: postgres
  postgres_url: postgresql://user:pass@localhost:5432/rustynail
```

### Vector (in-process + Qdrant)

```yaml
memory:
  backend: vector
  vector_store: qdrant
  vector_store_url: http://localhost:6333
  vector_decay_half_life_seconds: 1800.0   # 30 min
```

### Summarization

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `summarization.enabled` | `SUMMARIZATION_ENABLED` | `false` | Enable conversation summarization |
| `summarization.trigger_at` | `SUMMARIZATION_TRIGGER_AT` | `40` | Summarize when history exceeds N messages |
| `summarization.keep_recent` | `SUMMARIZATION_KEEP_RECENT` | `10` | Messages to keep after summarization |
| `summarization.model` | `SUMMARIZATION_MODEL` | `"claude-3-haiku-20240307"` | LLM for generating summaries |
| `summarization.trigger_token_budget` | `SUMMARIZATION_TRIGGER_TOKEN_BUDGET` | `0` (disabled) | Also summarize when estimated tokens exceed this |

```yaml
memory:
  backend: redis
  redis_url: redis://localhost:6379
  summarization:
    enabled: true
    trigger_at: 40
    keep_recent: 10
    model: claude-3-haiku-20240307
    trigger_token_budget: 8000
```

## `tools.*`

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `enabled` | — | `false` | Master switch for the tool registry |
| `max_steps` | — | `5` | Maximum ReAct agent steps per message |
| `filesystem_root` | — | — | Sandbox root for filesystem tool |
| `web_search_api_key` | `TAVILY_API_KEY` | — | Tavily API key for web search tool |
| `pdf_enabled` | `TOOLS_PDF_ENABLED` | `false` | Enable PDF analysis tool |
| `image_enabled` | `TOOLS_IMAGE_ENABLED` | `false` | Enable image analysis tool |
| `shell.enabled` | — | `false` | Enable shell execution tool |
| `shell.require_approval` | — | `true` | Require `approved=true` before executing |
| `shell.allowed_commands` | — | `[]` (any) | Allowlist of permitted commands, matched token-wise |

```yaml
tools:
  enabled: true
  max_steps: 5
  filesystem_root: /data/files
  web_search_api_key: ${TAVILY_API_KEY}
  pdf_enabled: true
  image_enabled: true
  shell:
    enabled: true
    require_approval: true
    allowed_commands:
      - "echo"
      - "ls"
      - "git status"
```

### How `shell.allowed_commands` is enforced

Entries are matched **token-wise against the parsed argv**, not as raw string
prefixes. An entry constrains the program and any leading subcommands it names,
leaving later arguments free:

| Entry | Permits | Rejects |
|---|---|---|
| `git` | `git status`, `git log --oneline` | `gitleaks detect` |
| `git status` | `git status --short` | `git push` |

A **non-empty allowlist also changes how commands run**: the command is exec'd
directly instead of being passed to `sh -c`, so shell features are unavailable —
pipes, redirection, command substitution, globbing, and `&&`/`;` chaining. Any
command containing `; | & $ ` > <` or a newline is rejected with an error.
Quotes still group arguments containing spaces (`git commit -m 'two words'`).

With an **empty** allowlist the command is passed to `sh -c` unchanged and full
shell semantics apply. That is the permissive default; it grants arbitrary
execution to whatever can call the tool, so pair it with `require_approval: true`.

> **Note:** prior to v0.15.0 the allowlist prefix-matched the raw command string
> and then ran it through `sh -c`, so an entry of `git` also permitted
> `git status; rm -rf ~`. If you relied on shell syntax inside an allowlisted
> command, it will now be rejected — split it into separate tool calls, or clear
> the allowlist to opt back into full shell semantics.

## `skills.*`

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `enabled` | `SKILLS_ENABLED` | `false` | Enable the skills system |
| `paths` | `SKILLS_PATHS` (colon-sep) | `["skills/"]` | Directories to search for `SKILL.md` files |
| `max_active` | `SKILLS_MAX_ACTIVE` | `3` | Maximum skills injected per agent system prompt |

A skill is a directory containing a `SKILL.md` file. The file content is injected verbatim into the agent's system prompt. Example: `skills/my-skill/SKILL.md`.

```yaml
skills:
  enabled: true
  paths:
    - skills/
    - /etc/rustynail/skills/
  max_active: 5
```

## `audit.*`

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `enabled` | `AUDIT_ENABLED` | `false` | Enable structured audit logging |
| `path` | `AUDIT_PATH` | `""` (stderr) | File path for NDJSON audit log |

**AuditEvent variants** (all written as NDJSON):

| Variant | Fields | Trigger |
|---------|--------|---------|
| `auth_rejected` | `endpoint`, `reason` | Bearer token mismatch |
| `rate_limit_hit` | `user_id`, `channel_id` | User exceeds rate limit |
| `message_received` | `user_id`, `channel_id`, `content_len` | Every inbound message |
| `tool_executed` | `tool_name`, `user_id`, `success` | After each tool call |
| `config_reloaded` | `changed_fields` | SIGHUP reload |
| `agent_created` | `user_id` | New per-user agent instantiated |
| `llm_error` | `user_id`, `error`, `attempt` | LLM call fails |
| `AdminAction` | `endpoint`, `param`, `success` | Admin API call |

```yaml
audit:
  enabled: true
  path: /var/log/rustynail/audit.ndjson
```

## `cron.*`

```yaml
cron:
  jobs:
    - name: daily-digest
      schedule: 24h
      message: "Generate a daily summary of recent activity."
      channel_id: discord-main
      user_id: cron-digest
      enabled: true
```

| Field | Required | Description |
|-------|----------|-------------|
| `name` | yes | Human-readable label for logs |
| `schedule` | yes | Interval with suffix: `30s`, `5m`, `2h`, `1d` |
| `message` | yes | Message text injected on each tick |
| `channel_id` | yes | Channel to route the synthetic message to |
| `user_id` | yes | User ID for the synthetic message |
| `enabled` | no (default true) | Whether this job is active |

View job statuses: `GET /cron/jobs`

## `mcp.*`

```yaml
mcp:
  servers:
    - name: my-tools
      transport: stdio
      command: /usr/local/bin/my-mcp-server
      args: ["--config", "/etc/mcp.yaml"]
      env:
        - ["MY_SECRET", "${MY_SECRET}"]
    - name: remote-tools
      transport: http
      url: http://mcp-server:8090
```

| Field | Default | Description |
|-------|---------|-------------|
| `transport` | `"stdio"` | `"stdio"` (subprocess) or `"http"` |
| `command` | — | Command to spawn (stdio only) |
| `args` | `[]` | Arguments for the subprocess |
| `env` | `[]` | Extra env vars as `[["KEY","VALUE"],...]` |
| `url` | — | Base URL (http only) |

Misconfigured or unreachable servers are logged and skipped at startup.

## `quarry.*`

Hosts [quarry](https://github.com/scttfrdmn/quarry) — bounded recursive
decomposition with verified provenance — as a gateway capability. quarry is
spawned as a **subprocess** per run; the gateway reads its `RunEvent` stream from
stdout and its citable record from disk.

```yaml
quarry:
  enabled: false
  binary_path: quarry
  max_concurrent_runs: 2
  run_record_dir: quarry-runs
  retention_max_runs: 50
  retention_max_age_seconds: 0
  run_timeout_seconds: 900
```

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `enabled` | `QUARRY_ENABLED` | `false` | Off by default: **a quarry run spends real money** |
| `binary_path` | `QUARRY_BINARY_PATH` | `"quarry"` | Path to the `quarry` binary, or a name on `PATH` |
| `max_concurrent_runs` | `QUARRY_MAX_CONCURRENT_RUNS` | `2` | Concurrent runs per gateway. Over the limit is **refused, not queued** |
| `run_record_dir` | `QUARRY_RUN_RECORD_DIR` | `"quarry-runs"` | Parent directory; each run gets its own subdirectory |
| `retention_max_runs` | `QUARRY_RETENTION_MAX_RUNS` | `50` | Keep at most this many run directories. `0` disables |
| `retention_max_age_seconds` | `QUARRY_RETENTION_MAX_AGE_SECONDS` | `0` | Delete run directories older than this. `0` disables |
| `run_timeout_seconds` | `QUARRY_RUN_TIMEOUT_SECONDS` | `900` | Kill a run after this long. `0` disables |

### The child's environment is constructed, not inherited

A quarry child is spawned with `env_clear()` and given only the variables its
caller explicitly puts in the request. It does **not** see `ANTHROPIC_API_KEY`,
`DISCORD_BOT_TOKEN`, `AWS_*`, `DATABASE_URL`, or any other provider or channel
credential — a provider key would let it bypass the gateway's metering entirely,
and a channel token would let it post as the bot. A short list of known-sensitive
names is additionally stripped as a backstop against a caller that builds the
wrong map, and a stripped key is logged (by name only) as a misconfiguration.

Every spawn is audited as `quarry_run_started` with an `env_keys` array: **keys
only, never values.** That is the record an operator reads to confirm nothing
leaked.

### Refused, not queued

A request over `max_concurrent_runs` is refused immediately. A queue would turn a
concurrency limit into unbounded, undisclosed latency — and a caller's own
deadline could expire while it waited, which would surface as time truncation of
a run that never started.

### `run_timeout_seconds` is not a substitute for quarry's own caps

This timeout is a host-side backstop. When it fires, the run is reported as
**time truncation** — never as budget degradation. The distinction matters
because the repair differs: a caller told "priced out" raises its spend cap and
buys nothing when what actually ran out was time. quarry's own `--cap` and
`--deadline` are what actually bound a run; this only bounds how long the gateway
will hold a slot.

Events already emitted before a kill are kept and reported. A killed run is a
truncated run, not a discarded one — the money was already spent, so the receipt
has to survive.

### How a run's outcome is classified

| Outcome | Meaning |
|---------|---------|
| `completed` | Clean exit, an answer, and quarry's record shows nothing was cut short |
| `truncated` | Clean exit with an answer, but a cap bit. `truncated_by` names which: `spend`, `latency`, or `due`. **A legitimate result** |
| `no_answer` | quarry produced nothing affordable. Its record is still written and still citable |
| `timed_out` | `run_timeout_seconds` fired. Time truncation |
| `cancelled` | Cancelled mid-flight, including at shutdown. Time truncation |
| `crashed` | Non-zero exit that is not `no_answer`. A fault, not degradation |
| `killed_by_signal` | Terminated by a signal; the child's own error reporting never ran |
| `stream_malformed` | The child emitted no parseable event at all — wrong binary, or a contract break |

The truncation verdict is read from quarry's own record file, never inferred from
how many events arrived: a short stream is equally consistent with a small tree, a
crash, or a deadline. An individually unparseable **line** is skipped, recorded,
and the run continues; a **stream** with no events at all is `stream_malformed`,
which is a different fault and should not be retried the same way.

### Shutdown

In-flight runs are given up to `gateway.shutdown_timeout_seconds` to finish before
their tasks are aborted, so a run that already spent money gets a chance to write
its record. Anything still running when that budget expires is killed, and the
loss is logged.

### Retention

When enabled, a background reaper runs hourly (and once at startup, since records
left by a previous process are the most likely to be overdue). Both limits are
independent, and with both set to `0` nothing is ever deleted — a legitimate
choice when records are archived elsewhere.

## `otel.*`

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `endpoint` | `OTEL_EXPORTER_OTLP_ENDPOINT` | — | OTLP gRPC endpoint. Empty = tracing disabled |
| `service_name` | — | `"rustynail"` | Service name in traces |

```yaml
otel:
  endpoint: http://otel-collector:4317
  service_name: rustynail-prod
```

## `dashboard.*`

| Field | Env var | Default | Description |
|-------|---------|---------|-------------|
| `auth_password` | `DASHBOARD_AUTH_PASSWORD` | — | HTTP Basic Auth password for dashboard. Empty = no auth. Username is always `rustynail` |

```yaml
dashboard:
  auth_password: ${DASHBOARD_AUTH_PASSWORD}
```
