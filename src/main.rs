use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use rustynail::channels::discord::DiscordChannel;
use rustynail::config::Config;
use rustynail::gateway::Gateway;
use std::sync::Arc;
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

    /// MCP (Model Context Protocol) subcommands
    Mcp(McpArgs),
}

#[derive(clap::Args)]
struct McpArgs {
    #[command(subcommand)]
    command: McpCommands,
}

#[derive(Subcommand)]
enum McpCommands {
    /// Expose RustyNail's registered tools as an MCP server over stdio.
    ///
    /// Pipe this into Claude Code or any MCP-compatible client:
    ///   rustynail mcp serve
    Serve,
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
    /// Run preflight validation checks and exit 0 (OK) or 1 (failed)
    Validate,
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
            ConfigCommands::Validate => cmd_config_validate(),
        },
        Commands::Completions { shell } => cmd_completions(shell),
        Commands::Mcp(args) => match args.command {
            McpCommands::Serve => cmd_mcp_serve().await,
        },
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

    // Spawn SIGHUP hot-reload handler (Unix only)
    #[cfg(unix)]
    {
        let hot = gateway.hot_config_handle();
        tokio::spawn(async move {
            use signal::unix::{signal as unix_signal, SignalKind};
            let mut sighup = match unix_signal(SignalKind::hangup()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to register SIGHUP handler: {}", e);
                    return;
                }
            };
            loop {
                sighup.recv().await;
                match rustynail::config::Config::load() {
                    Ok(new_cfg) => {
                        let changed = hot.write().await.apply(&new_cfg);
                        if changed.is_empty() {
                            info!("SIGHUP: config reloaded (no hot-reloadable changes)");
                        } else {
                            info!("Config reloaded. Changed fields: {:?}", changed);
                        }
                    }
                    Err(e) => tracing::error!("Hot-reload config parse failed: {}", e),
                }
            }
        });
    }

    // Wait for shutdown signal
    match signal::ctrl_c().await {
        Ok(()) => info!("Shutdown signal received"),
        Err(err) => tracing::error!("Unable to listen for shutdown signal: {}", err),
    }

    info!("Shutting down...");
    let shutdown_timeout = std::time::Duration::from_secs(config.gateway.shutdown_timeout_seconds);
    match tokio::time::timeout(shutdown_timeout, gateway.stop()).await {
        Ok(Ok(())) => info!("Gateway stopped cleanly"),
        Ok(Err(e)) => tracing::error!("Gateway stop error: {}", e),
        Err(_) => tracing::warn!(
            "Gateway stop timed out after {}s",
            config.gateway.shutdown_timeout_seconds
        ),
    }

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
                config.memory.summarization.trigger_at, config.memory.summarization.keep_recent
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
    if config.channels.teams.as_ref().is_some_and(|c| c.enabled) {
        channels.push("teams");
    }
    if channels.is_empty() {
        println!("  Channels:         (none configured)");
    } else {
        println!("  Channels:         {}", channels.join(", "));
    }

    println!(
        "  Gateway auth:     {}",
        if config
            .gateway
            .api_token
            .as_deref()
            .map(|t| !t.is_empty())
            .unwrap_or(false)
        {
            "enabled"
        } else {
            "disabled"
        }
    );

    println!(
        "  Skills:           {}",
        if config.skills.enabled {
            let mut registry = rustynail::skills::SkillRegistry::new();
            let n = registry.discover_skills(&config.skills.paths);
            format!(
                "enabled ({} paths, {} skills loaded)",
                config.skills.paths.len(),
                n
            )
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "  WS origins:       {}",
        if config.gateway.allowed_ws_origins.is_empty() {
            "allow all".to_string()
        } else {
            config.gateway.allowed_ws_origins.join(", ")
        }
    );
    println!(
        "  Shutdown timeout: {}s",
        config.gateway.shutdown_timeout_seconds
    );
    println!("  Cron jobs:        {}", config.cron.jobs.len());
    println!(
        "  PDF tool:         {}",
        if config.tools.pdf_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  Image tool:       {}",
        if config.tools.image_enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    // Phrased as the effect on a run, not as the value of `verification.enabled`:
    // "verified" and "every run refused" are both `enabled: true`, and a summary
    // that reported the setting would print the same word for both. Whether a
    // verifier exists is asked of a real gate rather than assumed, so this line
    // stops saying "refused" on its own once #103 installs one.
    println!(
        "  Quarry:           {}",
        if !config.quarry.enabled {
            "disabled"
        } else if !config.quarry.verification.enabled {
            "enabled (UNVERIFIED — signature checks off)"
        } else if rustynail::quarry::verify::SpawnGate::new(
            config.quarry.verification.clone(),
            config.quarry.run_record_dir.clone(),
            Some(config.gateway.http_port),
        )
        .has_verifier()
        {
            "enabled (signature verification on)"
        } else {
            "enabled (every run refused: no verifier — see config validate)"
        }
    );

    Ok(())
}

/// `rustynail config validate` — preflight checks; exits 0 (all pass) or 1 (any fail).
fn cmd_config_validate() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("rustynail=warn")
        .try_init();

    let mut failures = 0usize;

    // ── Check 1: config loads ─────────────────────────────────────────────────
    let config = match Config::load() {
        Ok(cfg) => {
            let source = std::env::var("CONFIG_FILE").unwrap_or_else(|_| "(env vars)".to_string());
            println!("[✓] Config loaded ({})", source);
            cfg
        }
        Err(e) => {
            println!("[✗] Config load failed: {}", e);
            failures += 1;
            // Cannot continue without config
            println!("\n{} check(s) failed.", failures);
            std::process::exit(1);
        }
    };

    // ── Check 2: API key present ──────────────────────────────────────────────
    if config.agents.api_key.trim().is_empty() {
        println!("[✗] API key missing (agents.api_key / ANTHROPIC_API_KEY)");
        failures += 1;
    } else {
        println!("[✓] API key present ({})", config.agents.llm_provider);
    }

    // ── Check 3: memory backend dependencies ─────────────────────────────────
    match config.memory.backend.as_str() {
        "redis" => {
            if config
                .memory
                .redis_url
                .as_deref()
                .map(|u| u.is_empty())
                .unwrap_or(true)
            {
                println!("[✗] Memory backend is redis but memory.redis_url is not set");
                failures += 1;
            } else {
                println!("[✓] Memory backend: redis (url set)");
            }
        }
        "sqlite" => {
            if config
                .memory
                .sqlite_path
                .as_deref()
                .map(|p| p.is_empty())
                .unwrap_or(true)
            {
                println!("[✗] Memory backend is sqlite but memory.sqlite_path is not set");
                failures += 1;
            } else {
                println!("[✓] Memory backend: sqlite (path set)");
            }
        }
        "postgres" => {
            if config
                .memory
                .postgres_url
                .as_deref()
                .map(|u| u.is_empty())
                .unwrap_or(true)
            {
                println!("[✗] Memory backend is postgres but memory.postgres_url is not set");
                failures += 1;
            } else {
                println!("[✓] Memory backend: postgres (url set)");
            }
        }
        backend => {
            println!("[✓] Memory backend: {}", backend);
        }
    }

    // ── Check 4: quarry binary verification ───────────────────────────────────
    //
    // Reports the *effect* of the configuration, not the configuration. An operator
    // reading "enabled: true" would reasonably conclude runs are verified; with
    // #103's mechanism unimplemented they are refused instead, and the difference
    // between "verified" and "refused" is the one this check exists to state.
    failures += check_quarry_verification(&config);

    // ── Summary ───────────────────────────────────────────────────────────────
    if failures == 0 {
        println!("[✓] All checks passed.");
        Ok(())
    } else {
        println!("\n{} check(s) failed.", failures);
        std::process::exit(1);
    }
}

