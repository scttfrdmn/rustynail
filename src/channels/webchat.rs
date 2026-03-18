use crate::channels::Channel;
use crate::config::WebchatConfig;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};

const NAME: &str = "webchat";

/// Shared map from session_id → broadcast sender for outbound messages.
pub type WebchatSessions = Arc<DashMap<String, broadcast::Sender<String>>>;

pub struct WebchatChannel {
    id: String,
    pub config: WebchatConfig,
    health: Arc<RwLock<ChannelHealth>>,
    /// Shared sessions map; also given to AppState for HTTP WS handler.
    pub sessions: WebchatSessions,
}

impl WebchatChannel {
    pub fn new(id: String, config: WebchatConfig) -> Self {
        Self {
            id,
            config,
            health: Arc::new(RwLock::new(ChannelHealth::Unhealthy {
                reason: "not started".to_string(),
            })),
            sessions: Arc::new(DashMap::new()),
        }
    }

    /// Return a clone of the sessions map to share with the HTTP layer.
    pub fn sessions_handle(&self) -> WebchatSessions {
        self.sessions.clone()
    }
}

#[async_trait]
impl Channel for WebchatChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        NAME
    }

    async fn start(&mut self) -> Result<()> {
        info!("Starting web chat channel (WebSocket mode)");
        *self.health.write().await = ChannelHealth::Healthy;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping web chat channel");
        self.sessions.clear();
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "stopped".to_string(),
        };
        Ok(())
    }

    /// Route the message to the session identified by `message.channel_id`.
    /// The channel_id for webchat messages is `webchat-<session_id>`.
    async fn send_message(&self, message: Message) -> Result<()> {
        let session_key = message
            .channel_id
            .strip_prefix("webchat-")
            .unwrap_or(&message.channel_id);

        if let Some(tx) = self.sessions.get(session_key) {
            let payload = serde_json::json!({
                "type": "message",
                "from": message.username,
                "content": message.content,
            });
            let text = serde_json::to_string(&payload).unwrap_or_default();
            let _ = tx.send(text);
        } else {
            warn!("Webchat: no session found for '{}'", session_key);
        }
        Ok(())
    }

    fn health(&self) -> ChannelHealth {
        self.health.blocking_read().clone()
    }

    fn is_running(&self) -> bool {
        matches!(self.health.blocking_read().clone(), ChannelHealth::Healthy)
    }
}

// ── Embedded widget JS ────────────────────────────────────────────────────────

pub const WIDGET_JS: &str = r#"
(function() {
  var RUSTYNAIL_BASE = window.RUSTYNAIL_BASE || '';
  var sessionId = sessionStorage.getItem('rn_session_id');
  if (!sessionId) {
    sessionId = Math.random().toString(36).slice(2) + Date.now().toString(36);
    sessionStorage.setItem('rn_session_id', sessionId);
  }

  function buildUI() {
    var style = document.createElement('style');
    style.textContent = [
      '#rn-chat{position:fixed;bottom:20px;right:20px;width:340px;font-family:sans-serif;z-index:9999}',
      '#rn-bubble{background:#5865f2;color:#fff;border-radius:50%;width:50px;height:50px;display:flex;align-items:center;justify-content:center;cursor:pointer;font-size:22px;box-shadow:0 2px 8px rgba(0,0,0,.3)}',
      '#rn-box{display:none;flex-direction:column;background:#fff;border-radius:12px;box-shadow:0 4px 20px rgba(0,0,0,.2);overflow:hidden;height:420px}',
      '#rn-header{background:#5865f2;color:#fff;padding:12px 16px;font-weight:600}',
      '#rn-msgs{flex:1;overflow-y:auto;padding:12px;display:flex;flex-direction:column;gap:8px}',
      '.rn-msg{padding:8px 12px;border-radius:12px;max-width:80%;word-break:break-word}',
      '.rn-msg.user{background:#5865f2;color:#fff;align-self:flex-end}',
      '.rn-msg.bot{background:#f0f0f0;color:#333;align-self:flex-start}',
      '#rn-form{display:flex;border-top:1px solid #eee}',
      '#rn-input{flex:1;border:none;padding:10px 12px;outline:none;font-size:14px}',
      '#rn-send{background:#5865f2;color:#fff;border:none;padding:10px 16px;cursor:pointer;font-size:16px}',
    ].join('');
    document.head.appendChild(style);

    var container = document.createElement('div'); container.id = 'rn-chat';
    var bubble = document.createElement('div'); bubble.id = 'rn-bubble'; bubble.textContent = '💬';
    var box = document.createElement('div'); box.id = 'rn-box';
    var header = document.createElement('div'); header.id = 'rn-header'; header.textContent = 'Chat';
    var msgs = document.createElement('div'); msgs.id = 'rn-msgs';
    var form = document.createElement('div'); form.id = 'rn-form';
    var input = document.createElement('input'); input.id = 'rn-input'; input.placeholder = 'Type a message…';
    var send = document.createElement('button'); send.id = 'rn-send'; send.textContent = '➤';
    form.appendChild(input); form.appendChild(send);
    box.appendChild(header); box.appendChild(msgs); box.appendChild(form);
    container.appendChild(bubble); container.appendChild(box);
    document.body.appendChild(container);

    bubble.onclick = function() {
      var open = box.style.display === 'flex';
      box.style.display = open ? 'none' : 'flex';
      if (!open) input.focus();
    };

    return { msgs: msgs, input: input, send: send };
  }

  function addMsg(msgs, text, cls) {
    var el = document.createElement('div');
    el.className = 'rn-msg ' + cls;
    el.textContent = text;
    msgs.appendChild(el);
    msgs.scrollTop = msgs.scrollHeight;
  }

  var ui = buildUI();
  var ws, reconnectDelay = 1000;

  function connect() {
    var proto = location.protocol === 'https:' ? 'wss' : 'ws';
    var base = RUSTYNAIL_BASE || (proto + '://' + location.host);
    ws = new WebSocket(base.replace(/^http/, 'ws') + '/channels/webchat/ws?session_id=' + sessionId);

    var streamingEl = null;

    ws.onmessage = function(e) {
      try {
        var data = JSON.parse(e.data);
        if (data.type === 'message') {
          addMsg(ui.msgs, data.content, 'bot');
        } else if (data.type === 'welcome') {
          addMsg(ui.msgs, data.content, 'bot');
        } else if (data.type === 'token') {
          if (!streamingEl) {
            streamingEl = document.createElement('div');
            streamingEl.className = 'rn-msg bot';
            ui.msgs.appendChild(streamingEl);
          }
          streamingEl.textContent += data.content;
          ui.msgs.scrollTop = ui.msgs.scrollHeight;
        } else if (data.type === 'done') {
          streamingEl = null;
        }
      } catch(_) {}
    };

    ws.onclose = function() {
      reconnectDelay = Math.min(reconnectDelay * 2, 30000);
      setTimeout(connect, reconnectDelay);
    };

    ws.onopen = function() { reconnectDelay = 1000; };
  }

  function sendMsg() {
    var text = ui.input.value.trim();
    if (!text || !ws || ws.readyState !== 1) return;
    ws.send(JSON.stringify({ type: 'message', content: text }));
    addMsg(ui.msgs, text, 'user');
    ui.input.value = '';
  }

  ui.send.onclick = sendMsg;
  ui.input.onkeydown = function(e) { if (e.key === 'Enter') sendMsg(); };
  connect();
})();
"#;
