//! Aria Registry System — persistence and caching layer for all SDK primitives.
//!
//! Every registry follows the same structural pattern:
//! - PostgreSQL persistence
//! - In-memory cache (`DashMap`) with lazy loading
//! - Tenant-scoped lookups via `(tenant_id, name)` composite keys
//! - Soft deletes (status field) except CronFunctionRegistry (hard deletes)
//! - Upsert-by-name: same tenant+name = update, different name = create

pub mod registry;
pub mod types;

pub mod agent_registry;
pub mod container_registry;
pub mod cron_registry;
pub mod feed_registry;
pub mod hooks;
pub mod kv_registry;
pub mod memory_registry;
pub mod network_registry;
pub mod pipeline_registry;
pub mod task_registry;
pub mod team_registry;
pub mod tool_registry;

pub use registry::Registry;
pub use types::*;

use std::sync::Arc;

use deadpool_postgres::Pool;

use self::agent_registry::AgentRegistry;
use self::container_registry::ContainerRegistry;
use self::cron_registry::CronFunctionRegistry;
use self::feed_registry::FeedRegistry;
use self::hooks::AriaHooks;
use self::kv_registry::KvRegistry;
use self::memory_registry::MemoryRegistry;
use self::network_registry::NetworkRegistry;
use self::pipeline_registry::PipelineRegistry;
use self::task_registry::TaskRegistry;
use self::team_registry::TeamRegistry;
use self::tool_registry::ToolRegistry;

/// All 11 Aria registries, initialized from a shared database pool.
pub struct AriaRegistries {
    pub tools: Arc<ToolRegistry>,
    pub agents: Arc<AgentRegistry>,
    pub memory: Arc<MemoryRegistry>,
    pub tasks: Arc<TaskRegistry>,
    pub feeds: Arc<FeedRegistry>,
    pub cron_functions: Arc<CronFunctionRegistry>,
    pub kv: Arc<KvRegistry>,
    pub teams: Arc<TeamRegistry>,
    pub pipelines: Arc<PipelineRegistry>,
    pub containers: Arc<ContainerRegistry>,
    pub networks: Arc<NetworkRegistry>,
    pub hooks: Arc<AriaHooks>,
}

impl AriaRegistries {
    /// Create all registries from a shared database pool.
    pub fn new(pool: Pool) -> Self {
        let hooks = Arc::new(AriaHooks::new());
        Self {
            tools: Arc::new(ToolRegistry::new(pool.clone())),
            agents: Arc::new(AgentRegistry::new(pool.clone())),
            memory: Arc::new(MemoryRegistry::new(pool.clone())),
            tasks: Arc::new(TaskRegistry::new(pool.clone())),
            feeds: Arc::new(FeedRegistry::new(pool.clone())),
            cron_functions: Arc::new(CronFunctionRegistry::new(pool.clone())),
            kv: Arc::new(KvRegistry::new(pool.clone())),
            teams: Arc::new(TeamRegistry::new(pool.clone())),
            pipelines: Arc::new(PipelineRegistry::new(pool.clone())),
            containers: Arc::new(ContainerRegistry::new(pool.clone())),
            networks: Arc::new(NetworkRegistry::new(pool)),
            hooks,
        }
    }
}
