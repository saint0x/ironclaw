//! Aria Task Registry — manages task definitions and execution lifecycle.
//!
//! Implements the `Registry` trait (with `upload` serving as the create operation)
//! plus additional methods for status transitions, cancellation, and filtering.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{Registry, RegistryError, RegistryResult};
use crate::aria::types::{Pagination, TaskStatus};

/// A registered task with execution state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaTask {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub handler_code: String,
    pub params: serde_json::Value,
    pub status: String,
    pub result: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub agent_id: Option<Uuid>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a task.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaTaskCreate {
    pub name: String,
    pub description: String,
    pub handler_code: String,
    #[serde(default = "default_params")]
    pub params: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
}

fn default_params() -> serde_json::Value {
    serde_json::json!({})
}

pub struct TaskRegistry {
    pool: Pool,
}

impl TaskRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Update the status of a task, optionally setting result and error_message.
    ///
    /// Also updates `started_at` when transitioning to `running` and
    /// `completed_at` when transitioning to `completed` or `failed`.
    pub async fn update_status(
        &self,
        id: Uuid,
        status: TaskStatus,
        result: Option<serde_json::Value>,
        error_message: Option<String>,
    ) -> RegistryResult<AriaTask> {
        let client = self.pool.get().await?;
        let status_str = status.as_str();

        // Set started_at on transition to running.
        let started_at_clause = if status == TaskStatus::Running {
            ", started_at = COALESCE(started_at, NOW())"
        } else {
            ""
        };

        // Set completed_at on terminal transitions.
        let completed_at_clause = if status == TaskStatus::Completed || status == TaskStatus::Failed
        {
            ", completed_at = NOW()"
        } else {
            ""
        };

        let query = format!(
            "UPDATE aria_tasks SET \
               status = $2, \
               result = COALESCE($3, result), \
               error_message = COALESCE($4, error_message), \
               updated_at = NOW() \
               {started_at_clause}{completed_at_clause} \
             WHERE id = $1 \
             RETURNING id, tenant_id, name, description, handler_code, params, \
               status, result, error_message, agent_id, started_at, completed_at, \
               created_at, updated_at"
        );

        let row = client
            .query_opt(&query, &[&id, &status_str, &result, &error_message])
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "task".into(),
                id: id.to_string(),
            })?;

        Ok(row_to_task(&row))
    }

    /// Cancel a task by setting its status to cancelled.
    pub async fn cancel(&self, id: Uuid) -> RegistryResult<AriaTask> {
        self.update_status(id, TaskStatus::Cancelled, None, None)
            .await
    }

    /// List tasks filtered by status for a tenant.
    pub async fn list_by_status(
        &self,
        tenant_id: &str,
        status: TaskStatus,
    ) -> RegistryResult<Vec<AriaTask>> {
        let client = self.pool.get().await?;
        let status_str = status.as_str();
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, handler_code, params, \
                 status, result, error_message, agent_id, started_at, completed_at, \
                 created_at, updated_at \
                 FROM aria_tasks WHERE tenant_id = $1 AND status = $2 \
                 ORDER BY created_at DESC",
                &[&tenant_id, &status_str],
            )
            .await?;
        Ok(rows.iter().map(row_to_task).collect())
    }

    /// List pending tasks across all tenants (used by the task executor polling loop).
    pub async fn list_pending(&self, limit: i64) -> RegistryResult<Vec<AriaTask>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, handler_code, params, \
                 status, result, error_message, agent_id, started_at, completed_at, \
                 created_at, updated_at \
                 FROM aria_tasks WHERE status = 'pending' \
                 ORDER BY created_at ASC LIMIT $1",
                &[&limit],
            )
            .await?;
        Ok(rows.iter().map(row_to_task).collect())
    }

    /// Generate a system-prompt-injectable summary of pending and running tasks for a tenant.
    pub async fn get_prompt_section(&self, tenant_id: &str) -> RegistryResult<String> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT name, description, status, created_at FROM aria_tasks \
                 WHERE tenant_id = $1 AND status IN ('pending', 'running') \
                 ORDER BY created_at ASC",
                &[&tenant_id],
            )
            .await?;

        if rows.is_empty() {
            return Ok(String::new());
        }

        let mut section = String::from("## Active Tasks\n\n");
        for row in &rows {
            let name: &str = row.get(0);
            let desc: &str = row.get(1);
            let status: &str = row.get(2);
            section.push_str(&format!("- **{name}** [{status}]: {desc}\n"));
        }
        Ok(section)
    }
}

#[async_trait::async_trait]
impl Registry for TaskRegistry {
    type Entry = AriaTask;
    type Upload = AriaTaskCreate;

