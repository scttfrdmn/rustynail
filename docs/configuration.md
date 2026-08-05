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
  default_timezone: America/New_York
  approval_timeout_seconds: 300
  policy:
    default:
      allowed_denominations: [spend, due]
      max_spend_micro_usd: 1000000      # $1.00
      default_spend_micro_usd: 250000   # $0.25
      on_over_limit: reduce
    channels:
      discord-1:
        allowed_denominations: [spend, latency, due]
        max_spend_micro_usd: 5000000
        default_spend_micro_usd: 1000000
        max_latency_seconds: 1800
        on_over_limit: reduce
        scope_tags:
          tenant: engineering
    senders:
      alice:
        allowed_denominations: [spend, latency, due]
        max_spend_micro_usd: 50000000
        allow_unlimited: false
        on_over_limit: refuse
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
| `default_timezone` | `QUARRY_DEFAULT_TIMEZONE` | — | IANA zone deadlines resolve in when a sender has no stored preference. Empty = UTC |
| `approval_timeout_seconds` | `QUARRY_APPROVAL_TIMEOUT_SECONDS` | `300` | How long a sender has to approve a plan. **Expiry cancels at zero spend.** Values below `15` are clamped up; `0` does **not** disable the gate |
| `policy` | — (file only) | empty | Who may run quarry, with what caps, in what scope. **Empty means nobody may run.** See below |

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

### Caps come from the sender's words; a deadline is a price

A run cannot start without a cap. That is quarry's design, not a missing default:
`Caps.Validate()` refuses an uncapped run with *"at least one cap is required
(P9)"* because **planning is budget-conditioned** — a planner with no budget has
nothing to plan against. So the gateway will not pick a cap on a sender's behalf.
A message with no cap gets a question back.

Exactly three denominations exist, and they are not interchangeable:

| Sender writes | Denomination | Notes |
|---|---|---|
| `up to $5`, `5 dollars`, `USD 5` | `spend` | int64 micro-dollars, `$5` → `5000000` |
| `within 20 minutes`, `under 90s` | `latency` | A duration |
| `by 5pm`, `by tonight`, `by Friday` | `due` | An instant, in the **sender's** timezone |

**"at most 30 agents" is not a cap.** quarry has no agent-count denomination; how
wide to go is the planner's decision under the budget. The nearest thing is
recursion depth, which quarry calls *"a BACKSTOP, not the design"* — a run bounded
by depth is under-verified rather than complete. Asking for one gets a question,
not a silently dropped constraint.

**A deadline is a price control, not a scheduling field.** quarry's
`Deferrable()` is true when a `due` is set and no `latency` is: a run that is not
needed soon can use batch and off-peak inference, which is cheaper. This is why
`default_timezone` matters — "by tonight" resolved in UTC for a sender in New York
buys four hours less compute than they asked for. Set it to where your users are.

