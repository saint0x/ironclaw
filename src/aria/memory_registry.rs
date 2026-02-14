//! Aria Memory Registry — key-value memory store with tiers and TTL support.
//!
//! Unlike most registries, MemoryRegistry does NOT implement the `Registry` trait.
//! It uses set/get/delete by key rather than upload/get by name, and supports
//! tier-based storage (scratchpad, ephemeral, longterm) with expiration sweeping.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{RegistryError, RegistryResult};
use crate::aria::types::{MemoryTier, Pagination};

/// A memory entry stored in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaMemoryEntry {
    pub id: Uuid,
    pub tenant_id: String,
    pub key: String,
    pub value: serde_json::Value,
    pub tier: String,
    pub session_id: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for setting a memory entry.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaMemorySet {
    pub key: String,
    pub value: serde_json::Value,
    #[serde(default = "default_tier")]
    pub tier: MemoryTier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

fn default_tier() -> MemoryTier {
    MemoryTier::Longterm
}

pub struct MemoryRegistry {
    pool: Pool,
}

impl MemoryRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Set (upsert) a memory entry by tenant_id + key.
    pub async fn set(
        &self,
        tenant_id: &str,
        input: AriaMemorySet,
    ) -> RegistryResult<AriaMemoryEntry> {
        let client = self.pool.get().await?;

        let value_json = serde_json::to_value(&input.value)?;
        let tier_str = input.tier.as_str();

        let row = client
            .query_one(
                "INSERT INTO aria_memory (tenant_id, key, value, tier, session_id, expires_at) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (tenant_id, key) DO UPDATE SET \
                   value = EXCLUDED.value, \
                   tier = EXCLUDED.tier, \
                   session_id = EXCLUDED.session_id, \
                   expires_at = EXCLUDED.expires_at, \
                   updated_at = NOW() \
                 RETURNING id, tenant_id, key, value, tier, session_id, expires_at, \
                   created_at, updated_at",
                &[
                    &tenant_id,
                    &input.key,
                    &value_json,
                    &tier_str,
                    &input.session_id,
                    &input.expires_at,
                ],
            )
            .await?;

        Ok(row_to_memory(&row))
    }

    /// Get a memory entry by tenant_id, key, and optional tier filter.
    pub async fn get_by_key(
        &self,
        tenant_id: &str,
        key: &str,
        tier: Option<MemoryTier>,
    ) -> RegistryResult<AriaMemoryEntry> {
        let client = self.pool.get().await?;

        let row = match tier {
            Some(t) => {
                let tier_str = t.as_str();
                client
                    .query_opt(
                        "SELECT id, tenant_id, key, value, tier, session_id, expires_at, \
                         created_at, updated_at \
                         FROM aria_memory \
                         WHERE tenant_id = $1 AND key = $2 AND tier = $3",
                        &[&tenant_id, &key, &tier_str],
                    )
                    .await?
            }
            None => {
                client
                    .query_opt(
                        "SELECT id, tenant_id, key, value, tier, session_id, expires_at, \
                         created_at, updated_at \
                         FROM aria_memory \
                         WHERE tenant_id = $1 AND key = $2",
                        &[&tenant_id, &key],
                    )
                    .await?
            }
        };

        row.map(|r| row_to_memory(&r))
            .ok_or_else(|| RegistryError::NotFound {
                entity: "memory".into(),
                id: format!("{tenant_id}:{key}"),
            })
    }

    /// List memory entries for a tenant.
    pub async fn list(
        &self,
        tenant_id: &str,
        pagination: Pagination,
    ) -> RegistryResult<Vec<AriaMemoryEntry>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, key, value, tier, session_id, expires_at, \
                 created_at, updated_at \
                 FROM aria_memory WHERE tenant_id = $1 \
                 ORDER BY key LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_memory).collect())
    }

    /// Count memory entries for a tenant.
    pub async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_memory WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// Delete a memory entry by tenant_id + key.
    pub async fn delete_by_key(&self, tenant_id: &str, key: &str) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "DELETE FROM aria_memory WHERE tenant_id = $1 AND key = $2",
                &[&tenant_id, &key],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "memory".into(),
                id: format!("{tenant_id}:{key}"),
            });
        }
        Ok(())
    }

    /// Sweep expired entries — DELETE all rows where expires_at < NOW().
    /// Returns the number of rows deleted.
    pub async fn sweep_expired(&self) -> RegistryResult<u64> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "DELETE FROM aria_memory WHERE expires_at IS NOT NULL AND expires_at < NOW()",
                &[],
            )
            .await?;
        Ok(n)
    }

    /// Clear all memory entries for a specific session.
    /// Returns the number of rows deleted.
    pub async fn clear_session(&self, session_id: &str) -> RegistryResult<u64> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "DELETE FROM aria_memory WHERE session_id = $1",
                &[&session_id],
            )
            .await?;
        Ok(n)
    }

    /// Generate a system-prompt-injectable summary of memory entries for a tenant.
    pub async fn get_prompt_section(&self, tenant_id: &str) -> RegistryResult<String> {
        let client = self.pool.get().await?;

        // Get counts per tier.
        let rows = client
            .query(
                "SELECT tier, COUNT(*) FROM aria_memory \
                 WHERE tenant_id = $1 \
                 GROUP BY tier ORDER BY tier",
                &[&tenant_id],
            )
            .await?;

        if rows.is_empty() {
            return Ok(String::new());
        }

        let mut section = String::from("## Memory Store\n\n");
        for row in &rows {
            let tier: &str = row.get(0);
            let count: i64 = row.get(1);
            section.push_str(&format!("- **{tier}**: {count} entries\n"));
        }

        // Show recent longterm keys as a quick reference.
        let recent = client
            .query(
                "SELECT key FROM aria_memory \
                 WHERE tenant_id = $1 AND tier = 'longterm' \
                 ORDER BY updated_at DESC LIMIT 10",
                &[&tenant_id],
            )
            .await?;

        if !recent.is_empty() {
            section.push_str("\nRecent longterm keys: ");
            let keys: Vec<&str> = recent.iter().map(|r| r.get(0)).collect();
            section.push_str(&keys.join(", "));
            section.push('\n');
        }

        Ok(section)
    }
}

