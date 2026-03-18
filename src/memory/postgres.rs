use crate::memory::MemoryStore;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{error, info};
use uuid::Uuid;

/// PostgreSQL-backed conversation history store.
///
/// All async sqlx operations run on a dedicated single-threaded tokio runtime
/// to satisfy the sync `MemoryStore` trait without blocking the main runtime.
pub struct PostgresStore {
    rt: Arc<tokio::runtime::Runtime>,
    pool: PgPool,
    max_history: usize,
}

impl PostgresStore {
    /// Connect to a PostgreSQL database at `url` and initialise the schema.
    pub fn new(url: &str, max_history: usize) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let pool = rt.block_on(async {
            let pool = PgPool::connect(url).await?;

            sqlx::query(
                "CREATE TABLE IF NOT EXISTS rustynail_messages (
                    id      TEXT PRIMARY KEY,
                    user_id TEXT NOT NULL,
                    content TEXT NOT NULL,
                    ts      BIGINT NOT NULL
                )",
            )
            .execute(&pool)
            .await?;

            sqlx::query(
                "CREATE INDEX IF NOT EXISTS idx_rn_messages_user_ts
                 ON rustynail_messages (user_id, ts)",
            )
            .execute(&pool)
            .await?;

            anyhow::Ok(pool)
        })?;

        info!("PostgreSQL memory store connected (url={})", url);
        Ok(Self {
            rt: Arc::new(rt),
            pool,
            max_history,
        })
    }
}

impl MemoryStore for PostgresStore {
    fn get_history(&self, user_id: &str) -> Vec<String> {
        let pool = self.pool.clone();
        let uid = user_id.to_string();
        let limit = self.max_history as i64;

        self.rt
            .block_on(async move {
                sqlx::query_as::<_, (String,)>(
                    "SELECT content FROM rustynail_messages
                     WHERE user_id = $1
                     ORDER BY ts ASC
                     LIMIT $2",
                )
                .bind(&uid)
                .bind(limit)
                .fetch_all(&pool)
                .await
            })
            .map(|rows| rows.into_iter().map(|(c,)| c).collect())
            .unwrap_or_else(|e| {
                error!("Postgres get_history error: {}", e);
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
                "INSERT INTO rustynail_messages (id, user_id, content, ts)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(&id)
            .bind(&uid)
            .bind(&message)
            .bind(ts)
            .execute(&pool)
            .await?;

            // Trim oldest rows beyond max_history
            sqlx::query(
                "DELETE FROM rustynail_messages
                 WHERE id IN (
                     SELECT id FROM rustynail_messages
                     WHERE user_id = $1
                     ORDER BY ts ASC
                     LIMIT GREATEST(0, (
                         SELECT COUNT(*) FROM rustynail_messages WHERE user_id = $1
                     ) - $2)
                 )",
            )
            .bind(&uid)
            .bind(max)
            .execute(&pool)
            .await?;

            anyhow::Ok(())
        }) {
            error!("Postgres add_message error: {}", e);
        }
    }

    fn clear_history(&self, user_id: &str) {
        let pool = self.pool.clone();
        let uid = user_id.to_string();

        if let Err(e) = self
            .rt
            .block_on(
                sqlx::query("DELETE FROM rustynail_messages WHERE user_id = $1")
                    .bind(&uid)
                    .execute(&pool),
            )
        {
            error!("Postgres clear_history error: {}", e);
        }
    }

    fn max_history(&self) -> usize {
        self.max_history
    }
}
