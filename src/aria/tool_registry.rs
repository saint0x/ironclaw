//! Aria Tool Registry — manages user-uploaded tool definitions with handler code.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::aria::registry::{Registry, RegistryError, RegistryResult};
use crate::aria::types::Pagination;

/// A registered tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaTool {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
    pub handler_code: String,
    pub handler_hash: String,
    pub version: i32,
    pub sandbox_config: Option<serde_json::Value>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for uploading a tool.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaToolUpload {
    pub name: String,
    pub description: String,
    #[serde(default = "default_params_schema")]
    pub parameters_schema: serde_json::Value,
    pub handler_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_config: Option<serde_json::Value>,
}

fn default_params_schema() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
}

pub struct ToolRegistry {
    pool: Pool,
}

impl ToolRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Generate a system-prompt-injectable summary of available tools for a tenant.
    pub async fn get_prompt_section(&self, tenant_id: &str) -> RegistryResult<String> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT name, description, parameters_schema FROM aria_tools \
                 WHERE tenant_id = $1 AND status = 'active' ORDER BY name",
                &[&tenant_id],
            )
            .await?;

        if rows.is_empty() {
            return Ok(String::new());
        }

        let mut section = String::from("## Available Custom Tools\n\n");
        for row in &rows {
            let name: &str = row.get(0);
            let desc: &str = row.get(1);
            section.push_str(&format!("- **{name}**: {desc}\n"));
        }
        Ok(section)
    }
}

#[async_trait::async_trait]
impl Registry for ToolRegistry {
    type Entry = AriaTool;
    type Upload = AriaToolUpload;

    async fn upload(&self, tenant_id: &str, input: AriaToolUpload) -> RegistryResult<AriaTool> {
        let client = self.pool.get().await?;

        let handler_hash = format!("{:x}", Sha256::digest(input.handler_code.as_bytes()));
        let params_json = serde_json::to_value(&input.parameters_schema)?;

        // Upsert by name
        let row = client
            .query_one(
                "INSERT INTO aria_tools (tenant_id, name, description, parameters_schema, \
                 handler_code, handler_hash, sandbox_config) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   description = EXCLUDED.description, \
                   parameters_schema = EXCLUDED.parameters_schema, \
                   handler_code = EXCLUDED.handler_code, \
                   handler_hash = EXCLUDED.handler_hash, \
                   sandbox_config = EXCLUDED.sandbox_config, \
                   version = aria_tools.version + 1, \
                   status = 'active' \
                 RETURNING id, tenant_id, name, description, parameters_schema, \
                   handler_code, handler_hash, version, sandbox_config, status, \
                   created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.description,
                    &params_json,
                    &input.handler_code,
                    &handler_hash,
                    &input.sandbox_config,
                ],
            )
            .await?;

        Ok(row_to_tool(&row))
    }

    async fn get(&self, id: Uuid) -> RegistryResult<AriaTool> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, parameters_schema, \
                 handler_code, handler_hash, version, sandbox_config, status, \
                 created_at, updated_at \
                 FROM aria_tools WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "tool".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_tool(&row))
    }

    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<AriaTool> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, parameters_schema, \
                 handler_code, handler_hash, version, sandbox_config, status, \
                 created_at, updated_at \
                 FROM aria_tools WHERE tenant_id = $1 AND name = $2 AND status = 'active'",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "tool".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_tool(&row))
    }

    async fn list(&self, tenant_id: &str, pagination: Pagination) -> RegistryResult<Vec<AriaTool>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, parameters_schema, \
                 handler_code, handler_hash, version, sandbox_config, status, \
                 created_at, updated_at \
                 FROM aria_tools WHERE tenant_id = $1 AND status = 'active' \
                 ORDER BY name LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_tool).collect())
    }

    async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_tools WHERE tenant_id = $1 AND status = 'active'",
                &[&tenant_id],
            )
            .await?;
        Ok(row.get(0))
    }

    async fn delete(&self, id: Uuid) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_tools SET status = 'deleted' WHERE id = $1",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "tool".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_tool(row: &tokio_postgres::Row) -> AriaTool {
    AriaTool {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        description: row.get(3),
        parameters_schema: row.get(4),
        handler_code: row.get(5),
        handler_hash: row.get(6),
        version: row.get(7),
        sandbox_config: row.get(8),
        status: row.get(9),
        created_at: row.get(10),
        updated_at: row.get(11),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_hash_is_deterministic() {
        let code = "async function execute(params) { return params; }";
        let h1 = format!("{:x}", Sha256::digest(code.as_bytes()));
        let h2 = format!("{:x}", Sha256::digest(code.as_bytes()));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn default_params_schema_is_valid() {
        let schema = default_params_schema();
        assert_eq!(schema["type"], "object");
    }
}
