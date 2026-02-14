//! Aria Team Registry — manages multi-agent team configurations.

use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::aria::registry::{Registry, RegistryError, RegistryResult};
use crate::aria::types::{Pagination, TeamMode};

/// A registered team definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaTeam {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub mode: TeamMode,
    pub members: Vec<serde_json::Value>,
    pub shared_context: Option<serde_json::Value>,
    pub timeout_secs: Option<i64>,
    pub max_turns: Option<i64>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for uploading a team.
#[derive(Debug, Clone, Deserialize)]
pub struct AriaTeamUpload {
    pub name: String,
    pub description: String,
    pub mode: TeamMode,
    pub members: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_context: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<i64>,
}

pub struct TeamRegistry {
    pool: Pool,
}

impl TeamRegistry {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// List all teams across all tenants (admin use).
    pub async fn list_all(&self, pagination: Pagination) -> RegistryResult<Vec<AriaTeam>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, mode, members, \
                 shared_context, timeout_secs, max_turns, status, \
                 created_at, updated_at \
                 FROM aria_teams WHERE status = 'active' \
                 ORDER BY name LIMIT $1 OFFSET $2",
                &[&pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_team).collect())
    }
}

#[async_trait::async_trait]
impl Registry for TeamRegistry {
    type Entry = AriaTeam;
    type Upload = AriaTeamUpload;

    async fn upload(&self, tenant_id: &str, input: AriaTeamUpload) -> RegistryResult<AriaTeam> {
        let client = self.pool.get().await?;

        let members_json = serde_json::to_value(&input.members)?;
        let mode_str = input.mode.as_str();

        let row = client
            .query_one(
                "INSERT INTO aria_teams (tenant_id, name, description, mode, members, \
                 shared_context, timeout_secs, max_turns) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (tenant_id, name) DO UPDATE SET \
                   description = EXCLUDED.description, \
                   mode = EXCLUDED.mode, \
                   members = EXCLUDED.members, \
                   shared_context = EXCLUDED.shared_context, \
                   timeout_secs = EXCLUDED.timeout_secs, \
                   max_turns = EXCLUDED.max_turns, \
                   status = 'active', \
                   updated_at = now() \
                 RETURNING id, tenant_id, name, description, mode, members, \
                   shared_context, timeout_secs, max_turns, status, \
                   created_at, updated_at",
                &[
                    &tenant_id,
                    &input.name,
                    &input.description,
                    &mode_str,
                    &members_json,
                    &input.shared_context,
                    &input.timeout_secs,
                    &input.max_turns,
                ],
            )
            .await?;

        Ok(row_to_team(&row))
    }

    async fn get(&self, id: Uuid) -> RegistryResult<AriaTeam> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, mode, members, \
                 shared_context, timeout_secs, max_turns, status, \
                 created_at, updated_at \
                 FROM aria_teams WHERE id = $1 AND status = 'active'",
                &[&id],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "team".into(),
                id: id.to_string(),
            })?;
        Ok(row_to_team(&row))
    }

    async fn get_by_name(&self, tenant_id: &str, name: &str) -> RegistryResult<AriaTeam> {
        let client = self.pool.get().await?;
        let row = client
            .query_opt(
                "SELECT id, tenant_id, name, description, mode, members, \
                 shared_context, timeout_secs, max_turns, status, \
                 created_at, updated_at \
                 FROM aria_teams WHERE tenant_id = $1 AND name = $2 AND status = 'active'",
                &[&tenant_id, &name],
            )
            .await?
            .ok_or_else(|| RegistryError::NotFound {
                entity: "team".into(),
                id: format!("{tenant_id}:{name}"),
            })?;
        Ok(row_to_team(&row))
    }

    async fn list(&self, tenant_id: &str, pagination: Pagination) -> RegistryResult<Vec<AriaTeam>> {
        let client = self.pool.get().await?;
        let rows = client
            .query(
                "SELECT id, tenant_id, name, description, mode, members, \
                 shared_context, timeout_secs, max_turns, status, \
                 created_at, updated_at \
                 FROM aria_teams WHERE tenant_id = $1 AND status = 'active' \
                 ORDER BY name LIMIT $2 OFFSET $3",
                &[&tenant_id, &pagination.limit, &pagination.offset],
            )
            .await?;
        Ok(rows.iter().map(row_to_team).collect())
    }

    async fn count(&self, tenant_id: &str) -> RegistryResult<i64> {
        let client = self.pool.get().await?;
        let row = client
            .query_one(
                "SELECT COUNT(*) FROM aria_teams WHERE tenant_id = $1 AND status = 'active'",
                &[&tenant_id],
            )
            .await?;
        Ok(row.get(0))
    }

    async fn delete(&self, id: Uuid) -> RegistryResult<()> {
        let client = self.pool.get().await?;
        let n = client
            .execute(
                "UPDATE aria_teams SET status = 'deleted', updated_at = now() WHERE id = $1",
                &[&id],
            )
            .await?;
        if n == 0 {
            return Err(RegistryError::NotFound {
                entity: "team".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }
}

fn row_to_team(row: &tokio_postgres::Row) -> AriaTeam {
    let mode_str: String = row.get(4);
    AriaTeam {
        id: row.get(0),
        tenant_id: row.get(1),
        name: row.get(2),
        description: row.get(3),
        mode: mode_str
            .parse::<TeamMode>()
            .unwrap_or(TeamMode::Coordinator),
        members: serde_json::from_value(row.get::<_, serde_json::Value>(5)).unwrap_or_default(),
        shared_context: row.get(6),
        timeout_secs: row.get(7),
        max_turns: row.get(8),
        status: row.get(9),
        created_at: row.get(10),
        updated_at: row.get(11),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_upload_deserializes() {
        let json = serde_json::json!({
            "name": "review-team",
            "description": "Code review team",
            "mode": "round_robin",
            "members": [
                {"agent_name": "reviewer-1", "role": "reviewer"},
                {"agent_name": "reviewer-2", "role": "reviewer"}
            ],
            "timeout_secs": 300,
            "max_turns": 10
        });
        let upload: AriaTeamUpload = serde_json::from_value(json).unwrap();
        assert_eq!(upload.name, "review-team");
        assert_eq!(upload.mode, TeamMode::RoundRobin);
        assert_eq!(upload.members.len(), 2);
        assert_eq!(upload.timeout_secs, Some(300));
    }

    #[test]
    fn team_mode_roundtrip() {
        for mode in &[
            TeamMode::Coordinator,
            TeamMode::RoundRobin,
            TeamMode::Delegate,
            TeamMode::Parallel,
            TeamMode::Sequential,
        ] {
            let parsed: TeamMode = mode.as_str().parse().unwrap();
            assert_eq!(*mode, parsed);
        }
    }
}
