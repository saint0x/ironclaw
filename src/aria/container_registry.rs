//! Aria Container Registry — manages container definitions and runtime state.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{Registry, RegistryError, RegistryResult};
use crate::aria::types::{ContainerState, Pagination, RestartPolicy};

/// A registered container definition with runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaContainer {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub image: String,
    pub command: Option<String>,
    pub environment: Option<serde_json::Value>,
    pub volumes: Option<serde_json::Value>,
    pub ports: Option<serde_json::Value>,
    pub resource_limits: Option<serde_json::Value>,
    pub network_id: Option<Uuid>,
    pub labels: Option<serde_json::Value>,
    pub restart_policy: RestartPolicy,
    pub state: ContainerState,
    pub runtime_ip: Option<String>,
    pub runtime_pid: Option<i32>,
    pub config_hash: Option<String>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub exited_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for uploading a container definition.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaContainerUpload {
    pub name: String,
    pub image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volumes: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ports: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_limits: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<serde_json::Value>,
    #[serde(default = "default_restart_policy")]
    pub restart_policy: RestartPolicy,
}

fn default_restart_policy() -> RestartPolicy {
    RestartPolicy::No
}

pub struct ContainerRegistry {
    pool: Pool,
}

impl ContainerRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Update the runtime state of a container (state, IP, PID).
    pub async fn update_runtime_state(
        &self,
        id: Uuid,
        state: ContainerState,
        ip: Option<&str>,
        pid: Option<i32>,
    ) -> RegistryResult<AriaContainer> {
        let client = self.pool.get().await?;
        let state_str = state.as_str();

        let row = client
            .query_opt(
                "UPDATE aria_containers SET \
                   state = $2, \
                   runtime_ip = COALESCE($3, runtime_ip), \
                   runtime_pid = COALESCE($4, runtime_pid), \
                   started_at = CASE WHEN $2 = 'running' AND started_at IS NULL \
                                     THEN now() ELSE started_at END, \
                   exited_at = CASE WHEN $2 IN ('exited', 'error', 'stopped') \
                                    THEN now() ELSE exited_at END, \
                   updated_at = now() \
                 WHERE id = $1 AND status = 'active' \
                 RETURNING id, tenant_id, name, image, command, environment, \
                   volumes, ports, resource_limits, network_id, labels, \
                   restart_policy, state, runtime_ip, runtime_pid, config_hash, \
                   last_used_at, started_at, exited_at, exit_code, status, \
                   created_at, updated_at",
                &[&id, &state_str, &ip, &pid],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "container".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_container(&row))
    }

    /// List containers attached to a specific network.
    pub async fn list_by_network(&self, network_id: Uuid) -> RegistryResult<Vec<AriaContainer>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, image, command, environment, \
                 volumes, ports, resource_limits, network_id, labels, \
                 restart_policy, state, runtime_ip, runtime_pid, config_hash, \
                 last_used_at, started_at, exited_at, exit_code, status, \
                 created_at, updated_at \
                 FROM aria_containers \
                 WHERE network_id = $1 AND status = 'active' \
                 ORDER BY name",
                &[&network_id],
            )
            .await?;
        Ok(rows.iter().map(row_to_container).collect())
    }

    /// Touch a container to update its last_used_at timestamp.
    pub async fn touch(&self, id: Uuid) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_containers SET last_used_at = now(), updated_at = now() \
                 WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "container".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Registry for ContainerRegistry {
    type Entry = AriaContainer;
    type Upload = AriaContainerUpload;

    async fn upload(
        &self,
        tenant_id: &str,
        input: AriaContainerUpload,
    ) -> RegistryResult<AriaContainer> {
        let client = self.pool.get().await?;
        let restart_str = input.restart_policy.as_str();

        let row = client
            .query_one(
                "INSERT INTO aria_containers (tenant_id, name, image, command, \
                 environment, volumes, ports, resource_limits, network_id, \
                 labels, restart_policy) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   image = EXCLUDED.image, \
                   command = EXCLUDED.command, \
                   environment = EXCLUDED.environment, \
                   volumes = EXCLUDED.volumes, \
                   ports = EXCLUDED.ports, \
                   resource_limits = EXCLUDED.resource_limits, \
                   network_id = EXCLUDED.network_id, \
                   labels = EXCLUDED.labels, \
                   restart_policy = EXCLUDED.restart_policy, \
                   status = 'active', \
                   updated_at = now() \
                 RETURNING id, tenant_id, name, image, command, environment, \
                   volumes, ports, resource_limits, network_id, labels, \
                   restart_policy, state, runtime_ip, runtime_pid, config_hash, \
                   last_used_at, started_at, exited_at, exit_code, status, \
                   created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.image,
                    &input.command,
                    &input.environment,
                    &input.volumes,
                    &input.ports,
                    &input.resource_limits,
                    &input.network_id,
                    &input.labels,
                    &restart_str,
                ],
            )
            .await?;

        Ok(row_to_container(&row))
    }

    async fn get(&self, id: Uuid) -> RegistryResult<AriaContainer> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, image, command, environment, \
                 volumes, ports, resource_limits, network_id, labels, \
                 restart_policy, state, runtime_ip, runtime_pid, config_hash, \
                 last_used_at, started_at, exited_at, exit_code, status, \
                 created_at, updated_at \
                 FROM aria_containers WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "container".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_container(&row))
    }

    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<AriaContainer> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, image, command, environment, \
                 volumes, ports, resource_limits, network_id, labels, \
                 restart_policy, state, runtime_ip, runtime_pid, config_hash, \
                 last_used_at, started_at, exited_at, exit_code, status, \
                 created_at, updated_at \
                 FROM aria_containers \
                 WHERE tenant_id = $1 AND name = $2 AND status = 'active'",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "container".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_container(&row))
    }

    async fn list(
        &self,
        tenant_id: &str,
        pagination: Pagination,
    ) -> RegistryResult<Vec<AriaContainer>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, image, command, environment, \
                 volumes, ports, resource_limits, network_id, labels, \
                 restart_policy, state, runtime_ip, runtime_pid, config_hash, \
                 last_used_at, started_at, exited_at, exit_code, status, \
                 created_at, updated_at \
                 FROM aria_containers WHERE tenant_id = $1 AND status = 'active' \
                 ORDER BY name LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_container).collect())
    }

    async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_containers \
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
                "UPDATE aria_containers SET status = 'deleted', updated_at = now() WHERE id = $1",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "container".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_container(row: &tokio_postgres::Row) -> AriaContainer {
    let restart_str: String = row.get(11);
    let state_str: String = row.get(12);
    AriaContainer {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        image: row.get(3),
        command: row.get(4),
        environment: row.get(5),
        volumes: row.get(6),
        ports: row.get(7),
        resource_limits: row.get(8),
        network_id: row.get(9),
        labels: row.get(10),
        restart_policy: restart_str
            .parse::<RestartPolicy>()
            .unwrap_or(RestartPolicy::No),
        state: state_str
            .parse::<ContainerState>()
            .unwrap_or(ContainerState::Pending),
        runtime_ip: row.get(13),
        runtime_pid: row.get(14),
        config_hash: row.get(15),
        last_used_at: row.get(16),
        started_at: row.get(17),
        exited_at: row.get(18),
        exit_code: row.get(19),
        status: row.get(20),
        created_at: row.get(21),
        updated_at: row.get(22),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_upload_deserializes() {
        let json = serde_json::json!({
            "name": "redis-cache",
            "image": "redis:7-alpine",
            "command": "redis-server --appendonly yes",
            "environment": {"REDIS_PASSWORD": "secret"},
            "ports": [{"host_port": 6379, "container_port": 6379}],
            "resource_limits": {"memory_mb": 256, "cpu_shares": 512},
            "restart_policy": "always"
        });
        let upload: AriaContainerUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.name, "redis-cache");
        assert_eq!(upload.image, "redis:7-alpine");
        assert_eq!(upload.restart_policy, RestartPolicy::Always);
        assert!(upload.network_id.is_none());
    }

    #[test]
    fn default_restart_policy_is_no() {
        let json = serde_json::json!({
            "name": "temp",
            "image": "alpine:latest"
        });
        let upload: AriaContainerUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.restart_policy, RestartPolicy::No);
    }

    #[test]
    fn container_state_roundtrip() {
        for state in &[
            ContainerState::Pending,
            ContainerState::Starting,
            ContainerState::Running,
            ContainerState::Stopped,
            ContainerState::Exited,
            ContainerState::Error,
        ] {
            let parsed: ContainerState = state.as_str().parse().unwrap();
            assert_eq!(*state, parsed);
        }
    }
}
