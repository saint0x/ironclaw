//! SDK HTTP API routes for all Aria registries.
//!
//! Provides a unified REST API surface for SDK clients (macOS app, CLI, etc.)
//! to interact with all 11 Aria registries.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use serde::Serialize;
use uuid::Uuid;

use crate::aria::AriaRegistries;
use crate::aria::registry::{Registry, RegistryError};
use crate::llm::LlmProvider;
use crate::safety::SafetyLayer;
use crate::sandbox::SandboxManager;
use crate::sdk::types::*;
use crate::tools::ToolRegistry;

/// Shared state for all SDK routes.
#[derive(Clone)]
pub struct SdkState {
    pub registries: Arc<AriaRegistries>,
    /// LLM provider for execute endpoints (optional; execute routes return 501 if absent).
    pub llm: Option<Arc<dyn LlmProvider>>,
    /// Safety layer for execute endpoints.
    pub safety: Option<Arc<SafetyLayer>>,
    /// Tool registry for pipeline step dispatch.
    pub tool_registry: Option<Arc<ToolRegistry>>,
    /// Sandbox manager for container execution (Quilt).
    pub sandbox: Option<Arc<SandboxManager>>,
}

/// Build the full SDK router.
pub fn router(state: SdkState) -> Router {
    Router::new()
        .merge(tool_routes())
        .merge(agent_routes())
        .merge(memory_routes())
        .merge(task_routes())
        .merge(feed_routes())
        .merge(cron_routes())
        .merge(kv_routes())
        .merge(team_routes())
        .merge(pipeline_routes())
        .merge(container_routes())
        .merge(network_routes())
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract tenant_id from request headers, defaulting to "default".
fn tenant_id(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("default")
        .to_string()
}

/// Map RegistryError to HTTP status + JSON body.
fn registry_err(e: RegistryError) -> (StatusCode, Json<ErrorResponse>) {
    match &e {
        RegistryError::NotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::with_code(e.to_string(), "NOT_FOUND")),
        ),
        RegistryError::Duplicate { .. } => (
            StatusCode::CONFLICT,
            Json(ErrorResponse::with_code(e.to_string(), "DUPLICATE")),
        ),
        RegistryError::InvalidInput(_) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(e.to_string(), "INVALID_INPUT")),
        ),
        RegistryError::Database(_) | RegistryError::Serialization(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(e.to_string())),
        ),
    }
}

fn parse_uuid(s: &str) -> Result<Uuid, (StatusCode, Json<ErrorResponse>)> {
    Uuid::parse_str(s).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(
                format!("Invalid UUID: {s}"),
                "INVALID_UUID",
            )),
        )
    })
}

fn ok_mutation(id: Uuid) -> Json<MutationResponse> {
    Json(MutationResponse { id, status: "ok" })
}

fn ok_deleted(id: Uuid) -> Json<MutationResponse> {
    Json(MutationResponse {
        id,
        status: "deleted",
    })
}

type ApiResult<T> = Result<(StatusCode, Json<T>), (StatusCode, Json<ErrorResponse>)>;

fn ok<T: Serialize>(val: T) -> ApiResult<T> {
    Ok((StatusCode::OK, Json(val)))
}

fn created<T: Serialize>(val: T) -> ApiResult<T> {
    Ok((StatusCode::CREATED, Json(val)))
}

// ---------------------------------------------------------------------------
// Tool routes
// ---------------------------------------------------------------------------

fn tool_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/tools", post(tool_upload).get(tool_list))
        .route("/api/aria/tools/{id}", get(tool_get).delete(tool_delete))
        .route("/api/aria/tools/name/{name}", get(tool_get_by_name))
}

async fn tool_upload(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::tool_registry::AriaToolUpload>,
) -> ApiResult<crate::aria::tool_registry::AriaTool> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .tools
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn tool_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::tool_registry::AriaTool>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .tools
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .tools
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn tool_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::tool_registry::AriaTool> {
    let id = parse_uuid(&id)?;
    let entry = state.registries.tools.get(id).await.map_err(registry_err)?;
    ok(entry)
}

