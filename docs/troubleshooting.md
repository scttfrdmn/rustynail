# Troubleshooting

## Startup Failures

### `ANTHROPIC_API_KEY not set` / API key missing

```
Error: API key missing (agents.api_key / ANTHROPIC_API_KEY)
```

**Fix:** Set the environment variable before starting:
```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

Or add it to your `config.yaml`:
```yaml
agents:
  api_key: ${ANTHROPIC_API_KEY}
```

Run `rustynail config validate` to confirm it is detected.

---

### Config load failed (YAML parse error)

```
Error: Config load failed: ...
```

**Fix:** Run `rustynail config check` for a human-readable summary, or `rustynail config validate` for preflight checks. Common causes:

- Indentation errors in YAML (use 2 spaces, not tabs)
- Unquoted strings containing special characters (`:`, `{`, `}`)
- Referencing an env var that is not set: `${VAR}` expands to empty string — check the var is exported

---

### Port already in use

```
Error: ... Address already in use (os error 98)
```

**Fix:** Find and stop the conflicting process:
```bash
lsof -i :8080
kill -TERM <pid>
```

Or change the port in your config:
```yaml
gateway:
  http_port: 9090
```

---

### Cannot connect to Redis / Postgres / SQLite

```
Error: Memory backend connection failed
```

**Fix for Redis:**
```bash
# Test connectivity
redis-cli -u redis://localhost:6379 ping
# Should return: PONG
```

Check `memory.redis_url` is correct and Redis is running.

**Fix for Postgres:**
```bash
psql $DATABASE_URL -c "SELECT 1"
```

Check the connection string format: `postgresql://user:password@host:5432/dbname`

**Fix for SQLite:**
Check that the directory exists and is writable:
```bash
ls -la $(dirname /path/to/rustynail.db)
```

---

## Channel Connection Issues

### Discord: bot not responding

1. Check the bot token is valid: `DISCORD_BOT_TOKEN` must be a full bot token, not a client secret.
2. Verify **Message Content Intent** is enabled in the Discord Developer Portal under your app → Bot.
3. Check the bot has been invited to the server with `Send Messages` and `Read Messages` permissions.
4. Look for errors in logs: `RUST_LOG=debug cargo run 2>&1 | grep discord`

### WhatsApp: webhook verification fails (403)

The `verify_token` in your config must exactly match the token set in Meta's webhook configuration.

```bash
curl "https://your-domain/webhooks/whatsapp?hub.mode=subscribe&hub.verify_token=YOUR_TOKEN&hub.challenge=test"
# Should return: test
```

### Telegram: bot not receiving messages

For webhook mode: verify `setWebhook` was called and returns `ok: true`:
```bash
curl "https://api.telegram.org/bot<TOKEN>/getWebhookInfo"
```

For long-poll mode: check for errors in logs — the long-poll loop logs connection errors.

Ensure the `webhook_secret` in config matches what was set in `setWebhook`.

### Slack: signature verification failing (401)

```
warn: Slack signature verification failed
```

Causes:
- `signing_secret` is wrong — copy it from Slack App → Basic Information → App Credentials
- Request timestamp is stale (>5 minutes old) — check system clock sync
- Body was modified by a proxy — ensure raw bytes reach the handler

### Microsoft Teams: HMAC validation rejected

If `teams.auth.hmac_secret` is set, the `Authorization: HMAC <hex>` header must be present and valid. To disable validation, unset `TEAMS_HMAC_SECRET` (empty = skip).

---

## No Response to Messages

### Step 1: Check /status

```bash
curl http://localhost:8080/status
```

Look at `channels` — if a channel shows `"status": "degraded"` or `"unhealthy"`, the issue is at the channel layer.

### Step 2: Check /admin/channels/health

```bash
curl http://localhost:8080/admin/channels/health \
  -H 'Authorization: Bearer <token>'
```

This returns per-channel `health_detail` strings with error context for degraded channels.

### Step 3: Enable debug logging

```bash
RUST_LOG=debug cargo run 2>&1 | tee rustynail.log
```

Look for:
- `rate_limit_hit` — user is being rate-limited (friendly message sent instead of agent response)
- `dedup: dropping duplicate` — message was deduplicated
- `llm_error` — LLM call is failing; check API key and provider status
- `agent_created` vs `agent reused` — confirm the user is being recognized