The resolution chain is: the sender's stored preference (`POST
/users/:id/preferences` with `{"timezone": "America/New_York"}`), then
`default_timezone`, then UTC. Whichever step supplied it, the resolved instant and
its source are echoed back to the sender **before** spend, so a wrong guess is
visible while it is still free to correct.

Two things are disclosed rather than done quietly:

- **Ambiguity is asked about, not resolved.** "by 5" is two times twelve hours
  apart; picking one would pick the sender's budget for them.
- **`Due` has no upstream flag yet.** quarry's `cmd/quarry` sets
  `Caps{Spend, Latency}` and nothing populates `Due`, so a deadline can currently
  only be honoured as an equivalent `latency` — which forfeits `Deferrable()` and
  the cheap path with it. The substitution is reported to the sender rather than
  performed silently.

### `quarry.policy` — what a sender is *allowed*

What a sender **asks for** and what they **get** are separate decisions. The
section above is the first; `quarry.policy` is the second. A sender who writes
"spend up to $500" gets whatever policy permits, and is told so before any money
moves.

`policy` is **file-only**. A nested per-sender cap table cannot be expressed as one
environment variable, and flattening it into a delimited string would reintroduce
exactly the parsing this feature's security argument depends on avoiding.

#### Default-deny

An absent or empty `policy` means **nobody may run quarry**. A missing config is
never read as "unlimited"; the failure mode of the opposite default is an unbounded
spend on a fresh install.

#### Precedence: most specific wins, and entries are not merged

`senders[<sender_id>]`, else `channels[<channel_id>]`, else `default`. The matching
entry is used **whole** — fields are not inherited from a broader level.

That is deliberate. If levels merged, a channel entry with `allow_unlimited: false`
would silently stop applying the moment a sender override set an unrelated field,
because the override's own `false` default is indistinguishable from "not
specified". Taking an entry whole means each override is one auditable decision.
The cost is that an override must restate everything it wants.

#### Entry fields

| Field | Default | Description |
|---|---|---|
| `allowed_denominations` | `[]` | Which of `spend`, `latency`, `due` the sender may set themselves. Empty = none; the defaults below apply |
| `max_spend_micro_usd` | — | Largest spend cap requestable, in int64 micro-dollars. Omitted = no ceiling; `-1` = unlimited ceiling |
| `default_spend_micro_usd` | — | Spend cap applied when the sender names none |
| `max_latency_seconds` | — | Largest latency cap requestable |
| `default_latency_seconds` | — | Latency cap applied when the sender names none |
| `allow_unlimited` | `false` | Permit an explicitly unlimited spend cap |
| `on_over_limit` | `refuse` | `reduce` (grant the maximum, disclose it) or `refuse`. **Anything unrecognised refuses**, so a typo cannot land on the permissive branch |
| `scope_tags` | `{}` | Extra scope tags every matching run carries. Cannot override `user` or `channel` |

Costs are **int64 micro-dollars** throughout — `$1.00` is `1000000`. `-1` means
unlimited and is distinct from `0`, which is a zero budget. Note that a policy which
grants no cap in any denomination is refused as a misconfiguration rather than
treated as unlimited: quarry cannot plan without a budget.

#### Being allowed to set a cap is its own permission

A sender may be permitted `spend` but not `due`. That is not fussiness: a `due`
with no `latency` is what makes a run *deferrable*, and a sender who can set their
own `latency` can force every run onto the expensive path. A denomination the sender
is not permitted to set is discarded, the policy default applies, and the sender is
told — never silently dropped.

#### Reduce-with-disclosure, or refuse

Both outcomes appear in the plan message **before** spend. "You asked for $5, policy
allows $1, proceeding with $1" is fine; proceeding with $1 without saying so is the
quiet degradation quarry's P9 disclosure exists to prevent.

A deadline is the one thing never clamped downward: a later deadline is a *weaker*
constraint, and tightening it would be the opposite of what a limit is for.

#### Scope: this gateway is the security boundary

quarry's cache key is **scope-qualified**, not the statement hash alone. Two senders
can pose a byte-identical sub-problem while holding different entitlements, and one's
cached answer may derive from documents the other cannot see. So **getting the scope
wrong is a cross-tenant data leak**, not a misconfiguration.

quarry treats tags as opaque — it hashes them and compares them, nothing more. In
quarry's own reference deployment the real enforcement is AWS IAM and quarry's local
check is explicitly "a fast-fail courtesy, not the security boundary." **There is no
IAM here.** Nothing downstream catches a sloppy scope, so the gateway is defensive
about it:

- Scope is minted from **verified channel identity only** — `user` and `channel`
  from the channel adapter. Nothing in the message body reaches it, so a sender
  cannot widen their own scope by writing scope-shaped text.
- `scope_tags` in a policy entry **cannot override `user` or `channel`**. An entry
  that could would let one policy entry address another sender's cache namespace.
- Tag keys and values may not contain `=`, `;`, or a control character. quarry's
  `Scope.Key()` renders `k=v;` **without escaping its own separators**, so
  `{tenant: "victim;user=alice"}` produces byte-identical output to
  `{tenant: "victim", user: "alice"}`. Such a value is **refused**, not escaped:
  escaping would make this gateway's cache keys disagree with quarry's, and every
  hit would become a miss.
- A scope must carry at least one tag. quarry's `NarrowsTo` is a subset check, so an
  empty scope narrows to *every* scope and would pass any entitlement check.

**Narrowing relation: subset-of-tags.** This matches quarry exactly. Hierarchical
tag values are **not** supported — `{scope: "chemistry"}` does *not* narrow to
`{scope: "chemistry/chem-101"}`, because quarry's relation is subset, not prefix.
Adopting prefix semantics locally would make a check pass in the gateway and fail in
quarry, and it would fail *open* in the direction of the cache. Scope values are
opaque, flat identifiers.

#### The child's only credential

A quarry run calls the gateway's own `/v1/chat/completions` endpoint, which sits
behind bearer auth. The child receives exactly two environment variables —
`QUARRY_PROVIDER_URL` and `QUARRY_PROVIDER_TOKEN` — and nothing else. The token is
`gateway.api_token`, read live so a rotated token reaches the next run.

**With no `gateway.api_token` set, quarry runs are refused.** An absent token does
not mean "spawn without one": it means `/v1` has no authentication at all, so there
is no credential to hand over and nothing keeping anything else on the host out
either.

#### Audit and reload

Every decision is written to the audit log as `quarry_policy_decision` — grants as
well as refusals, with the requested and granted spend, which precedence level
matched, any adjustments, and the resolved scope key. Grants are logged because
"what was this run allowed to spend" is the first question asked after an unexpected
bill, and refusals alone cannot answer it.

`policy` is **SIGHUP-reloadable**. A reload applies to the next run; runs already in
flight keep the caps they started with, since those were handed to a child process
and cannot be revised. An operator who had to restart the gateway to tighten a cap
would not tighten it.

### The plan gate: a human approves in chat, or nothing runs

Being *allowed* to run under `policy` is not the same as running. Once the policy
resolves a grant, the gateway sends the sender a plan message and **waits**. Only
an explicit approval from that sender starts a quarry process.

The message states the granted caps, any adjustment the policy made to what was
asked for, and the expiry window:

```
**Before I spend anything — approve this run?**

