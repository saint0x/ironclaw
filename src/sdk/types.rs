//! SDK request/response types for the Aria registry API.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Generic list response with pagination metadata.
#[derive(Debug, Serialize)]
pub struct ListResponse<T: Serialize> {
    pub items: Vec<T>,
    pub count: i64,
    pub offset: i64,
    pub limit: i64,
}

/// Generic success response for mutations.
#[derive(Debug, Serialize)]
pub struct MutationResponse {
    pub id: Uuid,
    pub status: &'static str,
}

/// Error response body.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ErrorResponse {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: None,
        }
    }

    pub fn with_code(error: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: Some(code.into()),
        }
    }
}

/// Pagination query parameters.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

impl PaginationParams {
    pub fn to_pagination(&self) -> crate::aria::types::Pagination {
        crate::aria::types::Pagination {
            offset: self.offset.unwrap_or(0),
            limit: self.limit.unwrap_or(100).min(1000),
        }
    }
}

/// Task status update request.
#[derive(Debug, Deserialize)]
pub struct TaskStatusUpdate {
    pub status: String,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Feed status update request.
#[derive(Debug, Deserialize)]
pub struct StatusUpdate {
    pub status: String,
}

/// Container runtime state update request.
#[derive(Debug, Deserialize)]
pub struct ContainerRuntimeUpdate {
    pub state: String,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub pid: Option<i32>,
}

/// Route context extracted from auth token.
#[derive(Debug, Clone)]
pub struct RouteContext {
    pub tenant_id: String,
    pub user_id: String,
}

/// Memory set request.
#[derive(Debug, Deserialize)]
pub struct MemorySetRequest {
    pub key: String,
    pub value: serde_json::Value,
    #[serde(default = "default_tier")]
    pub tier: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

fn default_tier() -> String {
    "longterm".to_string()
}

/// KV set request.
#[derive(Debug, Deserialize)]
pub struct KvSetRequest {
    pub key: String,
    pub value: serde_json::Value,
}

/// KV query request.
#[derive(Debug, Deserialize)]
pub struct KvQueryParams {
    pub prefix: String,
}

/// Cron job ID link request.
#[derive(Debug, Deserialize)]
pub struct CronJobIdUpdate {
    pub cron_job_id: String,
}
