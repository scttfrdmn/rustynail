# Deployment Guide

This guide covers every deployment target: Docker, Docker Compose, Kubernetes via Helm, and bare-metal.

## Docker (Pre-built Image)

A distroless image (~8 MB) is published to GitHub Container Registry on every version tag.

```bash
docker pull ghcr.io/scttfrdmn/rustynail:latest

docker run --rm \
  -e ANTHROPIC_API_KEY=sk-ant-... \
  -e DISCORD_BOT_TOKEN=... \
  -p 8080:8080 \
  ghcr.io/scttfrdmn/rustynail:latest
```

To use a config file, mount it and set `CONFIG_FILE`:

```bash
docker run --rm \
  -e ANTHROPIC_API_KEY=sk-ant-... \
  -e CONFIG_FILE=/etc/rustynail/config.yaml \
  -v $(pwd)/config.yaml:/etc/rustynail/config.yaml:ro \
  -p 8080:8080 \
  ghcr.io/scttfrdmn/rustynail:latest
```

The image runs as distroless `nonroot` (uid 65532) and has no shell or package manager.

## Docker Compose (Local Development)

> **Important**: The build context must be the **parent directory** of `rustynail/` because `agenkit` is a local path dependency at `../agenkit/agenkit-rust`.

```bash
# From the parent directory (one level above rustynail/)
cd ..

# Start with env vars
ANTHROPIC_API_KEY=sk-ant-... \
DISCORD_BOT_TOKEN=... \
docker-compose -f rustynail/docker-compose.yml up

# Or use a .env file in the parent directory
echo "ANTHROPIC_API_KEY=sk-ant-..." > .env
docker-compose -f rustynail/docker-compose.yml up
```

**With persistent Redis memory:**

Add Redis to your compose or use the `--profile redis` if your compose defines it:

```yaml
# Snippet to add to docker-compose.yml
services:
  redis:
    image: redis:7-alpine
    volumes:
      - redis_data:/data

volumes:
  redis_data:
```

Then set `MEMORY_BACKEND=redis` and `REDIS_URL=redis://redis:6379`.

## Kubernetes via Helm

### Prerequisites

- Kubernetes 1.21+
- Helm 3.x
- An image accessible to your cluster (`ghcr.io/scttfrdmn/rustynail` or your own registry)

### Install

```bash
helm install rustynail ./deploy/helm/rustynail \
  --set secrets.anthropicApiKey=sk-ant-... \
  --set image.tag=0.15.0
```

### Key `values.yaml` overrides

```yaml
# Image
image:
  repository: ghcr.io/scttfrdmn/rustynail
  tag: "0.15.0"
  pullPolicy: IfNotPresent

# Replicas and autoscaling
replicaCount: 2
autoscaling:
  enabled: true
  minReplicas: 1
  maxReplicas: 5
  targetCPUUtilizationPercentage: 80

# Secrets (set via --set or external secret manager)
secrets:
  anthropicApiKey: ""
  discordBotToken: ""
  gatewayApiToken: ""
  teamsAppId: ""
  teamsAppPassword: ""

# Ingress
ingress:
  enabled: true
  className: nginx
  hosts:
    - host: rustynail.example.com
      paths:
        - path: /
          pathType: Prefix
  tls:
    - secretName: rustynail-tls
      hosts:
        - rustynail.example.com

# Redis subchart for persistent memory
redis:
  enabled: true
  auth:
    enabled: false

memory:
  backend: redis
  redisUrl: redis://rustynail-redis-master:6379

# SQLite with PVC
memory:
  backend: sqlite
  sqlitePath: /data/rustynail.db
# Add a PVC and volumeMount in your values override
```

### Upgrade

```bash
helm upgrade rustynail ./deploy/helm/rustynail \
  --set image.tag=0.15.0 \
  --reuse-values
```

### Uninstall

```bash
helm uninstall rustynail
```

## Environment Variables Cheat Sheet

Complete list of all environment variables and their config path equivalents.

