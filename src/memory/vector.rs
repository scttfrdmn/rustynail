use crate::memory::MemoryStore;
use agenkit::memory::vector_memory::{EmbeddingProvider, InMemoryVectorStore, VectorMemory};
use agenkit::core::AgentError;
use chrono::{DateTime, Utc};
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

// ── Decay helper ──────────────────────────────────────────────────────────────

/// Exponential decay weight: `exp(-ln(2) / half_life * age_secs)`.
/// Returns 1.0 when `half_life_secs` is zero or negative (decay disabled).
fn recency_weight(half_life_secs: f64, age_secs: f64) -> f64 {
    if half_life_secs <= 0.0 {
        return 1.0;
    }
    (-(std::f64::consts::LN_2 / half_life_secs) * age_secs).exp()
}

// ── VectorMemoryStore ─────────────────────────────────────────────────────────

/// Vector-backed conversation history store using agenkit's `VectorMemory`.
///
/// Semantic search is used internally via `VectorMemory::retrieve`. A secondary
/// in-memory ring buffer satisfies `get_history()` (which needs ordered, recent
/// messages without a semantic query). When `decay_half_life_seconds > 0` messages
/// are returned sorted by recency weight (most recent first).
///
/// All async agenkit operations run on a dedicated tokio runtime.
pub struct VectorMemoryStore {
    rt: Arc<tokio::runtime::Runtime>,
    vector_memory: Arc<VectorMemory>,
    /// Secondary ring buffer: ordered recent messages with timestamps per user.
    ring: Arc<RwLock<HashMap<String, Vec<(String, DateTime<Utc>)>>>>,
    max_history: usize,
    /// Exponential decay half-life in seconds. 0 = no decay.
    decay_half_life_seconds: f64,
}

impl VectorMemoryStore {
    pub fn new(max_history: usize) -> anyhow::Result<Self> {
        Self::with_decay(max_history, 3600.0)
    }

    pub fn with_decay(max_history: usize, decay_half_life_seconds: f64) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let embeddings = Box::new(SimpleEmbeddingProvider);
        let store = Box::new(InMemoryVectorStore::new());
        let vector_memory = Arc::new(VectorMemory::new(embeddings, Some(store)));

        info!("Vector memory store initialised (in-process, simple embeddings, half_life={}s)", decay_half_life_seconds);
        Ok(Self {
            rt: Arc::new(rt),
            vector_memory,
            ring: Arc::new(RwLock::new(HashMap::new())),
            max_history,
            decay_half_life_seconds,
        })
    }
}

impl MemoryStore for VectorMemoryStore {
    fn get_history(&self, user_id: &str) -> Vec<String> {
        let ring = self.ring.read().unwrap();
        let entries = match ring.get(user_id) {
            Some(e) => e.clone(),
            None => return Vec::new(),
        };
        drop(ring);

        if self.decay_half_life_seconds <= 0.0 {
            // No decay: return in insertion order
            return entries.into_iter().map(|(msg, _)| msg).collect();
        }

        // Sort descending by recency weight (most recent first)
        let now = Utc::now();
        let mut weighted: Vec<(String, f64)> = entries
            .into_iter()
            .map(|(msg, ts)| {
                let age_secs = (now - ts).num_milliseconds().max(0) as f64 / 1000.0;
                let w = recency_weight(self.decay_half_life_seconds, age_secs);
                (msg, w)
            })
            .collect();

        weighted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        weighted.into_iter().map(|(msg, _)| msg).collect()
    }

    fn add_message(&self, user_id: &str, message: String) {
        // Update ring buffer with timestamp
        {
            let mut ring = self.ring.write().unwrap();
            let history = ring.entry(user_id.to_string()).or_default();
            history.push((message.clone(), Utc::now()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recency_weight_at_half_life() {
        let w = recency_weight(3600.0, 3600.0);
        // At exactly the half-life, weight should be ≈ 0.5
        assert!((w - 0.5).abs() < 1e-6, "expected ≈0.5 at half-life, got {}", w);
    }

    #[test]
    fn test_recency_weight_zero_half_life() {
        assert_eq!(recency_weight(0.0, 100.0), 1.0);
        assert_eq!(recency_weight(-1.0, 100.0), 1.0);
    }

    #[test]
    fn test_recency_weight_recent_higher() {
        let new_w = recency_weight(3600.0, 60.0);
        let old_w = recency_weight(3600.0, 7200.0);
        assert!(new_w > old_w, "recent messages should have higher weight");
    }

    #[test]
    fn test_get_history_sorted_by_recency() {
        let store = VectorMemoryStore::with_decay(10, 60.0).unwrap();

        // Add a message, wait a tiny bit, add another
        store.add_message("u1", "older".to_string());
        // Artificially inject an older timestamp
        {
            let mut ring = store.ring.write().unwrap();
            let entries = ring.get_mut("u1").unwrap();
            // Make the first entry 120 seconds old
            entries[0].1 = Utc::now() - chrono::Duration::seconds(120);
        }
        store.add_message("u1", "newer".to_string());

        let history = store.get_history("u1");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0], "newer", "most recent message should rank first");
        assert_eq!(history[1], "older");
    }
}
