use anyhow::Result;
use rustynail::channels::discord::DiscordChannel;
use rustynail::config::Config;
use rustynail::gateway::Gateway;
use tokio::signal;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rustynail=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting RustyNail - Rust Never Sleeps!");

    // Load configuration
    let config = Config::load()?;
    info!("Configuration loaded");

    // Create gateway (owns its internal message channel and tool registry)
    let mut gateway = Gateway::new(config.clone());

    // Set up Discord channel if enabled
    if let Some(discord_config) = &config.channels.discord {
        if discord_config.enabled {
            info!("Setting up Discord channel");
            let discord = DiscordChannel::new(
                "discord-main".to_string(),
                discord_config.auth.token.clone(),
                gateway.message_sender(),
            );
            gateway.register_channel(Box::new(discord)).await;
        }
    }

    // Start the gateway (registers WhatsApp if configured, starts HTTP server
    // and spawns the internal message processing loop)
    gateway.start().await?;
    info!("Gateway started successfully");
    info!("RustyNail is now running. Press Ctrl-C to shutdown.");

    // Wait for shutdown signal
    match signal::ctrl_c().await {
        Ok(()) => info!("Shutdown signal received"),
        Err(err) => tracing::error!("Unable to listen for shutdown signal: {}", err),
    }

    info!("Shutting down...");
    gateway.stop().await?;
    info!("RustyNail shutdown complete");
    Ok(())
}
