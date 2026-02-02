use anyhow::Result;
use rustynail::channels::discord::DiscordChannel;
use rustynail::config::Config;
use rustynail::gateway::Gateway;
use tokio::signal;
use tokio::sync::mpsc;
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

    info!("🦀🔨 Starting RustyNail - Rust Never Sleeps!");

    // Load configuration
    let config = Config::load()?;
    info!("Configuration loaded");

    // Create gateway
    let mut gateway = Gateway::new(config.clone());

    // Create message channel for Discord -> Gateway communication
    let (message_tx, mut message_rx) = mpsc::unbounded_channel();

    // Set up Discord channel if enabled
    if let Some(discord_config) = &config.channels.discord {
        if discord_config.enabled {
            info!("Setting up Discord channel");
            let discord = DiscordChannel::new(
                "discord-main".to_string(),
                discord_config.auth.token.clone(),
                message_tx,
            );
            gateway.register_channel(Box::new(discord)).await;
        }
    }

    // Start the gateway
    gateway.start().await?;
    info!("Gateway started successfully");

    // Spawn message handling task
    let gateway_clone = std::sync::Arc::new(tokio::sync::Mutex::new(gateway));
    let message_handler = {
        let gateway = gateway_clone.clone();
        tokio::spawn(async move {
            while let Some(message) = message_rx.recv().await {
                let gateway = gateway.lock().await;
                if let Err(e) = gateway.handle_message(message).await {
                    tracing::error!("Error handling message: {}", e);
                }
            }
        })
    };

    // Wait for shutdown signal
    info!("RustyNail is now running. Press Ctrl-C to shutdown.");
    match signal::ctrl_c().await {
        Ok(()) => {
            info!("Shutdown signal received");
        }
        Err(err) => {
            tracing::error!("Unable to listen for shutdown signal: {}", err);
        }
    }

    // Shutdown
    info!("Shutting down...");
    message_handler.abort();

    let mut gateway = gateway_clone.lock().await;
    gateway.stop().await?;

    info!("RustyNail shutdown complete");
    Ok(())
}