async fn tool_get_by_name(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<crate::aria::tool_registry::AriaTool> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .tools
        .get_by_name(&tid, &name)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn tool_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .tools
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

// ---------------------------------------------------------------------------
// Agent routes
// ---------------------------------------------------------------------------

fn agent_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/agents", post(agent_upload).get(agent_list))
        .route("/api/aria/agents/{id}", get(agent_get).delete(agent_delete))
        .route("/api/aria/agents/{id}/execute", post(agent_execute))
        .route("/api/aria/agents/name/{name}", get(agent_get_by_name))
}

async fn agent_upload(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::agent_registry::AriaAgentUpload>,
) -> ApiResult<crate::aria::agent_registry::AriaAgent> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .agents
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn agent_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::agent_registry::AriaAgent>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .agents
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .agents
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn agent_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::agent_registry::AriaAgent> {
    let id = parse_uuid(&id)?;
    let entry = state
        .registries
        .agents
        .get(id)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn agent_get_by_name(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<crate::aria::agent_registry::AriaAgent> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .agents
        .get_by_name(&tid, &name)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn agent_execute(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Json(input): Json<ExecuteRequest>,
) -> ApiResult<ExecuteResponse> {
    let id = parse_uuid(&id)?;

    let llm = state.llm.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(ErrorResponse::with_code(
                "Execute not available (no LLM configured)",
                "NOT_CONFIGURED",
            )),
        )
    })?;

    let agent = state
        .registries
        .agents
        .get(id)
        .await
        .map_err(registry_err)?;

    // Build messages: system prompt from agent config + user input.
    let mut messages = Vec::new();
    if !agent.system_prompt.is_empty() {
        messages.push(crate::llm::ChatMessage::system(&agent.system_prompt));
    }
    messages.push(crate::llm::ChatMessage::user(&input.input));

    let start = std::time::Instant::now();
    let request = crate::llm::CompletionRequest::new(messages);
    let response = llm.complete(request).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(e.to_string())),
        )
    })?;

    ok(ExecuteResponse {
        run_id: Uuid::new_v4(),
        status: "completed".into(),
        output: Some(response.content),
        error: None,
        duration_ms: start.elapsed().as_millis() as u64,
        details: Some(serde_json::json!({
            "agent": agent.name,
            "model": agent.model,
            "input_tokens": response.input_tokens,
            "output_tokens": response.output_tokens,
        })),
    })
}

async fn agent_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .agents
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

// ---------------------------------------------------------------------------
// Memory routes
// ---------------------------------------------------------------------------

fn memory_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/memory", post(memory_set).get(memory_list))
        .route("/api/aria/memory/sweep", post(memory_sweep))
        .route(
            "/api/aria/memory/{key}",
            get(memory_get).delete(memory_delete),
        )
}

async fn memory_set(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<MemorySetRequest>,
) -> ApiResult<crate::aria::memory_registry::AriaMemoryEntry> {
    let tid = tenant_id(&headers);
    let tier = input
        .tier
        .parse::<crate::aria::types::MemoryTier>()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(e, "INVALID_TIER")),
            )
        })?;

    let expires_at = input
        .ttl_seconds
        .map(|ttl| chrono::Utc::now() + chrono::Duration::seconds(ttl));

    let entry = state
        .registries
        .memory
        .set(
            &tid,
            crate::aria::memory_registry::AriaMemorySet {
                key: input.key,
                value: input.value,
                tier,
                session_id: input.session_id,
                expires_at,
            },
        )
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn memory_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::memory_registry::AriaMemoryEntry>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .memory
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .memory
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn memory_get(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
) -> ApiResult<crate::aria::memory_registry::AriaMemoryEntry> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .memory
        .get_by_key(&tid, &key, None)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn memory_delete(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
) -> ApiResult<MutationResponse> {
    let tid = tenant_id(&headers);
    state
        .registries
        .memory
        .delete_by_key(&tid, &key)
        .await
        .map_err(registry_err)?;
    ok(MutationResponse {
        id: Uuid::nil(),
        status: "deleted",
    })
}

