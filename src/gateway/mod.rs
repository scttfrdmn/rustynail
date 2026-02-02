pub mod http;

use crate::agents::AgentManager;
use crate::channels::Channel;
use crate::config::Config;
use crate::memory::{InMemoryStore, MemoryStore};
use crate::types::{GatewayEvent, Message};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info};

pub struct Gateway {
    config: Config,
    channels: Arc<RwLock<Vec<Box<dyn Channel>>>>,
    memory: Arc<dyn MemoryStore>,
    agent_manager: Arc<AgentManager>,
    event_tx: broadcast::Sender<GatewayEvent>,
    _event_rx: broadcast::Receiver<GatewayEvent>,
    tasks: Vec<JoinHandle<()>>,
}

impl Gateway {
    pub fn new(config: Config) -> Self {
        let (event_tx, event_rx) = broadcast::channel(100);
        let memory = Arc::new(InMemoryStore::new(config.agents.max_history));
        let agent_manager = Arc::new(AgentManager::new(config.agents.clone()));

        Self {
            config,
            channels: Arc::new(RwLock::new(Vec::new())),
            memory,
            agent_manager,
            event_tx,
            _event_rx: event_rx,
            tasks: Vec::new(),
        }
    }

    /// Register a channel with the gateway
    pub async fn register_channel(&mut self, channel: Box<dyn Channel>) {
        info!("Registering channel: {}", channel.name());
        let mut channels = self.channels.write().await;
        channels.push(channel);
    }

    /// Start the gateway and all registered channels
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting gateway");

        // Start HTTP server in background
        let http_port = self.config.gateway.http_port;
        let channels_clone = self.channels.clone();
        let agent_manager_clone = self.agent_manager.clone();

        let http_task = tokio::spawn(async move {
            if let Err(e) = http::start_http_server(http_port, channels_clone, agent_manager_clone).await {
                error!("HTTP server error: {}", e);
            }
        });

        self.tasks.push(http_task);

        // Start all channels
        let mut channels = self.channels.write().await;
        for channel in channels.iter_mut() {
            info!("Starting channel: {}", channel.name());
            channel.start().await?;
        }

        info!("Gateway started successfully");
        Ok(())
    }

    /// Stop the gateway and all channels
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping gateway");

        // Send shutdown event
        let _ = self.event_tx.send(GatewayEvent::Shutdown);

        // Stop all channels
        let mut channels = self.channels.write().await;
        for channel in channels.iter_mut() {
            info!("Stopping channel: {}", channel.name());
            if let Err(e) = channel.stop().await {
                error!("Error stopping channel {}: {}", channel.name(), e);
            }
        }

        // Wait for all tasks to complete
        for task in self.tasks.drain(..) {
            task.abort();
        }

        info!("Gateway stopped");
        Ok(())
    }

    /// Handle an incoming message
    pub async fn handle_message(&self, message: Message) -> Result<()> {
        info!(
            "Handling message from {} in channel {}",
            message.username, message.channel_id
        );

        // Add user message to memory store for tracking
        self.memory
            .add_message(&message.user_id, format!("User: {}", message.content));

        // Process with Agenkit agent (maintains its own conversation history)
        let response_content = self
            .agent_manager
            .process_message(&message.user_id, &message.content)
            .await?;

        // Add assistant response to memory store
        self.memory
            .add_message(&message.user_id, format!("Assistant: {}", response_content));

        // Send response back through the channel
        let response = Message::new(
            message.channel_id.clone(),
            "assistant".to_string(),
            "RustyNail".to_string(),
            response_content,
        );

        // Find the channel and send the response
        let channels = self.channels.read().await;
        for channel in channels.iter() {
            if channel.id() == message.channel_id {
                channel.send_message(response).await?;
                break;
            }
        }

        Ok(())
    }

    /// Get the event sender for subscribing to gateway events
    pub fn event_sender(&self) -> broadcast::Sender<GatewayEvent> {
        self.event_tx.clone()
    }

    /// Get a handle to the memory store
    pub fn memory(&self) -> Arc<dyn MemoryStore> {
        self.memory.clone()
    }
}
