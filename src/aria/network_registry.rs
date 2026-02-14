//! Aria Network Registry — manages virtual network definitions for container isolation.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{Registry, RegistryError, RegistryResult};
use crate::aria::types::Pagination;

/// A registered network definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaNetwork {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub driver: String,
    pub isolation: String,
    pub ipv6: bool,
    pub dns_config: Option<serde_json::Value>,
    pub labels: Option<serde_json::Value>,
    pub options: Option<serde_json::Value>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for uploading a network definition.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaNetworkUpload {
    pub name: String,
    #[serde(default = "default_driver")]
    pub driver: String,
    #[serde(default = "default_isolation")]
    pub isolation: String,
    #[serde(default)]
    pub ipv6: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
}

fn default_driver() -> String {
    "bridge".to_string()
}

fn default_isolation() -> String {
    "default".to_string()
}

pub struct NetworkRegistry {
    pool: Pool,
}

impl NetworkRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl Registry for NetworkRegistry {
    type Entry = AriaNetwork;
    type Upload = AriaNetworkUpload;

    async fn upload(
        &self,
        tenant_id: &str,
        input: AriaNetworkUpload,
    ) -> RegistryResult<AriaNetwork> {
        let client = self.pool.get().await?;

        let row = client
            .query_one(
                "INSERT INTO aria_networks (tenant_id, name, driver, isolation, \
                 ipv6, dns_config, labels, options) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   driver = EXCLUDED.driver, \
                   isolation = EXCLUDED.isolation, \
                   ipv6 = EXCLUDED.ipv6, \
                   dns_config = EXCLUDED.dns_config, \
                   labels = EXCLUDED.labels, \
                   options = EXCLUDED.options, \
                   status = 'active', \
                   updated_at = now() \
                 RETURNING id, tenant_id, name, driver, isolation, ipv6, \
                   dns_config, labels, options, status, created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.driver,
                    &input.isolation,
                    &input.ipv6,
                    &input.dns_config,
                    &input.labels,
                    &input.options,
                ],
            )
            .await?;

        Ok(row_to_network(&row))
    }

    async fn get(&self, id: Uuid) -> RegistryResult<AriaNetwork> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, driver, isolation, ipv6, \
                 dns_config, labels, options, status, created_at, updated_at \
                 FROM aria_networks WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "network".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_network(&row))
    }

    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<AriaNetwork> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, driver, isolation, ipv6, \
                 dns_config, labels, options, status, created_at, updated_at \
                 FROM aria_networks \
                 WHERE tenant_id = $1 AND name = $2 AND status = 'active'",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "network".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_network(&row))
    }

    async fn list(
        &self,
        tenant_id: &str,
        pagination: Pagination,
    ) -> RegistryResult<Vec<AriaNetwork>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, driver, isolation, ipv6, \
                 dns_config, labels, options, status, created_at, updated_at \
                 FROM aria_networks WHERE tenant_id = $1 AND status = 'active' \
                 ORDER BY name LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_network).collect())
    }

    async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_networks \
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
                "UPDATE aria_networks SET status = 'deleted', updated_at = now() WHERE id = $1",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "network".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_network(row: &tokio_postgres::Row) -> AriaNetwork {
    AriaNetwork {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        driver: row.get(3),
        isolation: row.get(4),
        ipv6: row.get(5),
        dns_config: row.get(6),
        labels: row.get(7),
        options: row.get(8),
        status: row.get(9),
        created_at: row.get(10),
        updated_at: row.get(11),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_upload_deserializes() {
        let json = serde_json::json!({
            "name": "backend-net",
            "driver": "bridge",
            "isolation": "strict",
            "ipv6": true,
            "dns_config": {
                "servers": ["8.8.8.8", "1.1.1.1"],
                "search_domains": ["internal.local"]
            },
            "labels": {"env": "production"}
        });
        let upload: AriaNetworkUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.name, "backend-net");
        assert_eq!(upload.driver, "bridge");
        assert!(upload.ipv6);
        assert!(upload.dns_config.is_some());
    }

    #[test]
    fn network_upload_defaults() {
        let json = serde_json::json!({
            "name": "simple-net"
        });
        let upload: AriaNetworkUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.driver, "bridge");
        assert_eq!(upload.isolation, "default");
        assert!(!upload.ipv6);
        assert!(upload.dns_config.is_none());
        assert!(upload.labels.is_none());
        assert!(upload.options.is_none());
    }
}