async fn memory_sweep(State(state): State<SdkState>) -> ApiResult<serde_json::Value> {
    let count = state
        .registries
        .memory
        .sweep_expired()
        .await
        .map_err(registry_err)?;
    ok(serde_json::json!({ "swept": count }))
}

// ---------------------------------------------------------------------------
// Task routes
// ---------------------------------------------------------------------------

fn task_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/tasks", post(task_create).get(task_list))
        .route("/api/aria/tasks/{id}", get(task_get).delete(task_delete))
        .route("/api/aria/tasks/{id}/status", patch(task_update_status))
}

async fn task_create(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::task_registry::AriaTaskCreate>,
) -> ApiResult<crate::aria::task_registry::AriaTask> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .tasks
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn task_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::task_registry::AriaTask>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .tasks
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .tasks
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn task_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::task_registry::AriaTask> {
    let id = parse_uuid(&id)?;
    let entry = state.registries.tasks.get(id).await.map_err(registry_err)?;
    ok(entry)
}

async fn task_update_status(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Json(input): Json<TaskStatusUpdate>,
) -> ApiResult<crate::aria::task_registry::AriaTask> {
    let id = parse_uuid(&id)?;
    let status = input
        .status
        .parse::<crate::aria::types::TaskStatus>()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(e, "INVALID_STATUS")),
            )
        })?;
    let entry = state
        .registries
        .tasks
        .update_status(id, status, input.result, input.error)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn task_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .tasks
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

// ---------------------------------------------------------------------------
// Feed routes
// ---------------------------------------------------------------------------

fn feed_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/feeds", post(feed_upload).get(feed_list))
        .route("/api/aria/feeds/{id}", get(feed_get).delete(feed_delete))
        .route("/api/aria/feeds/{id}/items", get(feed_list_items))
        .route("/api/aria/feeds/{id}/status", patch(feed_update_status))
        .route("/api/aria/feeds/name/{name}", get(feed_get_by_name))
}

async fn feed_upload(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::feed_registry::AriaFeedUpload>,
) -> ApiResult<crate::aria::feed_registry::AriaFeed> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .feeds
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn feed_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::feed_registry::AriaFeed>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .feeds
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .feeds
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn feed_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::feed_registry::AriaFeed> {
    let id = parse_uuid(&id)?;
    let entry = state.registries.feeds.get(id).await.map_err(registry_err)?;
    ok(entry)
}

async fn feed_get_by_name(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<crate::aria::feed_registry::AriaFeed> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .feeds
        .get_by_name(&tid, &name)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn feed_list_items(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> ApiResult<Vec<crate::aria::feed_registry::AriaFeedItem>> {
    let id = parse_uuid(&id)?;
    let limit = params.limit.unwrap_or(50).min(200);
    let items = state
        .registries
        .feeds
        .list_items(id, limit)
        .await
        .map_err(registry_err)?;
    ok(items)
}

async fn feed_update_status(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Json(input): Json<StatusUpdate>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .feeds
        .update_status(id, &input.status)
        .await
        .map_err(registry_err)?;
    ok(ok_mutation(id).0)
}

async fn feed_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .feeds
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

// ---------------------------------------------------------------------------
// Cron routes
// ---------------------------------------------------------------------------

fn cron_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/cron", post(cron_upload).get(cron_list))
        .route("/api/aria/cron/{id}", get(cron_get).delete(cron_delete))
        .route("/api/aria/cron/{id}/runs", get(cron_list_runs))
        .route("/api/aria/cron/{id}/status", patch(cron_update_status))
        .route("/api/aria/cron/{id}/job-id", patch(cron_set_job_id))
        .route("/api/aria/cron/name/{name}", get(cron_get_by_name))
}

async fn cron_upload(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::cron_registry::AriaCronFunctionUpload>,
) -> ApiResult<crate::aria::cron_registry::AriaCronFunction> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .cron_functions
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn cron_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<Vec<crate::aria::cron_registry::AriaCronFunction>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .cron_functions
        .get_by_tenant(&tid, pagination)
        .await
        .map_err(registry_err)?;
    ok(items)
}

