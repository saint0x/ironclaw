//! Aria Feed Registry — manages feed definitions and their produced items.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::aria::registry::{Registry, RegistryError, RegistryResult};
use crate::aria::types::{FeedItem, Pagination};

/// A registered feed definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaFeed {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub handler_code: String,
    pub handler_hash: String,
    pub schedule: String,
    pub refresh_seconds: i32,
    pub category: String,
    pub retention: Option<serde_json::Value>,
    pub display: Option<serde_json::Value>,
    pub status: String,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for uploading a feed.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaFeedUpload {
    pub name: String,
    pub description: String,
    pub handler_code: String,
    pub schedule: String,
    #[serde(default = "default_refresh_seconds")]
    pub refresh_seconds: i32,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<serde_json::Value>,
}

fn default_refresh_seconds() -> i32 {
    3600
}

fn default_category() -> String {
    "general".to_string()
}

/// A persisted feed item produced by a feed run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaFeedItem {
    pub id: Uuid,
    pub tenant_id: String,
    pub feed_id: Uuid,
    pub run_id: Uuid,
    pub card_type: String,
    pub title: String,
    pub body: Option<String>,
    pub source: Option<String>,
    pub url: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub item_timestamp: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub struct FeedRegistry {
    pool: Pool,
}

impl FeedRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// List all active feeds across ALL tenants (for FeedScheduler startup).
    pub async fn list_all(&self) -> RegistryResult<Vec<AriaFeed>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, handler_code, handler_hash, \
                 schedule, refresh_seconds, category, retention, display, status, \
                 last_run_at, next_run_at, created_at, updated_at \
                 FROM aria_feeds WHERE status = 'active' ORDER BY name",
                &[],
            )
            .await?;
        Ok(rows.iter().map(row_to_feed).collect())
    }

    /// Update the status of a feed (e.g. "active", "paused", "error").
    pub async fn update_status(&self, id: Uuid, status: &str) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_feeds SET status = $2, updated_at = NOW() WHERE id = $1",
                &[&id, &status],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "feed".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Bulk-insert feed items produced by a single feed run.
    pub async fn insert_items(
        &self,
        tenant_id: &str,
        feed_id: Uuid,
        items: Vec<FeedItem>,
        run_id: Uuid,
    ) -> RegistryResult<Vec<AriaFeedItem>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        let client = self.pool.get().await?;
        let mut inserted = Vec::with_capacity(items.len());

        for item in &items {
            let item_ts: Option<DateTime<Utc>> = item
                .timestamp
                .map(|ms| DateTime::from_timestamp_millis(ms).unwrap_or_default());
            let metadata = item
                .metadata
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?;

            let row = client
                .query_one(
                    "INSERT INTO aria_feed_items \
                     (tenant_id, feed_id, run_id, card_type, title, body, source, url, \
                      metadata, item_timestamp) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                     RETURNING id, tenant_id, feed_id, run_id, card_type, title, body, \
                       source, url, metadata, item_timestamp, created_at",
                    &[
                        &tenant_id,
                        &feed_id,
                        &run_id,
                        &item.card_type,
                        &item.title,
                        &item.body,
                        &item.source,
                        &item.url,
                        &metadata,
                        &item_ts,
                    ],
                )
                .await?;
            inserted.push(row_to_feed_item(&row));
        }

        // Update the feed's last_run_at timestamp.
        client
            .execute(
                "UPDATE aria_feeds SET last_run_at = NOW(), updated_at = NOW() WHERE id = $1",
                &[&feed_id],
            )
            .await?;

        Ok(inserted)
    }

    /// Prune old feed items by retention policy.
    ///
    /// Deletes items that exceed `max_items` count or are older than `max_age_days`.
    pub async fn prune_by_retention(
        &self,
        feed_id: Uuid,
        max_items: Option<i64>,
        max_age_days: Option<i64>,
    ) -> RegistryResult<u64> {
        let client = self.pool.get().await?;
        let mut total_deleted: u64 = 0;

        // Prune by age first.
        if let Some(days) = max_age_days {
            let n = client
                .execute(
                    "DELETE FROM aria_feed_items \
                     WHERE feed_id = $1 AND created_at < NOW() - ($2 || ' days')::interval",
                    &[&feed_id, &days.to_string()],
                )
                .await?;
            total_deleted += n;
        }

        // Then prune by count, keeping the most recent items.
        if let Some(max) = max_items {
            let n = client
                .execute(
                    "DELETE FROM aria_feed_items WHERE id IN ( \
                       SELECT id FROM aria_feed_items \
                       WHERE feed_id = $1 \
                       ORDER BY created_at DESC \
                       OFFSET $2 \
                     )",
                    &[&feed_id, &max],
                )
                .await?;
            total_deleted += n;
        }

        Ok(total_deleted)
    }

    /// Update the `last_run_at` timestamp without inserting items.
    ///
    /// Called after every execution (including no-op runs) so we can track
    /// when a feed was last attempted.
    pub async fn touch_last_run(&self, feed_id: Uuid) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        client
            .execute(
                "UPDATE aria_feeds SET last_run_at = NOW(), updated_at = NOW() WHERE id = $1",
                &[&feed_id],
            )
            .await?;
        Ok(())
    }

    /// List recent items for a feed, ordered newest-first.
    pub async fn list_items(&self, feed_id: Uuid, limit: i64) -> RegistryResult<Vec<AriaFeedItem>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, feed_id, run_id, card_type, title, body, \
                 source, url, metadata, item_timestamp, created_at \
                 FROM aria_feed_items WHERE feed_id = $1 \
                 ORDER BY created_at DESC LIMIT $2",
                &[&feed_id, &limit],
            )
            .await?;
        Ok(rows.iter().map(row_to_feed_item).collect())
    }
}

