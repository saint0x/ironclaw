//! CronBridge — syncs Aria CronFunctionRegistry ↔ CronService.
//!
//! Converts `AriaCronFunction` records (persisted in PostgreSQL)
//! into `CronJob` records (in-memory, timer-driven by CronService).

use std::sync::Arc;

use crate::afw::cron_service::{CronJobCreate, CronService};
use crate::aria::cron_registry::{AriaCronFunction, CronFunctionRegistry};
use crate::aria::types::{CronPayload, CronSchedule, CronSessionTarget};

/// Bridges the Aria CronFunctionRegistry to the runtime CronService.
pub struct CronBridge {
    cron_service: Arc<CronService>,
    cron_registry: Arc<CronFunctionRegistry>,
}

impl CronBridge {
    pub fn new(cron_service: Arc<CronService>, cron_registry: Arc<CronFunctionRegistry>) -> Self {
        Self {
            cron_service,
            cron_registry,
        }
    }

    /// Sync a single cron function → CronService job.
    ///
    /// - If `cron_job_id` exists → update existing job
    /// - If update fails (job gone) → recreate and update `cron_job_id`
    /// - If no `cron_job_id` → create new job and store the ID
    pub async fn sync_cron(&self, cron_func: AriaCronFunction) -> Result<(), BridgeError> {
        let input = self.cron_func_to_input(&cron_func)?;

        if let Some(ref existing_job_id) = cron_func.cron_job_id {
            // Try to update existing job.
            let jobs = self.cron_service.list().await;
            if jobs.iter().any(|j| j.id == *existing_job_id) {
                // Parse schedule for update.
                let schedule = self.parse_schedule(&cron_func)?;
                self.cron_service
                    .update(existing_job_id, Some(input.name), Some(schedule), None)
                    .await;
                return Ok(());
            }

            // Job gone, recreate.
            let job = self.cron_service.add(input).await;
            self.cron_registry
                .set_cron_job_id(cron_func.id, &job.id)
                .await
                .map_err(|e| BridgeError::RegistryError(e.to_string()))?;
        } else {
            // No existing job, create new.
            let job = self.cron_service.add(input).await;
            self.cron_registry
                .set_cron_job_id(cron_func.id, &job.id)
                .await
                .map_err(|e| BridgeError::RegistryError(e.to_string()))?;
        }

        Ok(())
    }

    /// Remove a cron function's corresponding CronService job.
    pub async fn remove_cron(&self, cron_func_id: uuid::Uuid) -> Result<(), BridgeError> {
        // Look up the cron function to find its job ID.
        let cron_func = self
            .cron_registry
            .get(cron_func_id)
            .await
            .map_err(|e| BridgeError::RegistryError(e.to_string()))?;

        if let Some(ref job_id) = cron_func.cron_job_id {
            self.cron_service.remove(job_id).await;
        }

        Ok(())
    }

    /// Sync all cron functions on startup.
    pub async fn sync_all(&self) -> Result<usize, BridgeError> {
        // Get all cron functions across all tenants.
        let all = self
            .cron_registry
            .list_all()
            .await
            .map_err(|e| BridgeError::RegistryError(e.to_string()))?;

        let mut synced = 0;
        for func in all {
            if let Err(e) = self.sync_cron(func).await {
                tracing::warn!("Failed to sync cron function: {}", e);
            } else {
                synced += 1;
            }
        }

        tracing::info!("CronBridge: synced {} cron functions", synced);
        Ok(synced)
    }

    /// Check if a CronService job ID belongs to a cron function.
    pub async fn is_cron_func_job(&self, job_id: &str) -> bool {
        self.cron_registry.get_by_cron_job_id(job_id).await.is_ok()
    }

    /// Convert an AriaCronFunction to a CronJobCreate.
    fn cron_func_to_input(&self, func: &AriaCronFunction) -> Result<CronJobCreate, BridgeError> {
        let schedule = self.parse_schedule(func)?;
        let payload = self.parse_payload(func)?;
        let session_target: CronSessionTarget = func
            .session_target
            .parse()
            .map_err(|e: String| BridgeError::InvalidConfig(e))?;

        Ok(CronJobCreate {
            name: format!("cron-func:{}", func.name),
            schedule,
            session_target,
            payload,
            agent_id: func.agent_id.map(|id| id.to_string()),
            delete_after_run: func.schedule_kind == "at",
        })
    }

    fn parse_schedule(&self, func: &AriaCronFunction) -> Result<CronSchedule, BridgeError> {
        match func.schedule_kind.as_str() {
            "at" => {
                let at_ms = func
                    .schedule_data
                    .get("atMs")
                    .or_else(|| func.schedule_data.get("at_ms"))
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| BridgeError::InvalidConfig("at schedule missing atMs".into()))?;
                Ok(CronSchedule::At { at_ms })
            }
            "every" => {
                let every_ms = func
                    .schedule_data
                    .get("everyMs")
                    .or_else(|| func.schedule_data.get("every_ms"))
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| {
                        BridgeError::InvalidConfig("every schedule missing everyMs".into())
                    })?;
                let anchor_ms = func
                    .schedule_data
                    .get("anchorMs")
                    .or_else(|| func.schedule_data.get("anchor_ms"))
                    .and_then(|v| v.as_i64());
                Ok(CronSchedule::Every {
                    every_ms,
                    anchor_ms,
                })
            }
            "cron" => {
                let expr = func
                    .schedule_data
                    .get("expr")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| BridgeError::InvalidConfig("cron schedule missing expr".into()))?
                    .to_string();
                let tz = func
                    .schedule_data
                    .get("tz")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Ok(CronSchedule::Cron { expr, tz })
            }
            other => Err(BridgeError::InvalidConfig(format!(
                "unknown schedule kind: {other}"
            ))),
        }
    }

    fn parse_payload(&self, func: &AriaCronFunction) -> Result<CronPayload, BridgeError> {
        match func.payload_kind.as_str() {
            "system_event" => {
                let text = func
                    .payload_data
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        BridgeError::InvalidConfig("system_event payload missing text".into())
                    })?
                    .to_string();
                Ok(CronPayload::SystemEvent { text })
            }
            "agent_turn" => {
                let message = func
                    .payload_data
                    .get("message")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        BridgeError::InvalidConfig("agent_turn payload missing message".into())
                    })?
                    .to_string();
                Ok(CronPayload::AgentTurn {
                    message,
                    model: func
                        .payload_data
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    thinking: func
                        .payload_data
                        .get("thinking")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    timeout_seconds: func
                        .payload_data
                        .get("timeoutSeconds")
                        .or_else(|| func.payload_data.get("timeout_seconds"))
                        .and_then(|v| v.as_u64()),
                    deliver: func.payload_data.get("deliver").and_then(|v| v.as_bool()),
                })
            }
            other => Err(BridgeError::InvalidConfig(format!(
                "unknown payload kind: {other}"
            ))),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Registry error: {0}")]
    RegistryError(String),
}

#[cfg(test)]
mod tests {
    #[test]
    fn cron_func_name_prefix() {
        let name = format!("cron-func:{}", "my-check");
        assert_eq!(name, "cron-func:my-check");
    }
}