async fn cron_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::cron_registry::AriaCronFunction> {
    let id = parse_uuid(&id)?;
    let entry = state
        .registries
        .cron_functions
        .get(id)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn cron_get_by_name(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<crate::aria::cron_registry::AriaCronFunction> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .cron_functions
        .get_by_name(&tid, &name)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn cron_list_runs(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> ApiResult<Vec<crate::aria::cron_registry::AriaCronRun>> {
    let id = parse_uuid(&id)?;
    let limit = params.limit.unwrap_or(50).min(200);
    let runs = state
        .registries
        .cron_functions
        .list_runs(id, limit)
        .await
        .map_err(registry_err)?;
    ok(runs)
}

async fn cron_update_status(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Json(input): Json<StatusUpdate>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .cron_functions
        .update_status(id, &input.status)
        .await
        .map_err(registry_err)?;
    ok(ok_mutation(id).0)
}

async fn cron_set_job_id(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Json(input): Json<CronJobIdUpdate>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .cron_functions
        .set_cron_job_id(id, &input.cron_job_id)
        .await
        .map_err(registry_err)?;
    ok(ok_mutation(id).0)
}

async fn cron_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .cron_functions
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

// ---------------------------------------------------------------------------
// KV routes
// ---------------------------------------------------------------------------

fn kv_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/kv", post(kv_set).get(kv_list))
        .route("/api/aria/kv/query", get(kv_query))
        .route("/api/aria/kv/{key}", get(kv_get).delete(kv_delete))
}

async fn kv_set(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<KvSetRequest>,
) -> ApiResult<crate::aria::kv_registry::AriaKvEntry> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .kv
        .set(&tid, &input.key, input.value)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn kv_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::kv_registry::AriaKvEntry>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .kv
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .kv
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn kv_get(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
) -> ApiResult<crate::aria::kv_registry::AriaKvEntry> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .kv
        .get(&tid, &key)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn kv_query(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<KvQueryParams>,
) -> ApiResult<Vec<crate::aria::kv_registry::AriaKvEntry>> {
    let tid = tenant_id(&headers);
    let entries = state
        .registries
        .kv
        .query(&tid, &params.prefix)
        .await
        .map_err(registry_err)?;
    ok(entries)
}

async fn kv_delete(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
) -> ApiResult<MutationResponse> {
    let tid = tenant_id(&headers);
    state
        .registries
        .kv
        .delete(&tid, &key)
        .await
        .map_err(registry_err)?;
    ok(MutationResponse {
        id: Uuid::nil(),
        status: "deleted",
    })
}

// ---------------------------------------------------------------------------
// Team routes
// ---------------------------------------------------------------------------

fn team_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/teams", post(team_upload).get(team_list))
        .route("/api/aria/teams/{id}", get(team_get).delete(team_delete))
        .route("/api/aria/teams/{id}/execute", post(team_execute))
        .route("/api/aria/teams/name/{name}", get(team_get_by_name))
}

async fn team_upload(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::team_registry::AriaTeamUpload>,
) -> ApiResult<crate::aria::team_registry::AriaTeam> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .teams
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn team_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::team_registry::AriaTeam>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .teams
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .teams
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn team_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::team_registry::AriaTeam> {
    let id = parse_uuid(&id)?;
    let entry = state.registries.teams.get(id).await.map_err(registry_err)?;
    ok(entry)
}

async fn team_get_by_name(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<crate::aria::team_registry::AriaTeam> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .teams
        .get_by_name(&tid, &name)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn team_execute(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<ExecuteRequest>,
) -> ApiResult<ExecuteResponse> {
    let id = parse_uuid(&id)?;
    let tid = tenant_id(&headers);

    let llm = state.llm.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(ErrorResponse::with_code(
                "Execute not available (no LLM configured)",
                "NOT_CONFIGURED",
            )),
        )
    })?;
    let safety = state.safety.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(ErrorResponse::with_code(
                "Execute not available (no safety layer)",
                "NOT_CONFIGURED",
            )),
        )
    })?;

    // Load team and its agents.
    let team = state.registries.teams.get(id).await.map_err(registry_err)?;
    let mut agents = Vec::new();
    // Members can be string names or objects with a "name" field.
    let member_names: Vec<String> = team
        .members
        .iter()
        .filter_map(|m| match m {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(obj) => {
                obj.get("name").and_then(|n| n.as_str()).map(String::from)
            }
            _ => None,
        })
        .collect();
    for name in &member_names {
        match state.registries.agents.get_by_name(&tid, name).await {
            Ok(agent) => agents.push(agent),
            Err(e) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse::with_code(
                        format!("Agent '{}' not found: {}", name, e),
                        "AGENT_NOT_FOUND",
                    )),
                ));
            }
        }
    }

    let executor = crate::afw::TeamExecutor::new(Arc::clone(llm), Arc::clone(safety));
    let result = executor
        .execute(&team, &agents, &input.input)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(e.to_string())),
            )
        })?;

    ok(ExecuteResponse {
        run_id: Uuid::new_v4(),
        status: "completed".into(),
        output: Some(result.final_output),
        error: None,
        duration_ms: result.duration.as_millis() as u64,
        details: Some(serde_json::json!({
            "mode": format!("{:?}", result.mode),
            "total_turns": result.total_turns,
            "agent_outputs": result.agent_outputs.iter().map(|a| serde_json::json!({
                "agent": a.agent_name,
                "output": a.output,
                "turn": a.turn_number,
            })).collect::<Vec<_>>(),
        })),
    })
}