#[async_trait::async_trait]
impl Registry for FeedRegistry {
    type Entry = AriaFeed;
    type Upload = AriaFeedUpload;

    async fn upload(&self, tenant_id: &str, input: AriaFeedUpload) -> RegistryResult<AriaFeed> {
        let client = self.pool.get().await?;

        let handler_hash = format!("{:x}", Sha256::digest(input.handler_code.as_bytes()));
        let retention = input
            .retention
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        let display = input
            .display
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;

        let row = client
            .query_one(
                "INSERT INTO aria_feeds \
                 (tenant_id, name, description, handler_code, handler_hash, schedule, \
                  refresh_seconds, category, retention, display) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   description = EXCLUDED.description, \
                   handler_code = EXCLUDED.handler_code, \
                   handler_hash = EXCLUDED.handler_hash, \
                   schedule = EXCLUDED.schedule, \
                   refresh_seconds = EXCLUDED.refresh_seconds, \
                   category = EXCLUDED.category, \
                   retention = EXCLUDED.retention, \
                   display = EXCLUDED.display, \
                   status = 'active', \
                   updated_at = NOW() \
                 RETURNING id, tenant_id, name, description, handler_code, handler_hash, \
                   schedule, refresh_seconds, category, retention, display, status, \
                   last_run_at, next_run_at, created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.description,
                    &input.handler_code,
                    &handler_hash,
                    &input.schedule,
                    &input.refresh_seconds,
                    &input.category,
                    &retention,
                    &display,
                ],
            )
            .await?;

        Ok(row_to_feed(&row))
    }

    async fn get(&self, id: Uuid) -> RegistryResult<AriaFeed> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, handler_code, handler_hash, \
                 schedule, refresh_seconds, category, retention, display, status, \
                 last_run_at, next_run_at, created_at, updated_at \
                 FROM aria_feeds WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "feed".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_feed(&row))
    }

    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<AriaFeed> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, handler_code, handler_hash, \
                 schedule, refresh_seconds, category, retention, display, status, \
                 last_run_at, next_run_at, created_at, updated_at \
                 FROM aria_feeds WHERE tenant_id = $1 AND name = $2 AND status = 'active'",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "feed".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_feed(&row))
    }

    async fn list(&self, tenant_id: &str, pagination: Pagination) -> RegistryResult<Vec<AriaFeed>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, handler_code, handler_hash, \
                 schedule, refresh_seconds, category, retention, display, status, \
                 last_run_at, next_run_at, created_at, updated_at \
                 FROM aria_feeds WHERE tenant_id = $1 AND status = 'active' \
                 ORDER BY name LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_feed).collect())
    }

    async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_feeds \
                 WHERE tenant_id = $1 AND status = 'active'",
                &[&tenant_id],
            )
            .await?;
        Ok(row.get(0))
    }

    async fn delete(&self, id: Uuid) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_feeds SET status = 'deleted', updated_at = NOW() WHERE id = $1",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "feed".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_feed(row: &tokio_postgres::Row) -> AriaFeed {
    AriaFeed {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        description: row.get(3),
        handler_code: row.get(4),
        handler_hash: row.get(5),
        schedule: row.get(6),
        refresh_seconds: row.get(7),
        category: row.get(8),
        retention: row.get(9),
        display: row.get(10),
        status: row.get(11),
        last_run_at: row.get(12),
        next_run_at: row.get(13),
        created_at: row.get(14),
        updated_at: row.get(15),
    }
}

fn row_to_feed_item(row: &tokio_postgres::Row) -> AriaFeedItem {
    AriaFeedItem {
        id: row.get(0),
        tenant_id: row.get(1),
        feed_id: row.get(2),
        run_id: row.get(3),
        card_type: row.get(4),
        title: row.get(5),
        body: row.get(6),
        source: row.get(7),
        url: row.get(8),
        metadata: row.get(9),
        item_timestamp: row.get(10),
        created_at: row.get(11),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_hash_is_deterministic() {
        let code = "export default async function fetchFeed() { return []; }";
        let h1 = format!("{:x}", Sha256::digest(code.as_bytes()));
        let h2 = format!("{:x}", Sha256::digest(code.as_bytes()));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn default_refresh_seconds_is_one_hour() {
        assert_eq!(default_refresh_seconds(), 3600);
    }

    #[test]
    fn default_category_is_general() {
        assert_eq!(default_category(), "general");
    }
}
