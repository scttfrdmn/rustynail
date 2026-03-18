use crate::config::CronJobConfig;
use crate::types::Message;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJobStatus {
    pub name: String,
    pub schedule: String,
    pub channel_id: String,
    pub user_id: String,
    pub enabled: bool,
}

pub struct CronScheduler {
    jobs: Vec<CronJobConfig>,
    message_tx: mpsc::UnboundedSender<Message>,
    handles: Vec<JoinHandle<()>>,
}

impl CronScheduler {
    pub fn new(jobs: Vec<CronJobConfig>, message_tx: mpsc::UnboundedSender<Message>) -> Self {
        Self {
            jobs,
            message_tx,
            handles: Vec::new(),
        }
    }

    /// Parse a schedule string (e.g. "30s", "5m", "2h", "1d") into a Duration.
    fn parse_schedule(schedule: &str) -> Option<std::time::Duration> {
        let s = schedule.trim();
        if let Some(n) = s.strip_suffix('s') {
            n.parse::<u64>().ok().map(std::time::Duration::from_secs)
        } else if let Some(n) = s.strip_suffix('m') {
            n.parse::<u64>().ok().map(|v| std::time::Duration::from_secs(v * 60))
        } else if let Some(n) = s.strip_suffix('h') {
            n.parse::<u64>().ok().map(|v| std::time::Duration::from_secs(v * 3600))
        } else if let Some(n) = s.strip_suffix('d') {
            n.parse::<u64>().ok().map(|v| std::time::Duration::from_secs(v * 86400))
        } else {
            None
        }
    }

    /// Start all enabled jobs. Each job gets its own tokio task.
    pub fn start(&mut self) {
        let mut active = 0usize;
        for job in &self.jobs {
            if !job.enabled {
                continue;
            }
            let interval = match Self::parse_schedule(&job.schedule) {
                Some(d) => d,
                None => {
                    warn!(
                        "Cron job '{}': invalid schedule '{}', skipping",
                        job.name, job.schedule
                    );
                    continue;
                }
            };

            let tx = self.message_tx.clone();
            let channel_id = job.channel_id.clone();
            let user_id = job.user_id.clone();
            let message_text = job.message.clone();
            let name = job.name.clone();

            let handle = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    let msg = Message::new(
                        channel_id.clone(),
                        user_id.clone(),
                        "cron".to_string(),
                        message_text.clone(),
                    );
                    if tx.send(msg).is_err() {
                        // Gateway shut down; exit loop
                        break;
                    }
                    tracing::debug!("Cron job '{}' fired", name);
                }
            });

            self.handles.push(handle);
            active += 1;
        }

        if active > 0 {
            info!("Cron scheduler started ({} active jobs)", active);
        }
    }

    /// Abort all running job tasks.
    pub fn stop(&mut self) {
        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }

    /// Snapshot of all job statuses (for /cron/jobs endpoint).
    pub fn job_status(&self) -> Vec<CronJobStatus> {
        self.jobs
            .iter()
            .map(|j| CronJobStatus {
                name: j.name.clone(),
                schedule: j.schedule.clone(),
                channel_id: j.channel_id.clone(),
                user_id: j.user_id.clone(),
                enabled: j.enabled,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_schedule() {
        assert_eq!(
            CronScheduler::parse_schedule("30s"),
            Some(std::time::Duration::from_secs(30))
        );
        assert_eq!(
            CronScheduler::parse_schedule("5m"),
            Some(std::time::Duration::from_secs(300))
        );
        assert_eq!(
            CronScheduler::parse_schedule("2h"),
            Some(std::time::Duration::from_secs(7200))
        );
        assert_eq!(
            CronScheduler::parse_schedule("1d"),
            Some(std::time::Duration::from_secs(86400))
        );
        assert_eq!(CronScheduler::parse_schedule("24h"), Some(std::time::Duration::from_secs(86400)));
        assert_eq!(CronScheduler::parse_schedule("invalid"), None);
        assert_eq!(CronScheduler::parse_schedule(""), None);
    }
}
