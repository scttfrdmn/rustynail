use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use rustynail::channels::discord::DiscordChannel;
use rustynail::config::Config;
use rustynail::gateway::Gateway;
use tokio::signal;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rustynail",
    about = "RustyNail — high-performance personal AI assistant",
    version = env!("CARGO_PKG_VERSION")
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the RustyNail gateway (default when no subcommand is given)
    Start,

    /// Show the status of a running RustyNail instance
    Status {
        /// HTTP port the running instance is listening on
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },

    /// Print version and build information
    Version,

    /// Configuration subcommands
    Config(ConfigArgs),

    /// Generate shell completion scripts
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(clap::Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommands,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Validate configuration and print a summary (does not start the server)
    Check,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Start) {
        Commands::Start => cmd_start().await,
        Commands::Status { port } => cmd_status(port).await,
        Commands::Version => cmd_version(),
        Commands::Config(args) => match args.command {
            ConfigCommands::Check => cmd_config_check(),
        },
        Commands::Completions { shell } => cmd_completions(shell),
    }
}

// ── Subcommand implementations ────────────────────────────────────────────────

/// `rustynail start` — current default behavior.
async fn cmd_start() -> Result<()> {
    // Load configuration first (needed to decide whether to enable OTel)
    let config = Config::load()?;

    // Initialize tracing — optionally with an OTLP exporter
    let registry = tracing_subscriber::registry().with(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "rustynail=info,tower_http=info".into()),
    );

    if let Some(ref endpoint) = config.otel.endpoint {
        use opentelemetry_otlp::WithExportConfig;

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

    // Start the gateway (registers all channels, starts HTTP server,
    // spawns the internal message processing loop)
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

/// `rustynail status` — HTTP GET to running instance.
async fn cmd_status(port: u16) -> Result<()> {
    let url = format!("http://localhost:{}/status", port);
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| anyhow::anyhow!("Could not connect to RustyNail on port {}: {}", port, e))?;

    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "Status request failed: HTTP {}",
            resp.status()
        ));
    }

    let json: serde_json::Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

/// `rustynail version` — print version and build info.
fn cmd_version() -> Result<()> {
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    println!("repository: {}", env!("CARGO_PKG_REPOSITORY"));
    println!("license:    {}", env!("CARGO_PKG_LICENSE"));
    Ok(())
}

/// `rustynail config check` — load and validate config, then exit.
fn cmd_config_check() -> Result<()> {
    // Initialize minimal tracing so config errors are readable
    let _ = tracing_subscriber::fmt()
        .with_env_filter("rustynail=info")
        .try_init();

    let config = Config::load()?;

    println!("Configuration OK");
    println!("  HTTP port:        {}", config.gateway.http_port);
    println!("  WebSocket port:   {}", config.gateway.websocket_port);
    println!("  Log level:        {}", config.gateway.log_level);
    println!("  LLM provider:     {}", config.agents.llm_provider);
    println!("  LLM model:        {}", config.agents.llm_model);
    println!("  Memory backend:   {}", config.memory.backend);
    println!("  Tools enabled:    {}", config.tools.enabled);
    println!(
        "  Summarization:    {}",
        if config.memory.summarization.enabled {
            format!(
                "enabled (trigger_at={}, keep_recent={})",
                config.memory.summarization.trigger_at,
                config.memory.summarization.keep_recent
            )
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "  OTel endpoint:    {}",
        config.otel.endpoint.as_deref().unwrap_or("(disabled)")
    );
    println!(
        "  Dashboard auth:   {}",
        if config.dashboard.auth_password.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );

    let mut channels = Vec::new();
    if config.channels.discord.as_ref().is_some_and(|c| c.enabled) {
        channels.push("discord");
    }
    if config.channels.whatsapp.as_ref().is_some_and(|c| c.enabled) {
        channels.push("whatsapp");
    }
    if config.channels.telegram.as_ref().is_some_and(|c| c.enabled) {
        let mode = config
            .channels
            .telegram
            .as_ref()
            .map(|c| c.mode.as_str())
            .unwrap_or("webhook");
        if mode == "longpoll" {
            channels.push("telegram (long-poll)");
        } else {
            channels.push("telegram (webhook)");
        }
    }
    if config.channels.slack.as_ref().is_some_and(|c| c.enabled) {
        let mode = config
            .channels
            .slack
            .as_ref()
            .map(|c| c.mode.as_str())
            .unwrap_or("webhook");
        if mode == "socket" {
            channels.push("slack (socket mode)");
        } else {
            channels.push("slack (webhook)");
        }
    }
    if config.channels.sms.as_ref().is_some_and(|c| c.enabled) {
        channels.push("sms");
    }
    if config.channels.webchat.as_ref().is_some_and(|c| c.enabled) {
        channels.push("webchat");
    }
    if config.channels.email.as_ref().is_some_and(|c| c.enabled) {
        channels.push("email");
    }
    if channels.is_empty() {
        println!("  Channels:         (none configured)");
    } else {
        println!("  Channels:         {}", channels.join(", "));
    }

    Ok(())
}

/// `rustynail completions <shell>` — print shell completion script.
fn cmd_completions(shell: Shell) -> Result<()> {
    clap_complete::generate(shell, &mut Cli::command(), "rustynail", &mut std::io::stdout());
    Ok(())
}
