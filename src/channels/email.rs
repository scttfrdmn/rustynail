use crate::channels::Channel;
use crate::config::EmailConfig;
use crate::types::{ChannelHealth, Message};
use anyhow::Result;
use async_trait::async_trait;
use lettre::message::{header, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message as EmailMessage, Tokio1Executor};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

const NAME: &str = "email";

pub struct EmailChannel {
    id: String,
    config: EmailConfig,
    health: Arc<RwLock<ChannelHealth>>,
    message_tx: mpsc::UnboundedSender<Message>,
    poll_task: Option<JoinHandle<()>>,
}

impl EmailChannel {
    pub fn new(
        id: String,
        config: EmailConfig,
        message_tx: mpsc::UnboundedSender<Message>,
    ) -> Self {
        Self {
            id,
            config,
            health: Arc::new(RwLock::new(ChannelHealth::Unhealthy {
                reason: "not started".to_string(),
            })),
            message_tx,
            poll_task: None,
        }
    }
}

#[async_trait]
impl Channel for EmailChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        NAME
    }

    async fn start(&mut self) -> Result<()> {
        info!("Starting email channel (IMAP polling mode)");

        let config = self.config.clone();
        let tx = self.message_tx.clone();
        let channel_id = self.id.clone();
        let health = self.health.clone();

        *health.write().await = ChannelHealth::Healthy;

        let task = tokio::spawn(async move {
            imap_poll_loop(config, tx, channel_id, health).await;
        });

        self.poll_task = Some(task);
        info!("Email channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("Stopping email channel");
        if let Some(task) = self.poll_task.take() {
            task.abort();
        }
        *self.health.write().await = ChannelHealth::Unhealthy {
            reason: "stopped".to_string(),
        };
        Ok(())
    }

    async fn send_message(&self, message: Message) -> Result<()> {
        let smtp_config = &self.config.smtp;

        // Determine recipient: use channel_id if it looks like an email, else message.user_id
        let to_addr = if message.user_id.contains('@') {
            message.user_id.clone()
        } else {
            warn!("EmailChannel: user_id '{}' is not an email address", message.user_id);
            return Ok(());
        };

        let email = EmailMessage::builder()
            .from(smtp_config.from_address.parse().map_err(|e| {
                anyhow::anyhow!("invalid from address '{}': {}", smtp_config.from_address, e)
            })?)
            .to(to_addr.parse().map_err(|e| {
                anyhow::anyhow!("invalid to address '{}': {}", to_addr, e)
            })?)
            .subject("RustyNail")
            .multipart(
                MultiPart::alternative().singlepart(
                    SinglePart::builder()
                        .header(header::ContentType::TEXT_PLAIN)
                        .body(message.content.clone()),
                ),
            )
            .map_err(|e| anyhow::anyhow!("email build error: {}", e))?;

        let creds = Credentials::new(
            smtp_config.username.clone(),
            smtp_config.password.clone(),
        );

        let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp_config.host)
            .map_err(|e| anyhow::anyhow!("SMTP relay error: {}", e))?
            .port(smtp_config.port)
            .credentials(creds)
            .build();

        mailer
            .send(email)
            .await
            .map_err(|e| anyhow::anyhow!("SMTP send error: {}", e))?;

        info!("Email sent to {}", to_addr);
        Ok(())
    }

    fn health(&self) -> ChannelHealth {
        self.health.blocking_read().clone()
    }

    fn is_running(&self) -> bool {
        matches!(self.health.blocking_read().clone(), ChannelHealth::Healthy)
    }
}

// ── IMAP polling background task ──────────────────────────────────────────────

