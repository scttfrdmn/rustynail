# Channel Setup Guides

RustyNail supports 12 channels. This guide walks through the prerequisites, credential steps, and configuration for each one.

## 1. Discord

### Prerequisites

- A Discord account with server admin permissions
- Rust 1.75+ and RustyNail built

### Steps

1. Go to https://discord.com/developers/applications and create a **New Application**.
2. Navigate to **Bot** → **Add Bot**.
3. Copy the **Token** (this is your `DISCORD_BOT_TOKEN`).
4. Under **Privileged Gateway Intents**, enable **Message Content Intent**.
5. Go to **OAuth2 → URL Generator**: select `bot` scope, then `Send Messages` and `Read Messages/View Channels` permissions.
6. Use the generated URL to invite the bot to your server.

### Config

```yaml
channels:
  discord:
    enabled: true
    auth:
      token: ${DISCORD_BOT_TOKEN}
```

Or: `export DISCORD_BOT_TOKEN=<your-token>`

### Verification

Start RustyNail and send a message to your bot in Discord. It should reply.

### Notes

- Discord uses the serenity gateway (persistent WebSocket) — no public URL required.
- The bot only receives messages where it is mentioned (or in DMs).

---

## 2. WhatsApp

### Prerequisites

- Meta Business account with WhatsApp Cloud API enabled
- A phone number registered with Meta
- A public HTTPS URL for the webhook (use ngrok for local testing)

### Steps

1. Go to https://developers.facebook.com, create an app, and add **WhatsApp** product.
2. In **WhatsApp → Configuration**, note your **Phone Number ID** and generate a **System User Access Token**.
3. Set a **Verify Token** (any string you choose).
4. Set the webhook URL to: `https://your-domain/webhooks/whatsapp`
5. Subscribe to the `messages` field.

### Config

```yaml
channels:
  whatsapp:
    enabled: true
    phone_number_id: ${WHATSAPP_PHONE_NUMBER_ID}
    access_token: ${WHATSAPP_ACCESS_TOKEN}
    verify_token: ${WHATSAPP_VERIFY_TOKEN}
```

### Webhook URLs

- `GET /webhooks/whatsapp` — Meta webhook verification (challenge-response)
- `POST /webhooks/whatsapp` — inbound messages

### Notes

- Graph API version used: v18.0
- Long messages are automatically chunked if `gateway.chunking_enabled: true` (limit: 4096 chars).

---

## 3. Telegram (Webhook)

### Prerequisites

- A Telegram account
- A public HTTPS URL for the webhook

### Steps

1. Open Telegram and message `@BotFather`. Send `/newbot` and follow the prompts.
2. Copy the **bot token** (format: `123456789:ABCdef...`).
3. Choose a **webhook secret** (any random string ≥32 chars recommended).
4. Register the webhook:
   ```bash
   curl "https://api.telegram.org/bot<TOKEN>/setWebhook" \
     -d "url=https://your-domain/webhooks/telegram" \
     -d "secret_token=<YOUR_SECRET>"
   ```

### Config

```yaml
channels:
  telegram:
    enabled: true
    bot_token: ${TELEGRAM_BOT_TOKEN}
    webhook_secret: ${TELEGRAM_WEBHOOK_SECRET}
    mode: webhook
```

### Webhook URL

`POST /webhooks/telegram` — receives `X-Telegram-Bot-Api-Secret-Token` header for verification.

---

## 4. Telegram (Long-Poll)

Use this mode when you cannot expose a public URL (e.g. local dev or behind a firewall).

### Config

```yaml
channels:
  telegram:
    enabled: true
    bot_token: ${TELEGRAM_BOT_TOKEN}
    mode: longpoll
```

Or: `TELEGRAM_MODE=longpoll`

RustyNail spawns a background loop calling `getUpdates?timeout=30` with automatic offset tracking. No public URL required. On error it waits 5 seconds and retries.

---

## 5. Slack (Events API Webhook)

### Prerequisites

- Slack workspace admin access
- A public HTTPS URL for the webhook

### Steps

1. Go to https://api.slack.com/apps and create a **New App** from scratch.
2. Under **OAuth & Permissions → Scopes**, add `chat:write`, `channels:history`, `im:history`.
3. Install the app to your workspace and copy the **Bot User OAuth Token** (`xoxb-`).
4. Under **Basic Information → App Credentials**, copy the **Signing Secret**.
5. Under **Event Subscriptions**, enable events and set the Request URL to `https://your-domain/webhooks/slack`. Subscribe to `message.channels` and `message.im` bot events.

### Config

```yaml
channels:
  slack:
    enabled: true
    bot_token: ${SLACK_BOT_TOKEN}
    signing_secret: ${SLACK_SIGNING_SECRET}
    mode: webhook
```

### Webhook URLs

- `POST /webhooks/slack` — receives events; verifies HMAC-SHA256 signature using `X-Slack-Signature` header.

---

## 6. Slack (Socket Mode)

Use Socket Mode when you cannot expose a public URL.

### Additional Steps

1. Under **Socket Mode**, enable it and generate an **App-Level Token** with `connections:write` scope. It starts with `xapp-`.

### Config

```yaml
channels:
  slack:
    enabled: true
    bot_token: ${SLACK_BOT_TOKEN}
    mode: socket
    app_token: ${SLACK_APP_TOKEN}
```