/// Report what will actually happen to a quarry spawn, and count the failures.
///
/// Deliberately not a `[✓]`/`[✗]` pair. There are four distinct states and three of
/// them are neither pass nor fail:
///
/// - quarry disabled — nothing to verify, and no reason to nag
/// - verification on with a mechanism — runs are verified: pass
/// - verification on with **no** mechanism — every run is refused: fail, because a
///   gateway that will refuse every quarry run should not report a clean preflight
/// - verification off — runs happen and are unverified: a warning that is *not* a
///   failure, since it is a deliberate development setting, but never silent
fn check_quarry_verification(config: &Config) -> usize {
    let v = &config.quarry.verification;

    if !config.quarry.enabled {
        println!("[✓] Quarry: disabled (no binary is spawned, nothing to verify)");
        return 0;
    }

    if !v.enabled {
        // Not counted as a failure — `enabled: false` is a choice an operator made.
        // Stated in terms of consequence rather than setting, because "verification
        // disabled" understates it: the manifest is still checked, the provenance is
        // not, and only the second half is what an unsigned local build gives up.
        println!(
            "[!] Quarry: signature verification DISABLED \
             (quarry.verification.enabled = false)"
        );
        println!("      Runs will execute unverified. The capability manifest is still");
        println!("      checked, but from an unsigned sidecar that proves nothing about");
        println!("      provenance. Development only — do not ship this.");
        return 0;
    }

    // Verification is on. Whether it can be satisfied is the question.
    let mut missing: Vec<&str> = Vec::new();
    if v.expected_identity.trim().is_empty() {
        missing.push("quarry.verification.expected_identity");
    }
    if v.expected_issuer.trim().is_empty() {
        missing.push("quarry.verification.expected_issuer");
    }

    // The mechanism (#103) is not implemented, so no verifier can be installed. Asked
    // of a real gate rather than hardcoded, so that this check starts reporting
    // success on its own the day #103 lands — a preflight that had to be edited
    // separately is a preflight that goes stale.
    let gate = rustynail::quarry::verify::SpawnGate::new(
        v.clone(),
        config.quarry.run_record_dir.clone(),
        Some(config.gateway.http_port),
    );

    if !gate.has_verifier() {
        println!("[✗] Quarry: verification is enabled but no verifier is installed");
        println!("      Every quarry run will be REFUSED (mechanism_unavailable).");
        println!("      The cosign mechanism is tracked as #103 and is not implemented");
        println!("      yet. Until it lands, either leave quarry disabled or set");
        println!("      quarry.verification.enabled: false to run unverified.");
        return 1;
    }

    if !missing.is_empty() {
        // An unconstrained check is worse than none, because it succeeds. So an
        // absent identity is a failure even though a verifier is present.
        println!(
            "[✗] Quarry: verification is enabled but {} is not set",
            missing.join(" and ")
        );
        println!("      Every quarry run will be REFUSED: verifying without an expected");
        println!("      identity would accept any signature at all.");
        return 1;
    }

    println!("[✓] Quarry: signature verification enabled");
    println!("      identity: {}", v.expected_identity);
    println!("      issuer:   {}", v.expected_issuer);
    println!("      cosign:   {}", v.cosign_path);
    if v.allow_writable_binary {
        // Not a failure — an explicit opt-in — but the risk is restated, because the
        // operator who accepted it is often not the one reading this output.
        println!(
            "[!] Quarry: allow_writable_binary is set, so a writable binary or \
             directory will not stop a spawn"
        );
    }
    0
}

