//! Aria Pipeline Registry — manages multi-step pipeline definitions and their runs.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{Registry, RegistryError, RegistryResult};
use crate::aria::types::Pagination;

/// A registered pipeline definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaPipeline {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub steps: serde_json::Value,
    pub variables: Option<serde_json::Value>,
    pub timeout_secs: Option<i64>,
    pub max_parallel: Option<i32>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for uploading a pipeline.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaPipelineUpload {
    pub name: String,
    pub description: String,
    pub steps: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<i32>,
}

/// A single execution run of a pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaPipelineRun {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub tenant_id: String,
    pub variables: Option<serde_json::Value>,
    pub status: String,
    pub current_step: Option<String>,
    pub step_results: Option<serde_json::Value>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub struct PipelineRegistry {
    pool: Pool,
}

impl PipelineRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// List all pipelines across all tenants (admin use).
    pub async fn list_all(&self, pagination: Pagination) -> RegistryResult<Vec<AriaPipeline>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, steps, variables, \
                 timeout_secs, max_parallel, status, created_at, updated_at \
                 FROM aria_pipelines WHERE status = 'active' \
                 ORDER BY name LIMIT $1 OFFSET $2",
                &[&pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_pipeline).collect())
    }

    /// Create a new pipeline run.
    pub async fn create_run(
        &self,
        pipeline_id: Uuid,
        tenant_id: &str,
        variables: Option<serde_json::Value>,
    ) -> RegistryResult<AriaPipelineRun> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "INSERT INTO aria_pipeline_runs (pipeline_id, tenant_id, variables, status) \
                 VALUES ($1, $2, $3, 'pending') \
                 RETURNING id, pipeline_id, tenant_id, variables, status, \
                   current_step, step_results, started_at, completed_at, \
                   error_message, created_at",
                &[&pipeline_id, &tenant_id, &variables],
            )
            .await?;
        Ok(row_to_run(&row))
    }

    /// Update a pipeline run's state.
    pub async fn update_run(
        &self,
        run_id: Uuid,
        status: &str,
        current_step: Option<&str>,
        step_results: Option<serde_json::Value>,
        error: Option<&str>,
    ) -> RegistryResult<AriaPipelineRun> {
        let client = self.pool.get().await?;

        // Set started_at on first transition away from pending, completed_at on terminal states.
        let row = client
            .query_opt(
                "UPDATE aria_pipeline_runs SET \
                   status = $2, \
                   current_step = COALESCE($3, current_step), \
                   step_results = COALESCE($4, step_results), \
                   error_message = COALESCE($5, error_message), \
                   started_at = CASE WHEN started_at IS NULL AND $2 != 'pending' \
                                     THEN now() ELSE started_at END, \
                   completed_at = CASE WHEN $2 IN ('completed', 'failed', 'cancelled') \
                                       THEN now() ELSE completed_at END \
                 WHERE id = $1 \
                 RETURNING id, pipeline_id, tenant_id, variables, status, \
                   current_step, step_results, started_at, completed_at, \
                   error_message, created_at",
                &[&run_id, &status, &current_step, &step_results, &error],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "pipeline_run".into(),
                id: run_id.to_string(),
            })?;
        Ok(row_to_run(&row))
    }

    /// Get a pipeline run by ID.
    pub async fn get_run(&self, run_id: Uuid) -> RegistryResult<AriaPipelineRun> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, pipeline_id, tenant_id, variables, status, \
                 current_step, step_results, started_at, completed_at, \
                 error_message, created_at \
                 FROM aria_pipeline_runs WHERE id = $1",
                &[&run_id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "pipeline_run".into(),
                id: run_id.to_string(),
            })?;
        Ok(row_to_run(&row))
    }

    /// List runs for a given pipeline, ordered by most recent first.
    pub async fn list_runs(
        &self,
        pipeline_id: Uuid,
        limit: i64,
    ) -> RegistryResult<Vec<AriaPipelineRun>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, pipeline_id, tenant_id, variables, status, \
                 current_step, step_results, started_at, completed_at, \
                 error_message, created_at \
                 FROM aria_pipeline_runs WHERE pipeline_id = $1 \
                 ORDER BY created_at DESC LIMIT $2",
                &[&pipeline_id, &limit],
            )
            .await?;
        Ok(rows.iter().map(row_to_run).collect())
    }
}