| Env var | Config path | Required | Default |
|---------|-------------|----------|---------|
| `ANTHROPIC_API_KEY` | `agents.api_key` | **yes** | — |
| `DISCORD_BOT_TOKEN` | `channels.discord.auth.token` | if Discord | — |
| `CONFIG_FILE` | — | no | — |
| `RUST_LOG` | `gateway.log_level` | no | `info` |
| `GATEWAY_API_TOKEN` | `gateway.api_token` | no | — |
| `GATEWAY_MAX_BODY_BYTES` | `gateway.max_body_bytes` | no | `1048576` |
| `GATEWAY_REQUEST_TIMEOUT_SECONDS` | `gateway.request_timeout_seconds` | no | `30` |
| `GATEWAY_SHUTDOWN_TIMEOUT_SECONDS` | `gateway.shutdown_timeout_seconds` | no | `30` |
| `GATEWAY_ALLOWED_WS_ORIGINS` | `gateway.allowed_ws_origins` | no | — |
| `GATEWAY_CHUNKING_ENABLED` | `gateway.chunking_enabled` | no | `false` |
| `GATEWAY_FORMATTING_ENABLED` | `gateway.formatting_enabled` | no | `true` |
| `GATEWAY_AUTO_ROUTE_ATTACHMENTS` | `gateway.auto_route_attachments` | no | `false` |
| `GATEWAY_DEDUP_ENABLED` | `gateway.deduplication.enabled` | no | `false` |
| `GATEWAY_DEDUP_WINDOW_SIZE` | `gateway.deduplication.window_size` | no | `256` |
| `RATE_LIMIT_ENABLED` | `gateway.rate_limit.enabled` | no | `false` |
| `RATE_LIMIT_MESSAGES` | `gateway.rate_limit.messages_per_window` | no | `20` |
| `RATE_LIMIT_WINDOW_SECONDS` | `gateway.rate_limit.window_seconds` | no | `60` |
| `LLM_PROVIDER` | `agents.llm_provider` | no | `anthropic` |
| `AGENTS_RETRY_ENABLED` | `agents.retry.enabled` | no | `true` |
| `AGENTS_RETRY_MAX_ATTEMPTS` | `agents.retry.max_attempts` | no | `3` |
| `AGENTS_RETRY_BASE_DELAY_MS` | `agents.retry.base_delay_ms` | no | `100` |
| `AGENT_RETRY_JITTER_ENABLED` | `agents.retry.jitter_enabled` | no | `false` |
| `MEMORY_BACKEND` | `memory.backend` | no | `inmemory` |
| `REDIS_URL` | `memory.redis_url` | if redis | — |
| `REDIS_TTL_SECONDS` | `memory.redis_ttl_seconds` | no | `0` |
| `SQLITE_PATH` | `memory.sqlite_path` | if sqlite | — |
| `DATABASE_URL` | `memory.postgres_url` | if postgres | — |
| `VECTOR_DECAY_HALF_LIFE_SECONDS` | `memory.vector_decay_half_life_seconds` | no | `3600.0` |
| `SUMMARIZATION_ENABLED` | `memory.summarization.enabled` | no | `false` |
| `SUMMARIZATION_TRIGGER_AT` | `memory.summarization.trigger_at` | no | `40` |
| `SUMMARIZATION_KEEP_RECENT` | `memory.summarization.keep_recent` | no | `10` |
| `SUMMARIZATION_MODEL` | `memory.summarization.model` | no | `claude-3-haiku-20240307` |
| `SUMMARIZATION_TRIGGER_TOKEN_BUDGET` | `memory.summarization.trigger_token_budget` | no | `0` |
| `TOOLS_PDF_ENABLED` | `tools.pdf_enabled` | no | `false` |
| `TOOLS_IMAGE_ENABLED` | `tools.image_enabled` | no | `false` |
| `TAVILY_API_KEY` | `tools.web_search_api_key` | if web-search | — |
| `SKILLS_ENABLED` | `skills.enabled` | no | `false` |
| `SKILLS_PATHS` | `skills.paths` | no | `skills/` |
| `SKILLS_MAX_ACTIVE` | `skills.max_active` | no | `3` |
| `AUDIT_ENABLED` | `audit.enabled` | no | `false` |
| `AUDIT_PATH` | `audit.path` | no | stderr |
| `DASHBOARD_AUTH_PASSWORD` | `dashboard.auth_password` | no | — |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `otel.endpoint` | no | — |
| `WHATSAPP_PHONE_NUMBER_ID` | `channels.whatsapp.phone_number_id` | if WhatsApp | — |
| `WHATSAPP_ACCESS_TOKEN` | `channels.whatsapp.access_token` | if WhatsApp | — |
| `WHATSAPP_VERIFY_TOKEN` | `channels.whatsapp.verify_token` | if WhatsApp | — |
| `TELEGRAM_BOT_TOKEN` | `channels.telegram.bot_token` | if Telegram | — |
| `TELEGRAM_WEBHOOK_SECRET` | `channels.telegram.webhook_secret` | no | — |
| `TELEGRAM_MODE` | `channels.telegram.mode` | no | `webhook` |
| `SLACK_BOT_TOKEN` | `channels.slack.bot_token` | if Slack | — |
| `SLACK_SIGNING_SECRET` | `channels.slack.signing_secret` | if Slack webhook | — |
| `SLACK_APP_TOKEN` | `channels.slack.app_token` | if Slack socket | — |
| `SLACK_MODE` | `channels.slack.mode` | no | `webhook` |
| `TWILIO_ACCOUNT_SID` | `channels.sms.auth.account_sid` | if SMS | — |
| `TWILIO_AUTH_TOKEN` | `channels.sms.auth.auth_token` | if SMS | — |
| `TWILIO_FROM_NUMBER` | `channels.sms.auth.from_number` | if SMS | — |
| `TEAMS_APP_ID` | `channels.teams.auth.app_id` | if Teams | — |
| `TEAMS_APP_PASSWORD` | `channels.teams.auth.app_password` | if Teams | — |
| `TEAMS_HMAC_SECRET` | `channels.teams.auth.hmac_secret` | no | — |
| `EMAIL_IMAP_HOST` | `channels.email.imap.host` | if Email | — |
| `EMAIL_SMTP_HOST` | `channels.email.smtp.host` | if Email | — |
| `EMAIL_USERNAME` | `channels.email.imap.username` | if Email | — |
| `EMAIL_PASSWORD` | `channels.email.imap.password` | if Email | — |
| `WEBCHAT_ENABLED` | `channels.webchat.enabled` | no | `false` |
| `WEBCHAT_ALLOWED_ORIGINS` | `channels.webchat.allowed_origins` | no | — |
| `WEBCHAT_WELCOME_MESSAGE` | `channels.webchat.welcome_message` | no | — |
| `ANTHROPIC_API_BASE` | `agents.api_base` | no | — |
| `AWS_REGION` | `agents.aws_region` | if Bedrock | `us-east-1` |
| `QUARRY_ENABLED` | `quarry.enabled` | no | `false` |
| `QUARRY_BINARY_PATH` | `quarry.binary_path` | if quarry | `quarry` |
| `QUARRY_MAX_CONCURRENT_RUNS` | `quarry.max_concurrent_runs` | no | `2` |
| `QUARRY_RUN_RECORD_DIR` | `quarry.run_record_dir` | no | `quarry-runs` |
| `QUARRY_RETENTION_MAX_RUNS` | `quarry.retention_max_runs` | no | `50` |
| `QUARRY_RETENTION_MAX_AGE_SECONDS` | `quarry.retention_max_age_seconds` | no | `0` |
| `QUARRY_RUN_TIMEOUT_SECONDS` | `quarry.run_timeout_seconds` | no | `900` |
| `QUARRY_DEFAULT_TIMEZONE` | `quarry.default_timezone` | no | UTC |
| `QUARRY_APPROVAL_TIMEOUT_SECONDS` | `quarry.approval_timeout_seconds` | no | `300` |
| `QUARRY_VERIFY_ENABLED` | `quarry.verification.enabled` | no | `true` |
| `QUARRY_VERIFY_IDENTITY` | `quarry.verification.expected_identity` | if verifying | — |
| `QUARRY_VERIFY_ISSUER` | `quarry.verification.expected_issuer` | if verifying | — |
| `QUARRY_VERIFY_COSIGN_PATH` | `quarry.verification.cosign_path` | no | `cosign` |
| `QUARRY_VERIFY_ALLOW_WRITABLE_BINARY` | `quarry.verification.allow_writable_binary` | no | `false` |
| `QUARRY_VERIFY_MANIFEST_PATH` | `quarry.verification.manifest_path` | no | `<binary_path>.manifest.json` |

