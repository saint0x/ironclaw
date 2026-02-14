//! Aria Cron Function Registry — manages scheduled function definitions and run logs.
//!
//! Unlike most registries, CronFunctionRegistry uses **hard deletes** (not soft deletes)
//! and does NOT implement the generic `Registry` trait.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{RegistryError, RegistryResult};
use crate::aria::types::Pagination;

/// A registered cron function definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaCronFunction {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub schedule_kind: String,
    pub schedule_data: serde_json::Value,
    pub session_target: String,
    pub wake_mode: String,
    pub payload_kind: String,
    pub payload_data: serde_json::Value,
    pub isolation: Option<serde_json::Value>,
    pub cron_job_id: Option<String>,
    pub agent_id: Option<Uuid>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for uploading a cron function.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaCronFunctionUpload {
    pub name: String,
    pub description: String,
    pub schedule_kind: String,
    pub schedule_data: serde_json::Value,
    #[serde(default = "default_session_target")]
    pub session_target: String,
    #[serde(default = "default_wake_mode")]
    pub wake_mode: String,
    pub payload_kind: String,
    pub payload_data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
}

fn default_session_target() -> String {
    "main".to_string()
}

fn default_wake_mode() -> String {
    "now".to_string()
}

/// A single cron function execution record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaCronRun {
    pub id: Uuid,
    pub cron_function_id: Uuid,
    pub tenant_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: String,
    pub result: Option<serde_json::Value>,
    pub error_message: Option<String>,
}

pub struct CronFunctionRegistry {
    pool: Pool,
}

impl CronFunctionRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Upload (upsert) a cron function. Same tenant+name = update.
    pub async fn upload(
        &self,
        tenant_id: &str,
        input: AriaCronFunctionUpload,
    ) -> RegistryResult<AriaCronFunction> {
        let client = self.pool.get().await?;

        let schedule_data = serde_json::to_value(&input.schedule_data)?;
        let payload_data = serde_json::to_value(&input.payload_data)?;
        let isolation = input
            .isolation
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;

        let row = client
            .query_one(
                "INSERT INTO aria_cron_functions \
                 (tenant_id, name, description, schedule_kind, schedule_data, \
                  session_target, wake_mode, payload_kind, payload_data, \
                  isolation, agent_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   description = EXCLUDED.description, \
                   schedule_kind = EXCLUDED.schedule_kind, \
                   schedule_data = EXCLUDED.schedule_data, \
                   session_target = EXCLUDED.session_target, \
                   wake_mode = EXCLUDED.wake_mode, \
                   payload_kind = EXCLUDED.payload_kind, \
                   payload_data = EXCLUDED.payload_data, \
                   isolation = EXCLUDED.isolation, \
                   agent_id = EXCLUDED.agent_id, \
                   status = 'active', \
                   updated_at = NOW() \
                 RETURNING id, tenant_id, name, description, schedule_kind, schedule_data, \
                   session_target, wake_mode, payload_kind, payload_data, isolation, \
                   cron_job_id, agent_id, status, created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.description,
                    &input.schedule_kind,
                    &schedule_data,
                    &input.session_target,
                    &input.wake_mode,
                    &input.payload_kind,
                    &payload_data,
                    &isolation,
                    &input.agent_id,
                ],
            )
            .await?;

        Ok(row_to_cron_function(&row))
    }

    /// Get a cron function by ID.
    pub async fn get(&self, id: Uuid) -> RegistryResult<AriaCronFunction> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, schedule_kind, schedule_data, \
                 session_target, wake_mode, payload_kind, payload_data, isolation, \
                 cron_job_id, agent_id, status, created_at, updated_at \
                 FROM aria_cron_functions WHERE id = $1",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "cron_function".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_cron_function(&row))
    }

    /// Get a cron function by tenant + name.
    pub async fn get_by_name(
        &self,
        tenant_id: &str,
        name: &str,
    ) -> RegistryResult<AriaCronFunction> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, schedule_kind, schedule_data, \
                 session_target, wake_mode, payload_kind, payload_data, isolation, \
                 cron_job_id, agent_id, status, created_at, updated_at \
                 FROM aria_cron_functions WHERE tenant_id = $1 AND name = $2",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "cron_function".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_cron_function(&row))
    }

    /// List cron functions for a tenant with pagination.
    pub async fn get_by_tenant(
        &self,
        tenant_id: &str,
        pagination: Pagination,
    ) -> RegistryResult<Vec<AriaCronFunction>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, schedule_kind, schedule_data, \
                 session_target, wake_mode, payload_kind, payload_data, isolation, \
                 cron_job_id, agent_id, status, created_at, updated_at \
                 FROM aria_cron_functions WHERE tenant_id = $1 \
                 ORDER BY name LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_cron_function).collect())
    }

    /// Get a cron function by its external cron job ID.
    pub async fn get_by_cron_job_id(&self, cron_job_id: &str) -> RegistryResult<AriaCronFunction> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, schedule_kind, schedule_data, \
                 session_target, wake_mode, payload_kind, payload_data, isolation, \
                 cron_job_id, agent_id, status, created_at, updated_at \
                 FROM aria_cron_functions WHERE cron_job_id = $1",
                &[&cron_job_id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "cron_function".into(),
                id: format!("cron_job_id:{cron_job_id}"),
            })?;
        Ok(row_to_cron_function(&row))
    }

    /// Hard-delete a cron function by ID.
    pub async fn delete(&self, id: Uuid) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute("DELETE FROM aria_cron_functions WHERE id = $1", &[&id])
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "cron_function".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Set the external cron job ID (assigned by the scheduler).
    pub async fn set_cron_job_id(&self, id: Uuid, cron_job_id: &str) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_cron_functions SET cron_job_id = $2, updated_at = NOW() \
                 WHERE id = $1",
                &[&id, &cron_job_id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "cron_function".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// Update the status of a cron function (e.g. "active", "paused", "error").
    pub async fn update_status(&self, id: Uuid, status: &str) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_cron_functions SET status = $2, updated_at = NOW() WHERE id = $1",
                &[&id, &status],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "cron_function".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// List all cron functions across all tenants (unbounded, for startup sync).
    pub async fn list_all(&self) -> RegistryResult<Vec<AriaCronFunction>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, schedule_kind, schedule_data, \
                 session_target, wake_mode, payload_kind, payload_data, isolation, \
                 cron_job_id, agent_id, status, created_at, updated_at \
                 FROM aria_cron_functions WHERE status = 'active' ORDER BY name",
                &[],
            )
            .await?;
        Ok(rows.iter().map(row_to_cron_function).collect())
    }

    /// List all cron functions with pagination (across all tenants).
    pub async fn list(&self, pagination: Pagination) -> RegistryResult<Vec<AriaCronFunction>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, schedule_kind, schedule_data, \
                 session_target, wake_mode, payload_kind, payload_data, isolation, \
                 cron_job_id, agent_id, status, created_at, updated_at \
                 FROM aria_cron_functions ORDER BY name LIMIT $1 OFFSET $2",
                &[&pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_cron_function).collect())
    }

    /// Log a cron function execution run.
    pub async fn log_run(
        &self,
        cron_function_id: Uuid,
        tenant_id: &str,
        status: &str,
        result: Option<serde_json::Value>,
        error: Option<&str>,
    ) -> RegistryResult<AriaCronRun> {
        let client = self.pool.get().await?;
        let completed_at: Option<DateTime<Utc>> = if status == "completed" || status == "failed" {
            Some(Utc::now())
        } else {
            None
        };

        let row = client
            .query_one(
                "INSERT INTO aria_cron_runs \
                 (cron_function_id, tenant_id, status, result, error_message, completed_at) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 RETURNING id, cron_function_id, tenant_id, started_at, completed_at, \
                   status, result, error_message",
                &[
                    &cron_function_id,
                    &tenant_id,
                    &status,
                    &result,
                    &error,
                    &completed_at,
                ],
            )
            .await?;
        Ok(row_to_cron_run(&row))
    }

    /// List recent runs for a cron function, ordered newest-first.
    pub async fn list_runs(
        &self,
        cron_function_id: Uuid,
        limit: i64,
    ) -> RegistryResult<Vec<AriaCronRun>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, cron_function_id, tenant_id, started_at, completed_at, \
                 status, result, error_message \
                 FROM aria_cron_runs WHERE cron_function_id = $1 \
                 ORDER BY started_at DESC LIMIT $2",
                &[&cron_function_id, &limit],
            )
            .await?;
        Ok(rows.iter().map(row_to_cron_run).collect())
    }
}