async fn team_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .teams
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

// ---------------------------------------------------------------------------
// Pipeline routes
// ---------------------------------------------------------------------------

fn pipeline_routes() -> Router<SdkState> {
    Router::new()
        .route(
            "/api/aria/pipelines",
            post(pipeline_upload).get(pipeline_list),
        )
        .route(
            "/api/aria/pipelines/{id}",
            get(pipeline_get).delete(pipeline_delete),
        )
        .route("/api/aria/pipelines/{id}/run", post(pipeline_create_run))
        .route("/api/aria/pipelines/{id}/execute", post(pipeline_execute))
        .route("/api/aria/pipelines/{id}/runs", get(pipeline_list_runs))
        .route("/api/aria/pipelines/runs/{run_id}", get(pipeline_get_run))
        .route("/api/aria/pipelines/name/{name}", get(pipeline_get_by_name))
}

async fn pipeline_upload(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::pipeline_registry::AriaPipelineUpload>,
) -> ApiResult<crate::aria::pipeline_registry::AriaPipeline> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .pipelines
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn pipeline_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::pipeline_registry::AriaPipeline>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .pipelines
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .pipelines
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn pipeline_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::pipeline_registry::AriaPipeline> {
    let id = parse_uuid(&id)?;
    let entry = state
        .registries
        .pipelines
        .get(id)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn pipeline_get_by_name(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<crate::aria::pipeline_registry::AriaPipeline> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .pipelines
        .get_by_name(&tid, &name)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn pipeline_create_run(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(vars): Json<Option<serde_json::Value>>,
) -> ApiResult<crate::aria::pipeline_registry::AriaPipelineRun> {
    let id = parse_uuid(&id)?;
    let tid = tenant_id(&headers);
    let run = state
        .registries
        .pipelines
        .create_run(id, &tid, vars)
        .await
        .map_err(registry_err)?;
    created(run)
}

async fn pipeline_list_runs(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> ApiResult<Vec<crate::aria::pipeline_registry::AriaPipelineRun>> {
    let id = parse_uuid(&id)?;
    let limit = params.limit.unwrap_or(50).min(200);
    let runs = state
        .registries
        .pipelines
        .list_runs(id, limit)
        .await
        .map_err(registry_err)?;
    ok(runs)
}

async fn pipeline_get_run(
    State(state): State<SdkState>,
    Path(run_id): Path<String>,
) -> ApiResult<crate::aria::pipeline_registry::AriaPipelineRun> {
    let run_id = parse_uuid(&run_id)?;
    let run = state
        .registries
        .pipelines
        .get_run(run_id)
        .await
        .map_err(registry_err)?;
    ok(run)
}

async fn pipeline_execute(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Json(input): Json<ExecuteRequest>,
) -> ApiResult<ExecuteResponse> {
    let id = parse_uuid(&id)?;

    let llm = state.llm.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(ErrorResponse::with_code(
                "Execute not available (no LLM configured)",
                "NOT_CONFIGURED",
            )),
        )
    })?;
    let safety = state.safety.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(ErrorResponse::with_code(
                "Execute not available (no safety layer)",
                "NOT_CONFIGURED",
            )),
        )
    })?;

    let pipeline = state
        .registries
        .pipelines
        .get(id)
        .await
        .map_err(registry_err)?;

    let executor = crate::afw::PipelineExecutor::new(
        Arc::clone(llm),
        Arc::clone(safety),
        Arc::clone(&state.registries.pipelines),
    );

    let variables = input
        .variables
        .unwrap_or_else(|| serde_json::json!({ "input": input.input }));

    let result = executor.execute(&pipeline, variables).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(e.to_string())),
        )
    })?;

    let status_str = match result.status {
        crate::afw::pipeline_executor::PipelineStatus::Completed => "completed",
        crate::afw::pipeline_executor::PipelineStatus::Failed => "failed",
        crate::afw::pipeline_executor::PipelineStatus::PartialSuccess => "partial",
    };

    ok(ExecuteResponse {
        run_id: result.run_id,
        status: status_str.into(),
        output: result.final_output,
        error: if status_str == "failed" {
            Some("one or more steps failed".into())
        } else {
            None
        },
        duration_ms: result.duration.as_millis() as u64,
        details: Some(serde_json::to_value(&result.step_results).unwrap_or_default()),
    })
}