/// Long-poll IMAP loop. Checks for new messages every 30 seconds using
/// `spawn_blocking` to run the synchronous `imap` crate.
async fn imap_poll_loop(
    config: EmailConfig,
    tx: mpsc::UnboundedSender<Message>,
    channel_id: String,
    health: Arc<RwLock<ChannelHealth>>,
) {
    let mut last_uid: u32 = 0;
    let poll_interval = std::time::Duration::from_secs(30);

    info!("Email IMAP poll loop started ({}:{})", config.imap.host, config.imap.port);

    loop {
        let host = config.imap.host.clone();
        let port = config.imap.port;
        let username = config.imap.username.clone();
        let password = config.imap.password.clone();
        let inbox = config.imap.inbox.clone();
        let cid = channel_id.clone();
        let current_last_uid = last_uid;

        let result = tokio::task::spawn_blocking(move || {
            fetch_new_emails(&host, port, &username, &password, &inbox, &cid, current_last_uid)
        })
        .await;

        match result {
            Ok(Ok((messages, new_last_uid))) => {
                for msg in messages {
                    if let Err(e) = tx.send(msg) {
                        error!("Email: failed to enqueue message: {}", e);
                    }
                }
                if new_last_uid > last_uid {
                    last_uid = new_last_uid;
                }
            }
            Ok(Err(e)) => {
                warn!("Email IMAP fetch error: {}", e);
                *health.write().await = ChannelHealth::Unhealthy {
                    reason: format!("IMAP error: {}", e),
                };
                // Back off briefly, then try again
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                *health.write().await = ChannelHealth::Healthy;
            }
            Err(e) => {
                error!("Email IMAP task panicked: {}", e);
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Synchronous IMAP fetch using the `imap` crate (called via spawn_blocking).
///
/// Returns a list of new messages and the highest UID seen.
fn fetch_new_emails(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    inbox: &str,
    channel_id: &str,
    last_uid: u32,
) -> Result<(Vec<Message>, u32)> {
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| anyhow::anyhow!("TLS error: {}", e))?;

    let client = imap::connect((host, port), host, &tls)
        .map_err(|e| anyhow::anyhow!("IMAP connect error: {}", e))?;

    let mut session = client
        .login(username, password)
        .map_err(|(e, _)| anyhow::anyhow!("IMAP login error: {}", e))?;

    session
        .select(inbox)
        .map_err(|e| anyhow::anyhow!("IMAP select error: {}", e))?;

    // Search for messages with UID > last_uid
    let uid_set = format!("{}:*", last_uid + 1);
    let uids = session
        .uid_search(format!("UID {}", uid_set))
        .map_err(|e| anyhow::anyhow!("IMAP search error: {}", e))?;

    if uids.is_empty() {
        let _ = session.logout();
        return Ok((Vec::new(), last_uid));
    }

    let uid_list: Vec<String> = uids.iter().map(|u| u.to_string()).collect();
    let fetch_range = uid_list.join(",");

    let messages_raw = session
        .uid_fetch(&fetch_range, "(RFC822 UID)")
        .map_err(|e| anyhow::anyhow!("IMAP fetch error: {}", e))?;

    let mut messages = Vec::new();
    let mut max_uid = last_uid;

    for raw in messages_raw.iter() {
        let uid = raw.uid.unwrap_or(0);
        if uid > max_uid {
            max_uid = uid;
        }
        if uid <= last_uid {
            continue;
        }

        if let Some(body) = raw.body() {
            if let Some(msg) = parse_email(channel_id, body) {
                messages.push(msg);
            }
        }
    }

    let _ = session.logout();
    Ok((messages, max_uid))
}

/// Parse a raw RFC822 email into a [`Message`].
fn parse_email(channel_id: &str, raw: &[u8]) -> Option<Message> {
    let raw_str = String::from_utf8_lossy(raw);

    // Extract From: header
    let from = raw_str
        .lines()
        .find(|l| l.to_lowercase().starts_with("from:"))?
        .trim_start_matches(|c: char| c.is_alphabetic() || c == ':')
        .trim()
        .to_string();

    // Extract Subject: header (used as preview, not required)
    let _subject = raw_str
        .lines()
        .find(|l| l.to_lowercase().starts_with("subject:"))
        .map(|l| l[8..].trim().to_string())
        .unwrap_or_default();

    // Body is everything after the first blank line
    let body = raw_str
        .split_once("\r\n\r\n")
        .or_else(|| raw_str.split_once("\n\n"))
        .map(|(_, b)| strip_quoted_text(b.trim()))
        .unwrap_or_default();

    if body.is_empty() {
        return None;
    }

    // Use the From address as user_id
    let user_id = extract_email_address(&from).unwrap_or_else(|| from.clone());

    Some(Message::new(
        channel_id.to_string(),
        user_id.clone(),
        user_id,
        body,
    ))
}

/// Extract `user@example.com` from `"Name <user@example.com>"` or bare address.
fn extract_email_address(from: &str) -> Option<String> {
    if let Some(start) = from.find('<') {
        if let Some(end) = from.find('>') {
            return Some(from[start + 1..end].to_string());
        }
    }
    // bare address
    let addr = from.trim();
    if addr.contains('@') {
        Some(addr.to_string())
    } else {
        None
    }
}

/// Remove quoted reply text (lines starting with `>`).
fn strip_quoted_text(body: &str) -> String {
    body.lines()
        .filter(|l| !l.starts_with('>'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}