`quarry.policy` is file-only — there is no env equivalent, and an empty policy means
nobody may run quarry.

## Health Checks & Kubernetes Probes

| Endpoint | Purpose | Returns non-200 when |
|----------|---------|----------------------|
| `GET /live` | Liveness probe | Process is in a bad state |
| `GET /ready` | Readiness probe | Gateway not yet started |
| `GET /health` | Load balancer check | Never (always 200 while running) |
| `GET /status` | Detailed status | N/A (always 200, JSON body) |

**Kubernetes probe YAML:**

```yaml
livenessProbe:
  httpGet:
    path: /live
    port: 8080
  initialDelaySeconds: 10
  periodSeconds: 30

readinessProbe:
  httpGet:
    path: /ready
    port: 8080
  initialDelaySeconds: 5
  periodSeconds: 10
```

`/live` and `/ready` are always exempt from bearer token auth, even when `GATEWAY_API_TOKEN` is set.

## Prometheus & Grafana

### Prometheus scrape config

```yaml
scrape_configs:
  - job_name: rustynail
    static_configs:
      - targets: ['rustynail:8080']
    metrics_path: /metrics
```

The `/metrics` endpoint returns Prometheus text format (content-type `text/plain; version=0.0.4`).

### Key metrics

| Metric | Type | Description |
|--------|------|-------------|
| `rustynail_messages_in_total` | Counter | Inbound messages |
| `rustynail_messages_out_total` | Counter | Outbound messages |
| `rustynail_active_users` | Gauge | Current unique users |
| `rustynail_healthy_channels` | Gauge | Channels in healthy state |
| `rustynail_message_duration_seconds` | Histogram | End-to-end processing time |
| `rustynail_tokens_in_total` | Counter | LLM input tokens |
| `rustynail_tokens_out_total` | Counter | LLM output tokens |
| `rustynail_auth_failures_total` | Counter | Bearer auth rejections |
| `rustynail_rate_limit_hits_total` | Counter | Rate limit hits |
| `rustynail_llm_errors_total` | Counter | LLM call errors |
| `rustynail_llm_retries_total` | Counter | LLM retry attempts |