Or: `SLACK_MODE=socket SLACK_APP_TOKEN=xapp-...`

No public URL required. RustyNail opens a persistent WebSocket to Slack's infrastructure with exponential backoff reconnection.

---

## 7. SMS / Twilio

### Prerequisites

- Twilio account (https://www.twilio.com)
- A Twilio phone number
- A public HTTPS URL for the webhook

### Steps

1. In the Twilio Console, note your **Account SID** and **Auth Token**.
2. Purchase or use an existing Twilio phone number.
3. In the number's settings, set the **Messaging webhook** to: `https://your-domain/webhooks/sms` (HTTP POST, TwiML).

### Config

```yaml
channels:
  sms:
    enabled: true
    auth:
      account_sid: ${TWILIO_ACCOUNT_SID}
      auth_token: ${TWILIO_AUTH_TOKEN}
      from_number: "+15551234567"
```

### Webhook URL

`POST /webhooks/sms` — receives Twilio `Body`, `From`, `To` form fields and returns empty TwiML. Outbound SMS is sent via the Twilio Messages REST API.

---

## 8. Microsoft Teams

### Prerequisites

- Azure account and Microsoft Teams workspace
- A public HTTPS URL for the Bot Framework webhook

### Steps

1. Go to https://dev.botframework.com and register a **new bot**.
2. Note the **App ID** and generate an **App Password** (client secret).
3. Set the messaging endpoint to: `https://your-domain/channels/teams/messages`
4. In Teams Admin, add the bot as a channel.

### Config

```yaml
channels:
  teams:
    enabled: true
    auth:
      app_id: ${TEAMS_APP_ID}
      app_password: ${TEAMS_APP_PASSWORD}
      hmac_secret: ${TEAMS_HMAC_SECRET}  # optional
```

### HMAC Validation

Set `hmac_secret` (env: `TEAMS_HMAC_SECRET`) to enable HMAC-SHA256 validation of inbound Bot Framework activities using the `Authorization: HMAC <hex>` header. Empty = skip validation (backward compatible).

### Webhook URL

`POST /channels/teams/messages`

RustyNail uses OAuth2 client credentials (App ID + Password) to obtain Bearer tokens for outbound sends, with 60-second pre-refresh caching.

---

## 9. Email (IMAP + SMTP)

### Prerequisites

- An email account with IMAP access enabled (e.g. Gmail with "Allow less secure apps" or an App Password)
- IMAP and SMTP host/port details

### Config

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

### Notes

- IMAP is polled every 30 seconds using a sync client on a dedicated thread.
- HTML email and quoted-text reply headers are automatically stripped.
- Use an **App Password** for Gmail accounts with 2FA (not your regular password).
- `~` in paths is expanded to the home directory.

---

## 10. Webchat

Serves a self-contained JavaScript widget that users can embed in any web page.

### Config

```yaml
channels:
  webchat:
    enabled: true
    allowed_origins:
      - https://myapp.example.com
    welcome_message: "Hello! How can I help you today?"
```

Or: `WEBCHAT_ENABLED=true WEBCHAT_ALLOWED_ORIGINS=https://myapp.example.com`

### Embedding the widget

Add this to your HTML page:

```html
<script src="https://rustynail.example.com/channels/webchat/widget.js"></script>
```

The widget (~3KB, no external dependencies) opens a WebSocket connection and renders a chat UI in the bottom-right corner with:
- Auto-reconnecting WebSocket
- Token streaming support (messages appear word-by-word)
- Configurable welcome message

### WebSocket

`GET /channels/webchat/ws?session_id=<uuid>` — session_id must be a UUID; each page load gets a new session.

### CORS / Origins

`allowed_origins` restricts which `Origin` headers are accepted for WebSocket upgrades. Empty = allow all. Note this is separate from `gateway.allowed_ws_origins`, which applies to both the dashboard and webchat sockets.

---

## 11. Generic Webhook

Receive messages from any system that can send HTTP POST requests.

### Config

```yaml
channels:
  webhook:
    enabled: true
    endpoints:
      - path: my-crm
        user_id: crm-user
        secret: ${CRM_WEBHOOK_SECRET}
        extract_text: "$.event.message"
      - path: alerting
        user_id: alert-bot
```

### Routes

`POST /webhooks/<path>` — one route per configured endpoint.

### HMAC Verification

If `secret` is set, RustyNail verifies the `X-Hub-Signature-256` header (HMAC-SHA256 of the raw body, format: `sha256=<hex>`). Requests with invalid or missing signatures are rejected with 401.

### Text extraction

`extract_text` is a JSONPath expression (e.g. `$.event.message.text`). When absent, the full raw request body is used as the message text.

---

## 12. Test Channel

A zero-credential development channel for integration testing. **Never enable in production.**

### Config

```yaml
channels:
  test_channel: true
```

### Endpoints

- `POST /test/send` — inject a message:
  ```bash
  curl -X POST http://localhost:8080/test/send \
    -H 'Content-Type: application/json' \
    -d '{"user_id":"u1","content":"hello"}'
  ```
- `GET /test/responses` — drain pending bot responses:
  ```bash
  curl http://localhost:8080/test/responses
  ```

Use with `agents.llm_provider: stub` (echo mode) for fully offline integration tests. See `configs/harness.yaml` for the minimal zero-credential config.
