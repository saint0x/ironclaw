//! Aria Agent Registry — manages agent definitions with model config and tool bindings.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{Registry, RegistryError, RegistryResult};
use crate::aria::types::Pagination;

/// A registered agent definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaAgent {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub model: String,
    pub system_prompt: String,
    pub tools: serde_json::Value,
    pub thinking_level: Option<String>,
    pub max_retries: i32,
    pub timeout_secs: i64,
    pub runtime_injections: serde_json::Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for uploading (upserting) an agent.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaAgentUpload {
    pub name: String,
    pub description: String,
    pub model: String,
    pub system_prompt: String,
    #[serde(default = "default_tools")]
    pub tools: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    #[serde(default = "default_max_retries")]
    pub max_retries: i32,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: i64,
    #[serde(default = "default_runtime_injections")]
    pub runtime_injections: serde_json::Value,
}

fn default_tools() -> serde_json::Value {
    serde_json::json!([])
}

fn default_max_retries() -> i32 {
    3
}

fn default_timeout_secs() -> i64 {
    300
}

fn default_runtime_injections() -> serde_json::Value {
    serde_json::json!([])
}

pub struct AgentRegistry {
    pool: Pool,
}

impl AgentRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Generate a system-prompt-injectable summary of available agents for a tenant.
    pub async fn get_prompt_section(&self, tenant_id: &str) -> RegistryResult<String> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT name, description, model, tools FROM aria_agents \
                 WHERE tenant_id = $1 AND status = 'active' ORDER BY name",
                &[&tenant_id],
            )
            .await?;

        if rows.is_empty() {
            return Ok(String::new());
        }

        let mut section = String::from("## Available Agents\n\n");
        for row in &rows {
            let name: &str = row.get(0);
            let desc: &str = row.get(1);
            let model: &str = row.get(2);
            let tools: serde_json::Value = row.get(3);
            let tool_count = tools.as_array().map(|a| a.len()).unwrap_or(0);
            section.push_str(&format!(
                "- **{name}**: {desc} (model: {model}, tools: {tool_count})\n"
            ));
        }
        Ok(section)
    }
}

#[async_trait::async_trait]
impl Registry for AgentRegistry {
    type Entry = AriaAgent;
    type Upload = AriaAgentUpload;

    async fn upload(&self, tenant_id: &str, input: AriaAgentUpload) -> RegistryResult<AriaAgent> {
        let client = self.pool.get().await?;

        let tools_json = serde_json::to_value(&input.tools)?;
        let injections_json = serde_json::to_value(&input.runtime_injections)?;

        let row = client
            .query_one(
                "INSERT INTO aria_agents (tenant_id, name, description, model, system_prompt, \
                 tools, thinking_level, max_retries, timeout_secs, runtime_injections) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   description = EXCLUDED.description, \
                   model = EXCLUDED.model, \
                   system_prompt = EXCLUDED.system_prompt, \
                   tools = EXCLUDED.tools, \
                   thinking_level = EXCLUDED.thinking_level, \
                   max_retries = EXCLUDED.max_retries, \
                   timeout_secs = EXCLUDED.timeout_secs, \
                   runtime_injections = EXCLUDED.runtime_injections, \
                   status = 'active', \
                   updated_at = NOW() \
                 RETURNING id, tenant_id, name, description, model, system_prompt, \
                   tools, thinking_level, max_retries, timeout_secs, runtime_injections, \
                   status, created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.description,
                    &input.model,
                    &input.system_prompt,
                    &tools_json,
                    &input.thinking_level,
                    &input.max_retries,
                    &input.timeout_secs,
                    &injections_json,
                ],
            )
            .await?;

        Ok(row_to_agent(&row))
    }

    async fn get(&self, id: Uuid) -> RegistryResult<AriaAgent> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, model, system_prompt, \
                 tools, thinking_level, max_retries, timeout_secs, runtime_injections, \
                 status, created_at, updated_at \
                 FROM aria_agents WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "agent".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_agent(&row))
    }

    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<AriaAgent> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, model, system_prompt, \
                 tools, thinking_level, max_retries, timeout_secs, runtime_injections, \
                 status, created_at, updated_at \
                 FROM aria_agents WHERE tenant_id = $1 AND name = $2 AND status = 'active'",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "agent".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_agent(&row))
    }

    async fn list(
        &self,
        tenant_id: &str,
        pagination: Pagination,
    ) -> RegistryResult<Vec<AriaAgent>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, model, system_prompt, \
                 tools, thinking_level, max_retries, timeout_secs, runtime_injections, \
                 status, created_at, updated_at \
                 FROM aria_agents WHERE tenant_id = $1 AND status = 'active' \
                 ORDER BY name LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_agent).collect())
    }

    async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_agents WHERE tenant_id = $1 AND status = 'active'",
                &[&tenant_id],
            )
            .await?;
        Ok(row.get(0))
    }

    async fn delete(&self, id: Uuid) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_agents SET status = 'deleted', updated_at = NOW() WHERE id = $1",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "agent".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_agent(row: &tokio_postgres::Row) -> AriaAgent {
    AriaAgent {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        description: row.get(3),
        model: row.get(4),
        system_prompt: row.get(5),
        tools: row.get(6),
        thinking_level: row.get(7),
        max_retries: row.get(8),
        timeout_secs: row.get(9),
        runtime_injections: row.get(10),
        status: row.get(11),
        created_at: row.get(12),
        updated_at: row.get(13),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tools_is_empty_array() {
        let tools = default_tools();
        assert!(tools.is_array());
        assert_eq!(tools.as_array().unwrap().len(), 0);
    }

    #[test]
    fn default_runtime_injections_is_empty_array() {
        let injections = default_runtime_injections();
        assert!(injections.is_array());
        assert_eq!(injections.as_array().unwrap().len(), 0);
    }

    #[test]
    fn default_max_retries_value() {
        assert_eq!(default_max_retries(), 3);
    }

    #[test]
    fn default_timeout_secs_value() {
        assert_eq!(default_timeout_secs(), 300);
    }

    #[test]
    fn agent_upload_deserializes_with_defaults() {
        let json = serde_json::json!({
            "name": "test-agent",
            "description": "A test agent",
            "model": "claude-3-5-sonnet-20241022",
            "system_prompt": "You are a helpful assistant."
        });
        let upload: AriaAgentUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.name, "test-agent");
        assert_eq!(upload.max_retries, 3);
        assert_eq!(upload.timeout_secs, 300);
        assert!(upload.tools.is_array());
        assert!(upload.thinking_level.is_none());
        assert!(upload.runtime_injections.is_array());
    }

    #[test]
    fn agent_upload_deserializes_with_explicit_values() {
        let json = serde_json::json!({
            "name": "custom-agent",
            "description": "A custom agent",
            "model": "gpt-4o",
            "system_prompt": "Be creative.",
            "tools": ["search", "calculator"],
            "thinking_level": "high",
            "max_retries": 5,
            "timeout_secs": 600,
            "runtime_injections": [{"injection_type": "memory", "config": {}}]
        });
        let upload: AriaAgentUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.name, "custom-agent");
        assert_eq!(upload.max_retries, 5);
        assert_eq!(upload.timeout_secs, 600);
        assert_eq!(upload.thinking_level.as_deref(), Some("high"));
        assert_eq!(upload.tools.as_array().unwrap().len(), 2);
        assert_eq!(upload.runtime_injections.as_array().unwrap().len(), 1);
    }
}
