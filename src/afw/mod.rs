//! AFW — Aria Firmware: the execution layer for SDK primitives.
//!
//! This module provides the runtime machinery that brings Aria registry
//! definitions to life:
//!
//! - **TeamExecutor**: Multi-agent collaboration with 5 modes
//! - **PipelineExecutor**: DAG-based workflow execution
//! - **CronService**: Flexible job scheduling (at/every/cron)
//! - **CronBridge**: Syncs Aria cron registry ↔ CronService
//! - **FeedScheduler**: Scheduled feed execution
//! - **FeedExecutor**: Runs feed handlers and produces items
//! - **TaskExecutor**: Long-running task execution with polling
//! - **Schedule**: Cron expression parsing and next-run computation

pub mod cron_bridge;
pub mod cron_service;
pub mod feed_executor;
pub mod feed_scheduler;
pub mod pipeline_executor;
pub mod schedule;
pub mod task_executor;
pub mod team_executor;

pub use cron_bridge::CronBridge;
pub use cron_service::CronService;
pub use feed_executor::FeedExecutor;
pub use feed_scheduler::FeedScheduler;
pub use pipeline_executor::PipelineExecutor;
pub use schedule::compute_next_run;
pub use task_executor::TaskExecutor;
pub use team_executor::TeamExecutor;
