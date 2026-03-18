pub mod http;
pub mod user_prefs;

use crate::agents::AgentManager;
use crate::channels::Channel;
use crate::config::Config;
use crate::memory::{InMemoryStore, MemoryStore};
use crate::tools::ToolRegistry;
use crate::types::{GatewayEvent, Message};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info};

use agenkit::Tool;
use user_prefs::UserPreferences;

pub struct Gateway {
    config: Config,
    channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    memory: Arc<dyn MemoryStore>,
    agent_manager: Arc<AgentManager>,
    user_prefs: Arc<UserPreferences>,
    event_tx: broadcast::Sender<GatewayEvent>,
    _event_rx: broadcast::Receiver<GatewayEvent>,
    tasks: Vec<JoinHandle<()>>,
    /// Sender given to webhook-based channels / HTTP server for inbound messages
    message_tx: mpsc::UnboundedSender<Message>,
    message_rx: Option<mpsc::UnboundedReceiver<Message>>,
}

impl Gateway {
    pub fn new(config: Config) -> Self {
        let (event_tx, event_rx) = broadcast::channel(100);
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        let memory = Arc::new(InMemoryStore::new(config.agents.max_history));

        // Build tool registry from config
        let mut tool_registry = ToolRegistry::new();

        if config.tools.enabled {
            // Always register calculator
            let calc_tool = crate::tools::calculator::CalculatorTool;
            if let Err(e) = tool_registry.register(Arc::new(calc_tool)) {
                error!("Failed to register calculator tool: {}", e);
            }

            // Register filesystem tool if root is configured
            if let Some(ref fs_root) = config.tools.filesystem_root {
                let root = std::path::PathBuf::from(fs_root);
                let fs_tool = crate::tools::filesystem::FileSystemTool::new(root);
                if let Err(e) = tool_registry.register(Arc::new(fs_tool)) {
                    error!("Failed to register filesystem tool: {}", e);
                }
            }
        }

        let agent_manager = Arc::new(AgentManager::with_tools(
            config.agents.clone(),
            config.tools.clone(),
            tool_registry,
        ));

        Self {
            config,
            channels: Arc::new(RwLock::new(Vec::new())),
            memory,
            agent_manager,
            user_prefs: Arc::new(UserPreferences::new()),
            event_tx,
            _event_rx: event_rx,
            tasks: Vec::new(),
            message_tx,
            message_rx: Some(message_rx),
        }
    }

    /// Returns a sender for delivering inbound messages to this gateway.
    pub fn message_sender(&self) -> mpsc::UnboundedSender<Message> {
        self.message_tx.clone()
    }

    /// Returns a reference to the user preferences store.
    pub fn user_prefs(&self) -> Arc<UserPreferences> {
        self.user_prefs.clone()
    }

    /// Register a channel with the gateway.
    pub async fn register_channel(&mut self, channel: Box<dyn Channel>) {
        info!("Registering channel: {}", channel.name());
        let mut channels = self.channels.write().await;
        channels.push(channel);
    }

    /// Register a tool with the agent manager.
    pub async fn register_tool(&self, tool: Arc<dyn Tool>) -> Result<()> {
        self.agent_manager.register_tool(tool).await
    }

    /// Start the gateway and all registered channels.
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting gateway");

        // Add WhatsApp channel if enabled (gateway owns its lifecycle)
        if let Some(wa_config) = self.config.channels.whatsapp.clone() {
            if wa_config.enabled {
                info!("Setting up WhatsApp channel");
                let wa = crate::channels::whatsapp::WhatsAppChannel::new(
                    "whatsapp-main".to_string(),
                    wa_config,
                );
                self.register_channel(Box::new(wa)).await;
            }
        }

        // Pass WhatsApp webhook sender to HTTP only when WhatsApp is enabled
        let whatsapp_tx = self
            .config
            .channels
            .whatsapp
            .as_ref()
            .filter(|c| c.enabled)
            .map(|_| self.message_tx.clone());

        let whatsapp_verify_token = self
            .config
            .channels
            .whatsapp
            .as_ref()
            .map(|c| c.verify_token.clone())
            .unwrap_or_default();