fn row_to_cron_function(row: &tokio_postgres::Row) -> AriaCronFunction {
    AriaCronFunction {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        description: row.get(3),
        schedule_kind: row.get(4),
        schedule_data: row.get(5),
        session_target: row.get(6),
        wake_mode: row.get(7),
        payload_kind: row.get(8),
        payload_data: row.get(9),
        isolation: row.get(10),
        cron_job_id: row.get(11),
        agent_id: row.get(12),
        status: row.get(13),
        created_at: row.get(14),
        updated_at: row.get(15),
    }
}

fn row_to_cron_run(row: &tokio_postgres::Row) -> AriaCronRun {
    AriaCronRun {
        id: row.get(0),
        cron_function_id: row.get(1),
        tenant_id: row.get(2),
        started_at: row.get(3),
        completed_at: row.get(4),
        status: row.get(5),
        result: row.get(6),
        error_message: row.get(7),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_session_target_is_main() {
        assert_eq!(default_session_target(), "main");
    }

    #[test]
    fn default_wake_mode_is_now() {
        assert_eq!(default_wake_mode(), "now");
    }

    #[test]
    fn cron_function_upload_deserializes() {
        let json = serde_json::json!({
            "name": "daily_digest",
            "description": "Send daily digest email",
            "schedule_kind": "cron",
            "schedule_data": {"expr": "0 9 * * *", "tz": "UTC"},
            "payload_kind": "agent_turn",
            "payload_data": {"message": "Send the daily digest"}
        });
        let upload: AriaCronFunctionUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.name, "daily_digest");
        assert_eq!(upload.session_target, "main");
        assert_eq!(upload.wake_mode, "now");
    }
}