### Grafana

Import `deploy/grafana/dashboard.json` into Grafana. The provisioning config is at `deploy/grafana/provisioning/dashboards/rustynail.yml`.

Alert rules are in `deploy/prometheus/alerts.yaml`: `RustyNailDown`, `HighMessageLatency`, `ChannelUnhealthy`, `HighErrorRate`, `NoActiveUsers`.

## SIGHUP Hot-Reload

Some config fields update at runtime without restart. Send `SIGHUP` to trigger a reload:

```bash
# Find the PID
pgrep rustynail

# Send SIGHUP
kill -HUP <pid>
```

**Hot-reloadable fields:**

- `gateway.log_level`
- `gateway.api_token`
- `gateway.rate_limit.*`
- `audit.*`

Fields not in this list (channels, memory backend, LLM provider) require a full restart.

**Kubernetes equivalent:**

```bash
kubectl exec -it <pod> -- kill -HUP 1
# or trigger a rolling restart:
kubectl rollout restart deployment/rustynail
```

## Deploying quarry alongside the gateway

quarry is a **separate binary** the gateway spawns, not a library it links. If
`quarry.enabled` is `false` (the default) none of this applies — no binary is
spawned and nothing needs verifying.

### 1. Obtain the binary

Take a **release artifact**, not a `go build` of `main`. Verification is against the
release workflow's signing identity, so a locally-built binary cannot pass it — by
design.

```bash
VERSION=v0.4.0   # whichever release you intend to run
gh release download "$VERSION" --repo scttfrdmn/quarry \
  --pattern 'quarry_*_linux_amd64*'
```

### 2. Verify it before you install it

Verify by hand once, at install time, so a failure is something you see now rather
than a refused run at 3am. This is the same check the gateway makes at every spawn.

```bash
cosign verify-blob \
  --certificate-identity-regexp \
    'https://github.com/scttfrdmn/quarry/\.github/workflows/release\.yml@refs/tags/.*' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  --bundle quarry.bundle \
  quarry
```

Confirm the identity in the output is the quarry release workflow. A signature that
verifies but names a different identity is not the binary you asked for — that is
the case identity constraints exist to catch, and it is why `--certificate-identity-regexp`
is not optional.

### 3. Install it where the gateway cannot write to it

```bash
sudo install -o root -g root -m 0555 quarry /usr/local/bin/quarry
sudo chown root:root /usr/local/bin      # the *directory* matters too
```