        // Start HTTP server
        let http_port = self.config.gateway.http_port;
        let channels_clone = self.channels.clone();
        let agent_manager_clone = self.agent_manager.clone();
        let user_prefs_clone = self.user_prefs.clone();

        let http_task = tokio::spawn(async move {
            if let Err(e) = http::start_http_server(
                http_port,
                channels_clone,
                agent_manager_clone,
                whatsapp_tx,
                whatsapp_verify_token,
                user_prefs_clone,
            )
            .await
            {
                error!("HTTP server error: {}", e);
            }
        });
        self.tasks.push(http_task);

        // Start all channels
        {
            let mut channels = self.channels.write().await;
            for channel in channels.iter_mut() {
                info!("Starting channel: {}", channel.name());
                channel.start().await?;
            }
        }

        // Spawn internal message processing loop
        if let Some(mut rx) = self.message_rx.take() {
            let memory = self.memory.clone();
            let agent_manager = self.agent_manager.clone();
            let channels = self.channels.clone();
            let user_prefs = self.user_prefs.clone();

            let msg_task = tokio::spawn(async move {
                while let Some(message) = rx.recv().await {
                    if let Err(e) = handle_message_inner(
                        &memory,
                        &agent_manager,
                        &channels,
                        &user_prefs,
                        message,
                    )
                    .await
                    {
                        error!("Error handling message: {}", e);
                    }
                }
            });
            self.tasks.push(msg_task);
        }

        info!("Gateway started successfully");
        Ok(())
    }

    /// Stop the gateway and all channels.
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping gateway");

        let _ = self.event_tx.send(GatewayEvent::Shutdown);

        {
            let mut channels = self.channels.write().await;
            for channel in channels.iter_mut() {
                info!("Stopping channel: {}", channel.name());
                if let Err(e) = channel.stop().await {
                    error!("Error stopping channel {}: {}", channel.name(), e);
                }
            }
        }

        for task in self.tasks.drain(..) {
            task.abort();
        }

        info!("Gateway stopped");
        Ok(())
    }

    /// Handle an incoming message (kept for external callers / tests).
    pub async fn handle_message(&self, message: Message) -> Result<()> {
        handle_message_inner(
            &self.memory,
            &self.agent_manager,
            &self.channels,
            &self.user_prefs,
            message,
        )
        .await
    }

    pub fn event_sender(&self) -> broadcast::Sender<GatewayEvent> {
        self.event_tx.clone()
    }

    pub fn memory(&self) -> Arc<dyn MemoryStore> {
        self.memory.clone()
    }
}

/// Core message-handling logic shared between the internal loop and public method.
async fn handle_message_inner(
    memory: &Arc<dyn MemoryStore>,
    agent_manager: &Arc<AgentManager>,
    channels: &Arc<RwLock<Vec<Box<dyn Channel>>>>,
    user_prefs: &Arc<UserPreferences>,
    message: Message,
) -> Result<()> {
    info!(
        "Handling message from {} in channel {}",
        message.username, message.channel_id
    );

    // Resolve the channel to route the response to
    let response_channel_id = if let Some(ref preferred) = message.preferred_channel_id {
        preferred.clone()
    } else if let Some(pref) = user_prefs.get(&message.user_id).await {
        pref
    } else {
        message.channel_id.clone()
    };

    // Track in memory store
    memory.add_message(&message.user_id, format!("User: {}", message.content));

    // Process with agent
    let response_content = agent_manager
        .process_message(&message.user_id, &message.content)
        .await?;

    memory.add_message(
        &message.user_id,
        format!("Assistant: {}", response_content),
    );

    // Send response to the resolved channel
    let response = Message::new(
        response_channel_id.clone(),
        "assistant".to_string(),
        "RustyNail".to_string(),
        response_content,
    );

    let channels = channels.read().await;
    for channel in channels.iter() {
        if channel.id() == response_channel_id {
            channel.send_message(response).await?;
            return Ok(());
        }
    }

    error!(
        "No channel found with id '{}' to send response",
        response_channel_id
    );
    Ok(())
}