async fn pipeline_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .pipelines
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

// ---------------------------------------------------------------------------
// Container routes
// ---------------------------------------------------------------------------

fn container_routes() -> Router<SdkState> {
    Router::new()
        .route(
            "/api/aria/containers",
            post(container_upload).get(container_list),
        )
        .route(
            "/api/aria/containers/{id}",
            get(container_get).delete(container_delete),
        )
        .route(
            "/api/aria/containers/{id}/runtime",
            patch(container_update_runtime),
        )
        .route("/api/aria/containers/{id}/exec", post(container_exec))
        .route(
            "/api/aria/containers/name/{name}",
            get(container_get_by_name),
        )
}

async fn container_upload(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::container_registry::AriaContainerUpload>,
) -> ApiResult<crate::aria::container_registry::AriaContainer> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .containers
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn container_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::container_registry::AriaContainer>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .containers
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .containers
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn container_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::container_registry::AriaContainer> {
    let id = parse_uuid(&id)?;
    let entry = state
        .registries
        .containers
        .get(id)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn container_get_by_name(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<crate::aria::container_registry::AriaContainer> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .containers
        .get_by_name(&tid, &name)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn container_update_runtime(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Json(input): Json<ContainerRuntimeUpdate>,
) -> ApiResult<crate::aria::container_registry::AriaContainer> {
    let id = parse_uuid(&id)?;
    let container_state = input
        .state
        .parse::<crate::aria::types::ContainerState>()
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(e, "INVALID_STATE")),
            )
        })?;
    let entry = state
        .registries
        .containers
        .update_runtime_state(id, container_state, input.ip.as_deref(), input.pid)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn container_exec(
    State(state): State<SdkState>,
    Path(id): Path<String>,
    Json(input): Json<ContainerExecRequest>,
) -> ApiResult<ContainerExecResponse> {
    let id = parse_uuid(&id)?;

    // Verify the container exists in registry.
    let _container = state
        .registries
        .containers
        .get(id)
        .await
        .map_err(registry_err)?;

    let sandbox = state.sandbox.as_ref().ok_or_else(|| {
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(ErrorResponse::with_code(
                "Container execution not available (no sandbox configured)",
                "NOT_CONFIGURED",
            )),
        )
    })?;

    let cwd = std::path::PathBuf::from(&input.cwd);
    let result = sandbox
        .execute(&input.command, &cwd, input.env)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(e.to_string())),
            )
        })?;

    ok(ContainerExecResponse {
        exit_code: result.exit_code,
        stdout: result.stdout,
        stderr: result.stderr,
        duration_ms: result.duration.as_millis() as u64,
        truncated: result.truncated,
    })
}