Both the file **and its containing directory** must be unwritable by the gateway's
user. A writable directory lets the binary be replaced between the hash and the
`execve`, which is the same TOCTOU window as a writable file. The gateway refuses
either by default; `allow_writable_binary: true` downgrades both refusals to a
per-spawn warning and should not be set in production.

> **Residual window.** Even with both unwritable, a gap remains between hashing the
> file and executing it: closing it entirely needs `fexecve` against the same
> descriptor that was hashed, which the async process API in use cannot express and
> whose portable `/proc/self/fd` form does not exist on macOS. Root-owned, `0555`,
> in a root-owned directory is what narrows it from the other end.

### 4. Install the capability manifest

```bash
sudo install -o root -g root -m 0444 \
  quarry.manifest.json /usr/local/bin/quarry.manifest.json
```

The manifest must declare exactly this gateway's `http_port` for egress and exactly
its `run_record_dir` as writable — see
[configuration.md](configuration.md#what-the-manifest-must-say). A port mismatch
after you move the gateway to a new port presents as `manifest_rejected`, not as a
connection error.

### 5. Configure and confirm

```yaml
quarry:
  enabled: true
  binary_path: /usr/local/bin/quarry
  run_record_dir: /var/lib/rustynail/quarry-runs
  verification:
    enabled: true
    expected_identity: "https://github.com/scttfrdmn/quarry/.github/workflows/release.yml@refs/tags/*"
    expected_issuer: "https://token.actions.githubusercontent.com"
    cosign_path: /usr/local/bin/cosign
```

```bash
rustynail config validate
```

**Read that output before you rely on it.** The cosign mechanism the spawn gate calls
into is tracked as [#103](https://github.com/scttfrdmn/rustynail/issues/103) and is
not implemented yet, so with verification on, `config validate` reports that every
run will be refused and exits non-zero. That is the fail-closed default working as
intended. Until #103 lands your options are: leave `quarry.enabled: false`, or accept
unverified runs with `verification.enabled: false` and a manifest sidecar in place.

### Reading a refusal

Every refusal is logged with the failing check named and audited as
`quarry_verification_refused` with a `reason` field. The message the sender receives
says only that the capability is unavailable — no path, digest, identity, or config
key — so the log and the audit trail are where the reason lives.

| `reason` | What to fix |
|---|---|
| `mechanism_unavailable` | No verifier installed (#103). Not a misconfiguration on your side |
| `identity_not_configured` | Set `expected_identity` and `expected_issuer` |
| `unsigned` | The artifact has no signature — likely a locally-built binary |
| `wrong_identity` / `wrong_issuer` | Signed, but not by the quarry release workflow. **Investigate before overriding anything** |
| `cosign_unavailable` | `cosign_path` does not resolve to an executable |
| `transparency_log_unreachable` | Network or Rekor outage. Refused rather than proceeding unverified |
| `writable_binary` / `writable_binary_dir` | Re-do step 3 |
| `manifest_rejected` | The manifest declares something other than this gateway's port and run-record directory |
| `manifest_unparseable` | Missing sidecar (development mode) or malformed JSON |
| `binary_missing` | `binary_path` does not resolve |

## Production Checklist

Before going live, verify:

- [ ] `GATEWAY_API_TOKEN` set — all non-probe endpoints require bearer auth
- [ ] `AUDIT_ENABLED=true` and `AUDIT_PATH` points to a writable file or volume
- [ ] `rate_limit.enabled: true` with appropriate `messages_per_window` / `window_seconds`
- [ ] Persistent memory backend configured (Redis, SQLite, or Postgres) — `inmemory` loses history on restart
- [ ] `OTEL_EXPORTER_OTLP_ENDPOINT` set if you want distributed traces
- [ ] `gateway.allowed_ws_origins` set to your frontend domains (empty = allow all)
- [ ] `gateway.shutdown_timeout_seconds` tuned for your workload (default 30s)
- [ ] `DASHBOARD_AUTH_PASSWORD` set if the dashboard is externally accessible
- [ ] Liveness and readiness probes configured in your orchestrator
- [ ] Prometheus scrape configured and Grafana dashboard imported
- [ ] Image pinned to a specific version tag, not `latest`
- [ ] If `quarry.enabled: true` — `rustynail config validate` shows quarry
      verification in the state you intend, `verification.enabled` is **not** `false`,
      `allow_writable_binary` is **not** set, and the binary and its directory are
      root-owned and unwritable by the gateway's user
