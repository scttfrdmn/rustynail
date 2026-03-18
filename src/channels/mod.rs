pub mod channel;              // Trait definition
pub mod discord;              // Discord via serenity
pub mod email;                // Email (IMAP/SMTP)
pub mod slack;                // Slack webhook
pub mod slack_socketmode;     // Slack Socket Mode
pub mod sms;                  // Twilio SMS
pub mod teams;                // Microsoft Teams (Bot Framework)
pub mod telegram;             // Telegram webhook
pub mod telegram_longpoll;    // Telegram long-poll
pub mod testchan;             // Zero-credential test channel
pub mod webhook;              // Generic webhook
pub mod webchat;              // Web chat widget
pub mod whatsapp;             // WhatsApp Graph API

pub use channel::Channel;     // Re-export trait