    async fn upload(&self, tenant_id: &str, input: AriaTaskCreate) -> RegistryResult<AriaTask> {
        let client = self.pool.get().await?;

        let params_json = serde_json::to_value(&input.params)?;

        let row = client
            .query_one(
                "INSERT INTO aria_tasks (tenant_id, name, description, handler_code, \
                 params, agent_id) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   description = EXCLUDED.description, \
                   handler_code = EXCLUDED.handler_code, \
                   params = EXCLUDED.params, \
                   agent_id = EXCLUDED.agent_id, \
                   status = 'pending', \
                   result = NULL, \
                   error_message = NULL, \
                   started_at = NULL, \
                   completed_at = NULL, \
                   updated_at = NOW() \
                 RETURNING id, tenant_id, name, description, handler_code, params, \
                   status, result, error_message, agent_id, started_at, completed_at, \
                   created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.description,
                    &input.handler_code,
                    &params_json,
                    &input.agent_id,
                ],
            )
            .await?;

        Ok(row_to_task(&row))
    }

    async fn get(&self, id: Uuid) -> RegistryResult<AriaTask> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, handler_code, params, \
                 status, result, error_message, agent_id, started_at, completed_at, \
                 created_at, updated_at \
                 FROM aria_tasks WHERE id = $1",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "task".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_task(&row))
    }

    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<AriaTask> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, handler_code, params, \
                 status, result, error_message, agent_id, started_at, completed_at, \
                 created_at, updated_at \
                 FROM aria_tasks WHERE tenant_id = $1 AND name = $2",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "task".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_task(&row))
    }

    async fn list(&self, tenant_id: &str, pagination: Pagination) -> RegistryResult<Vec<AriaTask>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, handler_code, params, \
                 status, result, error_message, agent_id, started_at, completed_at, \
                 created_at, updated_at \
                 FROM aria_tasks WHERE tenant_id = $1 \
                 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_task).collect())
    }

    async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_tasks WHERE tenant_id = $1",
                &[&tenant_id],
            )
            .await?;
        Ok(row.get(0))
    }

    async fn delete(&self, id: Uuid) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_tasks SET status = 'cancelled', updated_at = NOW() WHERE id = $1",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "task".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_task(row: &tokio_postgres::Row) -> AriaTask {
    AriaTask {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        description: row.get(3),
        handler_code: row.get(4),
        params: row.get(5),
        status: row.get(6),
        result: row.get(7),
        error_message: row.get(8),
        agent_id: row.get(9),
        started_at: row.get(10),
        completed_at: row.get(11),
        created_at: row.get(12),
        updated_at: row.get(13),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_params_is_empty_object() {
        let params = default_params();
        assert!(params.is_object());
        assert_eq!(params.as_object().unwrap().len(), 0);
    }

    #[test]
    fn task_create_deserializes_with_defaults() {
        let json = serde_json::json!({
            "name": "data-export",
            "description": "Export user data to CSV",
            "handler_code": "async function run(params) { return {}; }"
        });
        let create: AriaTaskCreate = serde_json::from_value(json).unwrap();
        assert_eq!(create.name, "data-export");
        assert!(create.params.is_object());
        assert!(create.agent_id.is_none());
    }

    #[test]
    fn task_create_deserializes_with_explicit_values() {
        let agent_id = Uuid::new_v4();
        let json = serde_json::json!({
            "name": "send-report",
            "description": "Generate and send weekly report",
            "handler_code": "async function run(params) { return { sent: true }; }",
            "params": {"recipients": ["alice@example.com"]},
            "agent_id": agent_id.to_string()
        });
        let create: AriaTaskCreate = serde_json::from_value(json).unwrap();
        assert_eq!(create.name, "send-report");
        assert_eq!(create.agent_id, Some(agent_id));
        assert!(create.params["recipients"].is_array());
    }

    #[test]
    fn task_status_round_trips() {
        for status in &[
            TaskStatus::Pending,
            TaskStatus::Running,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            let s = status.as_str();
            let parsed: TaskStatus = s.parse().unwrap();
            assert_eq!(*status, parsed);
        }
    }

    #[test]
    fn task_serializes_correctly() {
        let task = AriaTask {
            id: Uuid::nil(),
            tenant_id: "t1".into(),
            name: "test-task".into(),
            description: "A test".into(),
            handler_code: "fn(){}".into(),
            params: serde_json::json!({}),
            status: "pending".into(),
            result: None,
            error_message: None,
            agent_id: None,
            started_at: None,
            completed_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["name"], "test-task");
        assert_eq!(json["status"], "pending");
        assert!(json["result"].is_null());
        assert!(json["agent_id"].is_null());
    }
}