#[async_trait::async_trait]
impl Registry for PipelineRegistry {
    type Entry = AriaPipeline;
    type Upload = AriaPipelineUpload;

    async fn upload(
        &self,
        tenant_id: &str,
        input: AriaPipelineUpload,
    ) -> RegistryResult<AriaPipeline> {
        let client = self.pool.get().await?;

        let row = client
            .query_one(
                "INSERT INTO aria_pipelines (tenant_id, name, description, steps, \
                 variables, timeout_secs, max_parallel) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   description = EXCLUDED.description, \
                   steps = EXCLUDED.steps, \
                   variables = EXCLUDED.variables, \
                   timeout_secs = EXCLUDED.timeout_secs, \
                   max_parallel = EXCLUDED.max_parallel, \
                   status = 'active', \
                   updated_at = now() \
                 RETURNING id, tenant_id, name, description, steps, variables, \
                   timeout_secs, max_parallel, status, created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.description,
                    &input.steps,
                    &input.variables,
                    &input.timeout_secs,
                    &input.max_parallel,
                ],
            )
            .await?;

        Ok(row_to_pipeline(&row))
    }

    async fn get(&self, id: Uuid) -> RegistryResult<AriaPipeline> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, steps, variables, \
                 timeout_secs, max_parallel, status, created_at, updated_at \
                 FROM aria_pipelines WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "pipeline".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_pipeline(&row))
    }

    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<AriaPipeline> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, steps, variables, \
                 timeout_secs, max_parallel, status, created_at, updated_at \
                 FROM aria_pipelines WHERE tenant_id = $1 AND name = $2 AND status = 'active'",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "pipeline".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_pipeline(&row))
    }

    async fn list(
        &self,
        tenant_id: &str,
        pagination: Pagination,
    ) -> RegistryResult<Vec<AriaPipeline>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, steps, variables, \
                 timeout_secs, max_parallel, status, created_at, updated_at \
                 FROM aria_pipelines WHERE tenant_id = $1 AND status = 'active' \
                 ORDER BY name LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_pipeline).collect())
    }

    async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_pipelines \
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
                "UPDATE aria_pipelines SET status = 'deleted', updated_at = now() WHERE id = $1",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "pipeline".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_pipeline(row: &tokio_postgres::Row) -> AriaPipeline {
    AriaPipeline {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        description: row.get(3),
        steps: row.get(4),
        variables: row.get(5),
        timeout_secs: row.get(6),
        max_parallel: row.get(7),
        status: row.get(8),
        created_at: row.get(9),
        updated_at: row.get(10),
    }
}

fn row_to_run(row: &tokio_postgres::Row) -> AriaPipelineRun {
    AriaPipelineRun {
        id: row.get(0),
        pipeline_id: row.get(1),
        tenant_id: row.get(2),
        variables: row.get(3),
        status: row.get(4),
        current_step: row.get(5),
        step_results: row.get(6),
        started_at: row.get(7),
        completed_at: row.get(8),
        error_message: row.get(9),
        created_at: row.get(10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_upload_deserializes() {
        let json = serde_json::json!({
            "name": "deploy-pipeline",
            "description": "CI/CD deployment pipeline",
            "steps": [
                {
                    "name": "build",
                    "execute": "tool:shell",
                    "params": {"command": "cargo build --release"},
                    "depends_on": []
                },
                {
                    "name": "test",
                    "execute": "tool:shell",
                    "params": {"command": "cargo test"},
                    "depends_on": ["build"]
                }
            ],
            "variables": {"environment": "staging"},
            "timeout_secs": 600,
            "max_parallel": 2
        });
        let upload: AriaPipelineUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.name, "deploy-pipeline");
        assert!(upload.steps.is_array());
        assert_eq!(upload.max_parallel, Some(2));
    }

    #[test]
    fn pipeline_run_has_correct_fields() {
        let run = AriaPipelineRun {
            id: Uuid::new_v4(),
            pipeline_id: Uuid::new_v4(),
            tenant_id: "tenant-1".into(),
            variables: Some(serde_json::json!({"env": "prod"})),
            status: "pending".into(),
            current_step: None,
            step_results: None,
            started_at: None,
            completed_at: None,
            error_message: None,
            created_at: Utc::now(),
        };
        assert_eq!(run.status, "pending");
        assert!(run.current_step.is_none());
    }
}