> how many moons does mars have

**Limits in force**
• Spend: at most $5.0000
• Time: at most 30m

quarry plans to fit these limits, so the spend limit is the most this can cost.

**I had to change what you asked for**
• you asked for spend $50.0000, policy allows $5.0000 — proceeding with $5.0000

**No cost estimate:** quarry has no plan-only mode, so producing one would be a
real planner call that itself spends.

This offer expires in 5m, and expiring cancels it — I will not run anything
unless you say so.
Reply **yes / y / approve** to run it, or **no / n / cancel** to cancel.
```

The plan gates on **the cap, not on an estimate**. Under quarry's P4 the cap is
the contract — quarry fits its plan to the cap rather than discovering the cost
partway through — so the spend limit shown *is* the ceiling on the bill. No
estimate appears because quarry has no plan-only mode upstream; the only planner
entry point is a real call that spends, so producing an estimate would spend money
to decide whether to spend money. That absence is **stated in the message** rather
than rendered as `$0.00`, since a sender who reads a fabricated zero is
approving against a measurement nobody took.

#### Reply vocabulary

Replies are matched **whole-word**, case-insensitively, after trimming and
stripping trailing punctuation:

| Meaning | Accepted |
|---------|----------|
| Approve | `yes` `y` `approve` `approved` `ok` `okay` `go` `run` |
| Cancel | `no` `n` `cancel` `stop` `abort` `nope` `nevermind` |

A **multi-word** reply is never scanned for a keyword. "yes, but cheaper" and
"no, wait — yes" each contain an approve word and neither is an approval; both
get a re-prompt that repeats the vocabulary, and the run keeps waiting. Only the exact
forms above settle the gate.

#### What cannot be configured

- **Silence never approves.** There is no `default_approve` setting and no way to
  write one. A timeout cancels, and cancelling records zero spend.
- **`approval_timeout_seconds: 0` does not disable the gate.** It is clamped up to
  15 seconds — a zero-second window would cancel every run before its plan
  message finished sending, which reads as a broken integration rather than as a
  policy.
- **A bystander cannot approve.** The pending approval is keyed on
  `(channel, sender)`, so a `yes` from anyone other than the sender who asked is
  an ordinary message. Two senders in the same channel can each have a plan
  outstanding without either being able to answer the other's.
- **A second request supersedes the first.** The older plan is cancelled at zero
  spend and the sender is told, rather than leaving two live gates keyed on
  the same sender.

Approval replies are exempt from message deduplication. A sender who approves two
runs in a session sends the *same* one-word reply twice, and the dedup ring buffer
would otherwise drop the second as a repeat — leaving that run to expire
unapproved with nothing to explain why. The exemption is not a bypass: it applies
only while that sender has a plan outstanding on that channel, and settling the
approval clears it.

Every decision is audited as `quarry_plan_decision` with the request id and the
outcome (`approved`, `cancelled`, `expired`, `superseded`), correlating with the
`quarry_policy_decision` and `quarry_run_started` records for the same run.

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