/// `rustynail completions <shell>` — print shell completion script.
fn cmd_completions(shell: Shell) -> Result<()> {
    clap_complete::generate(
        shell,
        &mut Cli::command(),
        "rustynail",
        &mut std::io::stdout(),
    );
    Ok(())
}

/// `rustynail mcp serve` — expose RustyNail tools as an MCP server over stdio.
async fn cmd_mcp_serve() -> Result<()> {
    use agenkit::protocols::{McpServer, McpServerConfig};
    use rustynail::tools::{calculator::CalculatorTool, formatter::FormatterTool};

    // Log to stderr so stdio transport stays clean
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("rustynail=warn")
        .try_init();

    let config = Config::load()?;

    let mut tools: Vec<Arc<dyn agenkit::Tool>> =
        vec![Arc::new(CalculatorTool), Arc::new(FormatterTool)];

    if config.tools.enabled {
        if let Some(ref fs_root) = config.tools.filesystem_root {
            tools.push(Arc::new(rustynail::tools::filesystem::FileSystemTool::new(
                std::path::PathBuf::from(fs_root),
            )));
        }
        if let Some(ref api_key) = config.tools.web_search_api_key {
            tools.push(Arc::new(rustynail::tools::web_search::WebSearchTool::new(
                api_key.clone(),
            )));
        }
        tools.push(Arc::new(
            rustynail::tools::calendar::CalendarTool::with_default_dir(),
        ));
    }

    let server = McpServer::new(McpServerConfig {
        name: "rustynail".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        tools,
    });

    server
        .serve_stdio()
        .await
        .map_err(|e| anyhow::anyhow!("MCP serve error: {}", e))
}
