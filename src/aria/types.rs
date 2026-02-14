//! Core types for the Aria registry system.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Status of a registry entry (soft delete pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    Active,
    Deleted,
}

impl EntryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deleted => "deleted",
        }
    }
}

impl std::str::FromStr for EntryStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "deleted" => Ok(Self::Deleted),
            _ => Err(format!("invalid status: {s}")),
        }
    }
}

/// Memory storage tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    /// Session-scoped, cleared when session ends.
    Scratchpad,
    /// TTL-based, auto-expires.
    Ephemeral,
    /// Persistent, never expires (default).
    Longterm,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Scratchpad => "scratchpad",
            Self::Ephemeral => "ephemeral",
            Self::Longterm => "longterm",
        }
    }
}

impl std::str::FromStr for MemoryTier {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "scratchpad" => Ok(Self::Scratchpad),
            "ephemeral" => Ok(Self::Ephemeral),
            "longterm" => Ok(Self::Longterm),
            _ => Err(format!("invalid memory tier: {s}")),
        }
    }
}

/// Task execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("invalid task status: {s}")),
        }
    }
}

/// Team execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamMode {
    /// One agent routes to others.
    Coordinator,
    /// Agents take turns in order.
    RoundRobin,
    /// Pick the best agent per task.
    Delegate,
    /// All agents run simultaneously.
    Parallel,
    /// Agents run in sequence, passing results.
    Sequential,
}

impl TeamMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::RoundRobin => "round_robin",
            Self::Delegate => "delegate",
            Self::Parallel => "parallel",
            Self::Sequential => "sequential",
        }
    }
}

impl std::str::FromStr for TeamMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "coordinator" => Ok(Self::Coordinator),
            "round_robin" => Ok(Self::RoundRobin),
            "delegate" | "delegate_to_best" => Ok(Self::Delegate),
            "parallel" => Ok(Self::Parallel),
            "sequential" => Ok(Self::Sequential),
            _ => Err(format!("invalid team mode: {s}")),
        }
    }
}

/// Cron schedule kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CronSchedule {
    /// One-shot at a specific time.
    At { at_ms: i64 },
    /// Recurring interval.
    Every {
        every_ms: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor_ms: Option<i64>,
    },
    /// Cron expression.
    Cron {
        expr: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tz: Option<String>,
    },
}

/// Cron session target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CronSessionTarget {
    Main,
    Isolated,
}

impl CronSessionTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Isolated => "isolated",
        }
    }
}

impl std::str::FromStr for CronSessionTarget {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "main" => Ok(Self::Main),
            "isolated" => Ok(Self::Isolated),
            _ => Err(format!("invalid session target: {s}")),
        }
    }
}

/// Cron wake mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CronWakeMode {
    NextHeartbeat,
    Now,
}

impl std::str::FromStr for CronWakeMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "next-heartbeat" | "next_heartbeat" => Ok(Self::NextHeartbeat),
            "now" => Ok(Self::Now),
            _ => Err(format!("invalid wake mode: {s}")),
        }
    }
}

/// Cron payload kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CronPayload {
    SystemEvent {
        text: String,
    },
    AgentTurn {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        deliver: Option<bool>,
    },
}

/// Cron isolation config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronIsolation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_to_main_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_to_main_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_to_main_max_chars: Option<usize>,
}

/// Container state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    Pending,
    Starting,
    Running,
    Stopped,
    Exited,
    Error,
}

impl ContainerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Exited => "exited",
            Self::Error => "error",
        }
    }
}

impl std::str::FromStr for ContainerState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "stopped" => Ok(Self::Stopped),
            "exited" => Ok(Self::Exited),
            "error" => Ok(Self::Error),
            _ => Err(format!("invalid container state: {s}")),
        }
    }
}

/// Restart policy for containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    No,
    Always,
    OnFailure,
    UnlessStopped,
}

impl RestartPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Always => "always",
            Self::OnFailure => "on_failure",
            Self::UnlessStopped => "unless_stopped",
        }
    }
}

impl std::str::FromStr for RestartPolicy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "no" => Ok(Self::No),
            "always" => Ok(Self::Always),
            "on_failure" | "on-failure" => Ok(Self::OnFailure),
            "unless_stopped" | "unless-stopped" => Ok(Self::UnlessStopped),
            _ => Err(format!("invalid restart policy: {s}")),
        }
    }
}

/// Feed card types (24 types as per SDK specification).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedCardType {
    Article,
    News,
    Weather,
    Stock,
    Calendar,
    Task,
    Reminder,
    Email,
    Chat,
    Social,
    Photo,
    Video,
    Music,
    Podcast,
    Map,
    Transit,
    Flight,
    Package,
    Finance,
    Health,
    Sport,
    Recipe,
    Quote,
    Custom,
}

/// A single feed item produced by a feed run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedItem {
    pub card_type: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

/// Result of a feed execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedResult {
    pub success: bool,
    pub items: Vec<FeedItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Runtime injection types — capabilities injected into agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionType {
    Logging,
    Memory,
    Tasks,
    Database,
    Secrets,
    Http,
    Workspace,
    Tools,
    Channels,
    Safety,
    Extensions,
    Sandbox,
}

/// A runtime injection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInjection {
    pub injection_type: InjectionType,
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Pipeline step definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    pub name: String,
    /// What to execute: "tool:name", "agent:name", "team:name".
    pub execute: String,
    #[serde(default)]
    pub params: serde_json::Value,
    /// Step names this depends on.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Condition expression for running this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Retry policy for pipeline steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_retries: u32,
    #[serde(default = "default_backoff_ms")]
    pub backoff_ms: u64,
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,
}

fn default_backoff_ms() -> u64 {
    1000
}
fn default_backoff_multiplier() -> f64 {
    2.0
}

/// Team member configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMemberConfig {
    pub agent_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub specialization: Option<String>,
}

/// Container volume mount.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerVolumeMount {
    pub host_path: String,
    pub container_path: String,
    #[serde(default)]
    pub read_only: bool,
}

/// Container port mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerPort {
    pub host_port: u16,
    pub container_port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

/// Network DNS configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkDnsConfig {
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default)]
    pub search_domains: Vec<String>,
}

/// Pagination parameters.
#[derive(Debug, Clone)]
pub struct Pagination {
    pub offset: i64,
    pub limit: i64,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 100,
        }
    }
}

/// Generic registry entry metadata returned by list operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryMeta {
    pub id: Uuid,
    pub tenant_id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
