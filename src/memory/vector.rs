use crate::memory::MemoryStore;
use agenkit::memory::vector_memory::{EmbeddingProvider, InMemoryVectorStore, VectorMemory};
use agenkit::core::AgentError;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{error, info};

// ── Simple deterministic embedding provider ───────────────────────────────────

/// A deterministic character n-gram frequency embedding.
/// Produces a 64-dimensional vector from text — not semantically meaningful
/// but useful for testing and basic similarity search.
struct SimpleEmbeddingProvider;

#[async_trait::async_trait]
impl EmbeddingProvider for SimpleEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f64>, AgentError> {
        const DIM: usize = 64;
        let mut vec = vec![0.0f64; DIM];

        // Populate via character bigram frequencies
        let chars: Vec<char> = text.to_lowercase().chars().collect();
        for window in chars.windows(2) {
            let a = window[0] as u8;
            let b = window[1] as u8;
            let idx = ((a as usize).wrapping_mul(31).wrapping_add(b as usize)) % DIM;
            vec[idx] += 1.0;
        }

        // L2 normalise
        let norm: f64 = vec.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm > 0.0 {
            vec.iter_mut().for_each(|x| *x /= norm);
        }

        Ok(vec)
    }

    fn dimension(&self) -> usize {
        64
    }
}

// ── VectorMemoryStore ─────────────────────────────────────────────────────────

/// Vector-backed conversation history store using agenkit's `VectorMemory`.
///
/// Semantic search is used internally via `VectorMemory::retrieve`. A secondary
/// in-memory ring buffer satisfies `get_history()` (which needs ordered, recent
/// messages without a semantic query).
///
/// All async agenkit operations run on a dedicated tokio runtime.
pub struct VectorMemoryStore {
    rt: Arc<tokio::runtime::Runtime>,
    vector_memory: Arc<VectorMemory>,
    /// Secondary ring buffer: ordered recent messages per user.
    ring: Arc<RwLock<HashMap<String, Vec<String>>>>,
    max_history: usize,
}

impl VectorMemoryStore {
    pub fn new(max_history: usize) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let embeddings = Box::new(SimpleEmbeddingProvider);
        let store = Box::new(InMemoryVectorStore::new());
        let vector_memory = Arc::new(VectorMemory::new(embeddings, Some(store)));

        info!("Vector memory store initialised (in-process, simple embeddings)");
        Ok(Self {
            rt: Arc::new(rt),
            vector_memory,
            ring: Arc::new(RwLock::new(HashMap::new())),
            max_history,
        })
    }
}

impl MemoryStore for VectorMemoryStore {
    fn get_history(&self, user_id: &str) -> Vec<String> {
        let ring = self.ring.read().unwrap();
        ring.get(user_id).cloned().unwrap_or_default()
    }

    fn add_message(&self, user_id: &str, message: String) {
        // Update ring buffer
        {
            let mut ring = self.ring.write().unwrap();
            let history = ring.entry(user_id.to_string()).or_default();
            history.push(message.clone());
            if history.len() > self.max_history {
                *history = history.split_off(history.len() - self.max_history);
            }
        }

        // Store embedding in VectorMemory (fire-and-forget via dedicated rt)
        let vm = self.vector_memory.clone();
        let uid = user_id.to_string();
        let msg = message.clone();
        if let Err(e) = self.rt.block_on(async move {
            let agenkit_msg = agenkit::core::Message::with_text("user", &msg);
            vm.store(&uid, agenkit_msg, None).await.map_err(|e| anyhow::anyhow!("{}", e))
        }) {
            error!("VectorMemory store error: {}", e);
        }
    }

    fn clear_history(&self, user_id: &str) {
        {
            let mut ring = self.ring.write().unwrap();
            ring.remove(user_id);
        }

        let vm = self.vector_memory.clone();
        let uid = user_id.to_string();
        if let Err(e) = self
            .rt
            .block_on(async move { vm.clear(&uid).await.map_err(|e| anyhow::anyhow!("{}", e)) })
        {
            error!("VectorMemory clear error: {}", e);
        }
    }

    fn max_history(&self) -> usize {
        self.max_history
    }
}
