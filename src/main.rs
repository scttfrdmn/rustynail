use anyhow::Result;
use rustynail::channels::discord::DiscordChannel;
use rustynail::config::Config;
use rustynail::gateway::Gateway;
use tokio::signal;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration first (needed to decide whether to enable OTel)
    let config = Config::load()?;

    // Initialize tracing — optionally with an OTLP exporter
    let registry = tracing_subscriber::registry().with(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "rustynail=info,tower_http=info".into()),
    );

    if let Some(ref endpoint) = config.otel.endpoint {
        use opentelemetry_otlp::WithExportConfig;

        // install_batch returns sdk::trace::Tracer which implements PreSampledTracer
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(endpoint),
            )
            .with_trace_config(opentelemetry_sdk::trace::Config::default().with_resource(
                opentelemetry_sdk::Resource::new(vec![opentelemetry::KeyValue::new(
                    "service.name",
                    config.otel.service_name.clone(),
                )]),
            ))
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .map_err(|e| anyhow::anyhow!("OTel pipeline error: {}", e))?;

        registry
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .with(tracing_subscriber::fmt::layer())
            .init();

        info!("OpenTelemetry tracing enabled (endpoint={})", endpoint);
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }

    info!("Starting RustyNail - Rust Never Sleeps!");
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

    // Start the gateway (registers WhatsApp/Telegram/Slack if configured, starts HTTP server
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

    // Flush OTel spans if exporter was configured
    if config.otel.endpoint.is_some() {
        opentelemetry::global::shutdown_tracer_provider();
    }

    info!("RustyNail shutdown complete");
    Ok(())
}