### Step 4: Check rate limiting

```bash
curl http://localhost:8080/metrics | grep rate_limit
# rustynail_rate_limit_hits_total 3
```

If the counter is increasing, users are hitting the rate limit. Adjust `gateway.rate_limit.messages_per_window` or `window_seconds`.

### Step 5: Use the test channel

The test channel bypasses all channel auth and is useful for isolating issues:

```bash
# config.yaml: channels: { test_channel: true }
curl -X POST http://localhost:8080/test/send \
  -H 'Content-Type: application/json' \
  -d '{"user_id":"debug-user","content":"hello"}'

sleep 2

curl http://localhost:8080/test/responses
```

If the test channel works but Discord does not, the issue is in the Discord channel adapter.

---

## Memory Backend Issues

### Redis: connection refused

```
WARN: Redis connection failed, falling back to in-memory
```

Check Redis is running and reachable:
```bash
redis-cli ping
```

The `redis://` URL must include the correct host:port. For Docker Compose, use the service name: `redis://redis:6379`.

### SQLite: permission denied

```
Error: unable to open database file
```

Ensure the directory exists and the process has write permission:
```bash
mkdir -p /data && chmod 777 /data
# or run as the correct user
```

### Postgres: relation does not exist

RustyNail auto-creates the `rustynail_messages` table on first use. If this fails (e.g. the user lacks CREATE TABLE permission), grant the permission:
```sql
GRANT CREATE ON SCHEMA public TO rustynail;
```

### History lost on restart

You are using the default `inmemory` backend. Switch to a persistent backend:
```yaml
memory:
  backend: redis
  redis_url: redis://localhost:6379
```

---

## Performance Tuning

### Reducing memory usage per user

Reduce the history window:
```yaml
agents:
  max_history: 10   # default 20
```

Enable summarization to compress old context:
```yaml
memory:
  summarization:
    enabled: true
    trigger_at: 20
    keep_recent: 5
```

### Handling high message volume

1. Enable rate limiting to protect the LLM API from overuse:
   ```yaml
   gateway:
     rate_limit:
       enabled: true
       messages_per_window: 20
       window_seconds: 60
   ```

2. Enable deduplication to discard accidental repeats:
   ```yaml
   gateway:
     deduplication:
       enabled: true
   ```

3. Add fallback providers so capacity errors on the primary are retried on a secondary:
   ```yaml
   agents:
     fallback_providers:
       - provider: openai
         model: gpt-4o-mini
         api_key: ${OPENAI_API_KEY}
   ```

4. Enable retry jitter to reduce thundering-herd on multi-instance deployments:
   ```yaml
   agents:
     retry:
       jitter_enabled: true
   ```

### Slow response times

Check `rustynail_message_duration_seconds` in metrics — percentile buckets show whether the tail is at the LLM or in the pipeline. The gateway overhead is <1ms; slow responses are almost always LLM latency.

Consider using a faster model (e.g. `claude-3-haiku-20240307`) for the summarization model to reduce background costs:
```yaml
memory:
  summarization:
    model: claude-3-haiku-20240307
```

---

## Useful Debug Commands

```bash
# Validate configuration before starting
rustynail config validate

# Print configuration summary
rustynail config check

# Check if the server is running
rustynail status

# Health check
curl http://localhost:8080/health

# Detailed status
curl http://localhost:8080/status

# Channel health (requires bearer auth if configured)
curl http://localhost:8080/admin/channels/health \
  -H 'Authorization: Bearer <token>'

# Prometheus metrics
curl http://localhost:8080/metrics

# Full debug logging
RUST_LOG=debug cargo run

# Check config loaded from specific file
CONFIG_FILE=production.yaml rustynail config check

# Test channel round-trip
curl -X POST http://localhost:8080/test/send \
  -H 'Content-Type: application/json' \
  -d '{"user_id":"test","content":"ping"}'
sleep 1
curl http://localhost:8080/test/responses
```

## Getting Help

- [GitHub Issues](https://github.com/scttfrdmn/rustynail/issues) — bug reports and feature requests
- [CHANGELOG.md](../CHANGELOG.md) — full history of changes
- [Configuration Reference](configuration.md) — all config fields and env vars
- [Architecture](architecture.md) — internals and message pipeline details
