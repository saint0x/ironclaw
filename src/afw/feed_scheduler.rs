//! FeedScheduler — manages scheduled execution of feeds.
//!
//! Runs independently from the CronService. On startup, loads all active feeds
//! from the registry and schedules them. Hooks handle live updates.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::afw::feed_executor::FeedExecutor;
use crate::afw::schedule::schedule_to_interval_secs;
use crate::aria::feed_registry::{AriaFeed, FeedRegistry};
use crate::aria::registry::Registry;
use crate::aria::types::FeedItem;

/// Minimum refresh interval (60 seconds) to prevent busy-spinning.
const MIN_INTERVAL_SECS: u64 = 60;

/// A scheduled feed with its timer handle.
struct ScheduledFeed {
    _feed_id: Uuid,
    _interval: Duration,
    handle: Option<tokio::task::JoinHandle<()>>,
}

/// Manages the scheduling and execution of feeds.
pub struct FeedScheduler {
    executor: Arc<FeedExecutor>,
    feed_registry: Arc<FeedRegistry>,
    jobs: Arc<RwLock<HashMap<Uuid, ScheduledFeed>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl FeedScheduler {
    pub fn new(executor: FeedExecutor, feed_registry: Arc<FeedRegistry>) -> Self {
        Self {
            executor: Arc::new(executor),
            feed_registry,
            jobs: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Start the scheduler — load all active feeds and begin scheduling.
    pub async fn start(&self) {
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.sync_all().await;
    }

    /// Stop the scheduler — cancel all timers.
    pub fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // Abort all scheduled feed tasks in a detached future
        // (we can't await here since we only have &self).
        let jobs = Arc::clone(&self.jobs);
        tokio::spawn(async move {
            let mut jobs = jobs.write().await;
            for (_, scheduled) in jobs.iter_mut() {
                if let Some(handle) = scheduled.handle.take() {
                    handle.abort();
                }
            }
            jobs.clear();
        });
    }

    /// Sync all active feeds from the registry.
    pub async fn sync_all(&self) {
        match self.feed_registry.list_all().await {
            Ok(feeds) => {
                tracing::info!("FeedScheduler: syncing {} active feeds", feeds.len());
                for feed in feeds {
                    self.sync_feed(feed).await;
                }
            }
            Err(e) => {
                tracing::error!("FeedScheduler: failed to load feeds: {}", e);
            }
        }
    }

    /// Schedule or reschedule a single feed.
    pub async fn sync_feed(&self, feed: AriaFeed) {
        let feed_id = feed.id;
        let raw_interval = if feed.refresh_seconds > 0 {
            feed.refresh_seconds as u64
        } else {
            schedule_to_interval_secs(&feed.schedule)
        };
        // Clamp to minimum interval to prevent busy-spinning on zero/tiny values.
        let interval_secs = raw_interval.max(MIN_INTERVAL_SECS);
        if raw_interval < MIN_INTERVAL_SECS {
            tracing::warn!(
                "Feed '{}' has interval {}s below minimum, clamped to {}s",
                feed.name,
                raw_interval,
                MIN_INTERVAL_SECS
            );
        }
        let interval = Duration::from_secs(interval_secs);

        // Cancel existing schedule if any.
        {
            let mut jobs = self.jobs.write().await;
            if let Some(existing) = jobs.remove(&feed_id) {
                if let Some(handle) = existing.handle {
                    handle.abort();
                }
            }
        }

        // Schedule the feed.
        let executor = Arc::clone(&self.executor);
        let feed_registry = Arc::clone(&self.feed_registry);
        let running = Arc::clone(&self.running);

        let handle = tokio::spawn(async move {
            // Initial delay: run once after a short startup delay.
            tokio::time::sleep(Duration::from_secs(5)).await;

            loop {
                if !running.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                // Reload feed from registry to check it's still active.
                let current_feed = match feed_registry.get(feed_id).await {
                    Ok(f) if f.status == "active" => f,
                    _ => {
                        tracing::debug!("Feed {} no longer active, stopping scheduler", feed_id);
                        break;
                    }
                };

                // Execute the feed.
                let run_id = Uuid::new_v4();
                tracing::debug!("Executing feed '{}' (run {})", current_feed.name, run_id);

                let result = executor.execute(&current_feed).await;

                if result.success && !result.items.is_empty() {
                    // Store feed items.
                    if let Err(e) = feed_registry
                        .insert_items(
                            &current_feed.tenant_id,
                            feed_id,
                            result.items.clone(),
                            run_id,
                        )
                        .await
                    {
                        tracing::error!(
                            "Failed to store feed items for '{}': {}",
                            current_feed.name,
                            e
                        );
                    }

                    // Apply retention policy.
                    let retention: serde_json::Value = current_feed
                        .retention
                        .clone()
                        .unwrap_or(serde_json::json!({}));
                    let max_items: Option<i64> = retention
                        .get("max_items")
                        .and_then(|v: &serde_json::Value| v.as_i64());
                    let max_age_days: Option<i64> = retention
                        .get("max_age_days")
                        .and_then(|v: &serde_json::Value| v.as_i64());

                    if let Err(e) = feed_registry
                        .prune_by_retention(feed_id, max_items, max_age_days)
                        .await
                    {
                        tracing::warn!(
                            "Failed to prune feed items for '{}': {}",
                            current_feed.name,
                            e
                        );
                    }

                    tracing::info!(
                        "Feed '{}' produced {} items (run {})",
                        current_feed.name,
                        result.items.len(),
                        run_id
                    );
                } else if let Some(ref error) = result.error {
                    tracing::warn!("Feed '{}' execution failed: {}", current_feed.name, error);
                } else if result.success && result.items.is_empty() {
                    // No-op run: success but no items produced.
                    tracing::debug!(
                        "Feed '{}' returned 0 items (no-op run {})",
                        current_feed.name,
                        run_id
                    );
                }

                // Update last_run_at on the feed regardless of result,
                // so we can track when a feed was last attempted.
                if let Err(e) = feed_registry.touch_last_run(feed_id).await {
                    tracing::warn!(
                        "Failed to update last_run_at for feed '{}': {}",
                        current_feed.name,
                        e
                    );
                }

                // Wait for next interval.
                tokio::time::sleep(interval).await;
            }
        });

        let mut jobs = self.jobs.write().await;
        jobs.insert(
            feed_id,
            ScheduledFeed {
                _feed_id: feed_id,
                _interval: interval,
                handle: Some(handle),
            },
        );
    }

    /// Remove a feed from the schedule.
    pub async fn remove_feed(&self, feed_id: Uuid) {
        let mut jobs = self.jobs.write().await;
        if let Some(scheduled) = jobs.remove(&feed_id) {
            if let Some(handle) = scheduled.handle {
                handle.abort();
            }
            tracing::debug!("Removed feed {} from scheduler", feed_id);
        }
    }

    /// Execute a feed immediately (one-shot, outside of schedule).
    pub async fn execute_now(&self, feed_id: Uuid) -> Result<Vec<FeedItem>, String> {
        let feed = self
            .feed_registry
            .get(feed_id)
            .await
            .map_err(|e| format!("feed not found: {e}"))?;

        let result = self.executor.execute(&feed).await;

        if result.success && !result.items.is_empty() {
            let run_id = Uuid::new_v4();
            let _ = self
                .feed_registry
                .insert_items(&feed.tenant_id, feed_id, result.items.clone(), run_id)
                .await;
        }

        if result.success {
            Ok(result.items)
        } else {
            Err(result.error.unwrap_or_else(|| "unknown error".into()))
        }
    }

    /// Get the count of currently scheduled feeds.
    pub async fn scheduled_count(&self) -> usize {
        self.jobs.read().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_parsing() {
        assert_eq!(schedule_to_interval_secs("*/5 * * * *"), 300);
        assert_eq!(schedule_to_interval_secs("0 * * * *"), 3600);
    }
}
