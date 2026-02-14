//! Aria KV Registry — a simple key-value store scoped per tenant.
//!
//! Does NOT implement the generic `Registry` trait because it is key-based
//! rather than name-based, and supports upsert semantics with prefix queries.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{RegistryError, RegistryResult};
use crate::aria::types::Pagination;

/// A key-value entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaKvEntry {
    pub id: Uuid,
    pub tenant_id: String,
    pub key: String,
    pub value: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct KvRegistry {
    pool: Pool,
}

impl KvRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Set (upsert) a key-value pair. Creates if new, updates if exists.
    pub async fn set(
        &self,
        tenant_id: &str,
        key: &str,
        value: serde_json::Value,
    ) -> RegistryResult<AriaKvEntry> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "INSERT INTO aria_kv (tenant_id, key, value) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (tenant_id, key) DO UPDATE SET \
                   value = EXCLUDED.value, \
                   updated_at = NOW() \
                 RETURNING id, tenant_id, key, value, created_at, updated_at",
                &[&tenant_id, &key, &value],
            )
            .await?;
        Ok(row_to_kv_entry(&row))
    }

    /// Get a single entry by tenant + key.
    pub async fn get(&self, tenant_id: &str, key: &str) -> RegistryResult<AriaKvEntry> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, key, value, created_at, updated_at \
                 FROM aria_kv WHERE tenant_id = $1 AND key = $2",
                &[&tenant_id, &key],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "kv".into(),
                id: format!("{tenant_id}:{key}"),
            })?;
        Ok(row_to_kv_entry(&row))
    }

    /// List all entries for a tenant with pagination.
    pub async fn list(
        &self,
        tenant_id: &str,
        pagination: Pagination,
    ) -> RegistryResult<Vec<AriaKvEntry>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, key, value, created_at, updated_at \
                 FROM aria_kv WHERE tenant_id = $1 \
                 ORDER BY key LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_kv_entry).collect())
    }

    /// Count all entries for a tenant.
    pub async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_kv WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await?;
        Ok(row.get(0))
    }

    /// Delete a single entry by tenant + key.
    pub async fn delete(&self, tenant_id: &str, key: &str) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "DELETE FROM aria_kv WHERE tenant_id = $1 AND key = $2",
                &[&tenant_id, &key],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "kv".into(),
                id: format!("{tenant_id}:{key}"),
            });
        }
        Ok(())
    }

    /// Query entries by key prefix for namespace-scoped lookups.
    ///
    /// For example, `query("tenant1", "config:")` returns all keys starting
    /// with `"config:"` for that tenant.
    pub async fn query(&self, tenant_id: &str, prefix: &str) -> RegistryResult<Vec<AriaKvEntry>> {
        let client = self.pool.get().await?;
        // Use the LIKE operator with the prefix escaped for safety,
        // then append '%' for the prefix match.
        let pattern = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = client
            .query(
                "SELECT id, tenant_id, key, value, created_at, updated_at \
                 FROM aria_kv WHERE tenant_id = $1 AND key LIKE $2 \
                 ORDER BY key",
                &[&tenant_id, &pattern],
            )
            .await?;
        Ok(rows.iter().map(row_to_kv_entry).collect())
    }
}

fn row_to_kv_entry(row: &tokio_postgres::Row) -> AriaKvEntry {
    AriaKvEntry {
        id: row.get(0),
        tenant_id: row.get(1),
        key: row.get(2),
        value: row.get(3),
        created_at: row.get(4),
        updated_at: row.get(5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_escaping_handles_special_chars() {
        let prefix = "config:%_test\\";
        let pattern = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        assert_eq!(pattern, "config:\\%\\_test\\\\%");
    }

    #[test]
    fn kv_entry_serializes_to_json() {
        let entry = AriaKvEntry {
            id: Uuid::nil(),
            tenant_id: "t1".to_string(),
            key: "my_key".to_string(),
            value: serde_json::json!({"foo": "bar"}),
            created_at: DateTime::default(),
            updated_at: DateTime::default(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["key"], "my_key");
        assert_eq!(json["value"]["foo"], "bar");
    }
}