async fn container_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .containers
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

// ---------------------------------------------------------------------------
// Network routes
// ---------------------------------------------------------------------------

fn network_routes() -> Router<SdkState> {
    Router::new()
        .route("/api/aria/networks", post(network_upload).get(network_list))
        .route(
            "/api/aria/networks/{id}",
            get(network_get).delete(network_delete),
        )
        .route("/api/aria/networks/name/{name}", get(network_get_by_name))
}

async fn network_upload(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<crate::aria::network_registry::AriaNetworkUpload>,
) -> ApiResult<crate::aria::network_registry::AriaNetwork> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .networks
        .upload(&tid, input)
        .await
        .map_err(registry_err)?;
    created(entry)
}

async fn network_list(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<PaginationParams>,
) -> ApiResult<ListResponse<crate::aria::network_registry::AriaNetwork>> {
    let tid = tenant_id(&headers);
    let pagination = params.to_pagination();
    let items = state
        .registries
        .networks
        .list(&tid, pagination)
        .await
        .map_err(registry_err)?;
    let count = state
        .registries
        .networks
        .count(&tid)
        .await
        .map_err(registry_err)?;
    ok(ListResponse {
        count,
        offset: params.offset.unwrap_or(0),
        limit: params.limit.unwrap_or(100),
        items,
    })
}

async fn network_get(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<crate::aria::network_registry::AriaNetwork> {
    let id = parse_uuid(&id)?;
    let entry = state
        .registries
        .networks
        .get(id)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn network_get_by_name(
    State(state): State<SdkState>,
    headers: axum::http::HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<crate::aria::network_registry::AriaNetwork> {
    let tid = tenant_id(&headers);
    let entry = state
        .registries
        .networks
        .get_by_name(&tid, &name)
        .await
        .map_err(registry_err)?;
    ok(entry)
}

async fn network_delete(
    State(state): State<SdkState>,
    Path(id): Path<String>,
) -> ApiResult<MutationResponse> {
    let id = parse_uuid(&id)?;
    state
        .registries
        .networks
        .delete(id)
        .await
        .map_err(registry_err)?;
    ok(ok_deleted(id).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_from_header() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-tenant-id", "my-tenant".parse().unwrap());
        assert_eq!(tenant_id(&headers), "my-tenant");
    }

    #[test]
    fn tenant_id_defaults_to_default() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(tenant_id(&headers), "default");
    }

    #[test]
    fn parse_uuid_valid() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn parse_uuid_invalid() {
        assert!(parse_uuid("not-a-uuid").is_err());
    }

    #[test]
    fn error_response_not_found() {
        let err = RegistryError::NotFound {
            entity: "tool".into(),
            id: "abc".into(),
        };
        let (status, _) = registry_err(err);
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn error_response_duplicate() {
        let err = RegistryError::Duplicate {
            entity: "tool".into(),
            name: "test".into(),
            tenant_id: "t1".into(),
        };
        let (status, _) = registry_err(err);
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[test]
    fn error_response_invalid_input() {
        let err = RegistryError::InvalidInput("bad data".into());
        let (status, _) = registry_err(err);
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn error_response_database() {
        let err = RegistryError::Database("connection failed".into());
        let (status, _) = registry_err(err);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn mutation_response_ok() {
        let id = Uuid::new_v4();
        let resp = ok_mutation(id);
        assert_eq!(resp.0.id, id);
        assert_eq!(resp.0.status, "ok");
    }

    #[test]
    fn mutation_response_deleted() {
        let id = Uuid::new_v4();
        let resp = ok_deleted(id);
        assert_eq!(resp.0.id, id);
        assert_eq!(resp.0.status, "deleted");
    }
}
