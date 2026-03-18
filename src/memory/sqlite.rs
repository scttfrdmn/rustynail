use crate::memory::MemoryStore;
use sqlx::SqlitePool;
use std::sync::Arc;
use tracing::{error, info};
use uuid::Uuid;

/// SQLite-backed conversation history store.
///
/// All async sqlx operations are executed on a dedicated single-threaded tokio
/// runtime so that the sync `MemoryStore` trait methods can block safely without
/// interfering with the main tokio runtime.
pub struct SqliteStore {
    rt: Arc<tokio::runtime::Runtime>,
    pool: SqlitePool,
    max_history: usize,
}

impl SqliteStore {
    /// Open (or create) a SQLite database at `path` and initialise the schema.
    pub fn new(path: &str, max_history: usize) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let url = format!("sqlite://{}?mode=rwc", path);

        let pool = rt.block_on(async {
            let pool = SqlitePool::connect(&url).await?;

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS messages (
                    id      TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    content TEXT NOT NULL,
                    ts      INTEGER NOT NULL
                )",
            )
            .execute(&pool)
            .await?;

            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_messages_user_ts
                 ON messages (user_id, ts)",
            )
            .execute(&pool)
            .await?;

            anyhow::Ok(pool)
        })?;

        info!("SQLite memory store opened (path={})", path);
        Ok(Self {
            rt: Arc::new(rt),
            pool,
            max_history,
        })
    }
}

impl MemoryStore for SqliteStore {
    fn get_history(&self, user_id: &str) -> Vec<String> {
        let pool = self.pool.clone();
        let uid = user_id.to_string();
        let limit = self.max_history as i64;

        self.rt
            .block_on(async move {
                sqlx::query_as::<_, (String,)>(
                    "SELECT content FROM messages
                     WHERE user_id = ?
                     ORDER BY ts ASC
                     LIMIT ?",
                )
                .bind(&uid)
                .bind(limit)
                .fetch_all(&pool)
                .await
            })
            .map(|rows| rows.into_iter().map(|(c,)| c).collect())
            .unwrap_or_else(|e| {
                error!("SQLite get_history error: {}", e);
                Vec::new()
            })
    }

    fn add_message(&self, user_id: &str, message: String) {
        let pool = self.pool.clone();
        let uid = user_id.to_string();
        let max = self.max_history as i64;
        let id = Uuid::new_v4().to_string();
        let ts = chrono::Utc::now().timestamp_millis();

        if let Err(e) = self.rt.block_on(async move {
            sqlx::query(
                "INSERT INTO messages (id, user_id, content, ts)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&uid)
            .bind(&message)
            .bind(ts)
            .execute(&pool)
            .await?;

            // Trim oldest rows beyond max_history
            sqlx::query(
                "DELETE FROM messages
                 WHERE id IN (
                     SELECT id FROM messages
                     WHERE user_id = ?
                     ORDER BY ts ASC
                     LIMIT MAX(0, (SELECT COUNT(*) FROM messages WHERE user_id = ?) - ?)
                 )",
            )
            .bind(&uid)
            .bind(&uid)
            .bind(max)
            .execute(&pool)
            .await?;

            anyhow::Ok(())
        }) {
            error!("SQLite add_message error: {}", e);
        }
    }

    fn clear_history(&self, user_id: &str) {
        let pool = self.pool.clone();
        let uid = user_id.to_string();

        if let Err(e) = self
            .rt
            .block_on(sqlx::query("DELETE FROM messages WHERE user_id = ?")
                .bind(&uid)
                .execute(&pool))
        {
            error!("SQLite clear_history error: {}", e);
        }
    }

    fn max_history(&self) -> usize {
        self.max_history
    }
}