fn row_to_memory(row: &tokio_postgres::Row) -> AriaMemoryEntry {
    AriaMemoryEntry {
        id: row.get(0),
        tenant_id: row.get(1),
        key: row.get(2),
        value: row.get(3),
        tier: row.get(4),
        session_id: row.get(5),
        expires_at: row.get(6),
        created_at: row.get(7),
        updated_at: row.get(8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tier_is_longterm() {
        assert_eq!(default_tier(), MemoryTier::Longterm);
    }

    #[test]
    fn memory_set_deserializes_with_defaults() {
        let json = serde_json::json!({
            "key": "user.preference.theme",
            "value": "dark"
        });
        let entry: AriaMemorySet = serde_json::from_value(json).unwrap();
        assert_eq!(entry.key, "user.preference.theme");
        assert_eq!(entry.tier, MemoryTier::Longterm);
        assert!(entry.session_id.is_none());
        assert!(entry.expires_at.is_none());
    }

    #[test]
    fn memory_set_deserializes_with_explicit_tier() {
        let json = serde_json::json!({
            "key": "temp.calc.result",
            "value": {"sum": 42},
            "tier": "scratchpad",
            "session_id": "sess_abc123"
        });
        let entry: AriaMemorySet = serde_json::from_value(json).unwrap();
        assert_eq!(entry.tier, MemoryTier::Scratchpad);
        assert_eq!(entry.session_id.as_deref(), Some("sess_abc123"));
    }

    #[test]
    fn memory_set_deserializes_ephemeral_with_expiry() {
        let json = serde_json::json!({
            "key": "cache.weather",
            "value": {"temp": 72, "unit": "F"},
            "tier": "ephemeral",
            "expires_at": "2025-12-31T23:59:59Z"
        });
        let entry: AriaMemorySet = serde_json::from_value(json).unwrap();
        assert_eq!(entry.tier, MemoryTier::Ephemeral);
        assert!(entry.expires_at.is_some());
    }

    #[test]
    fn memory_entry_serializes_correctly() {
        let entry = AriaMemoryEntry {
            id: Uuid::nil(),
            tenant_id: "t1".into(),
            key: "test.key".into(),
            value: serde_json::json!("hello"),
            tier: "longterm".into(),
            session_id: None,
            expires_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["key"], "test.key");
        assert_eq!(json["tier"], "longterm");
    }
}
