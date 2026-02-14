//! TaskExecutor — polls for pending tasks and executes them.
//!
//! Runs as a background service, polling the TaskRegistry for pending tasks
//! and executing them. Supports concurrency limits and proper error handling.

use std::sync::Arc;
use std::time::Duration;

use crate::aria::hooks::AriaHooks;
use crate::aria::task_registry::TaskRegistry;
use crate::aria::types::TaskStatus;
use crate::llm::{ChatMessage, CompletionRequest, LlmProvider};

/// Configuration for the task executor.
#[derive(Debug, Clone)]
pub struct TaskExecutorConfig {
    /// How often to poll for pending tasks (default: 5 seconds).
    pub poll_interval: Duration,
    /// Maximum concurrent tasks (default: 3).
    pub max_concurrent: usize,
    /// Task execution timeout (default: 5 minutes).
    pub timeout: Duration,
}

impl Default for TaskExecutorConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            max_concurrent: 3,
            timeout: Duration::from_secs(300),
        }
    }
}

/// Polls for and executes pending tasks.
pub struct TaskExecutor {
    config: TaskExecutorConfig,
    task_registry: Arc<TaskRegistry>,
    hooks: Arc<AriaHooks>,
    llm: Arc<dyn LlmProvider>,
    running: Arc<std::sync::atomic::AtomicBool>,
    active_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl TaskExecutor {
    pub fn new(
        config: TaskExecutorConfig,
        task_registry: Arc<TaskRegistry>,
        hooks: Arc<AriaHooks>,
        llm: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            config,
            task_registry,
            hooks,
            llm,
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            active_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Start the polling loop.
    pub fn start(&self) -> tokio::task::JoinHandle<()> {
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let running = Arc::clone(&self.running);
        let task_registry = Arc::clone(&self.task_registry);
        let hooks = Arc::clone(&self.hooks);
        let llm = Arc::clone(&self.llm);
        let config = self.config.clone();
        let active_count = Arc::clone(&self.active_count);

        tokio::spawn(async move {
            tracing::info!(
                "TaskExecutor started (poll interval: {:?})",
                config.poll_interval
            );

            while running.load(std::sync::atomic::Ordering::SeqCst) {
                // Check if we have room for more tasks.
                let current = active_count.load(std::sync::atomic::Ordering::SeqCst);
                if current >= config.max_concurrent {
                    tokio::time::sleep(config.poll_interval).await;
                    continue;
                }

                let slots = config.max_concurrent - current;

                // Fetch pending tasks from all tenants.
                // We use a simple approach: list pending tasks and take up to `slots`.
                match task_registry.list_pending(slots as i64).await {
                    Ok(tasks) => {
                        for task in tasks {
                            let task_id = task.id;
                            let task_registry = Arc::clone(&task_registry);
                            let hooks = Arc::clone(&hooks);
                            let llm = Arc::clone(&llm);
                            let timeout = config.timeout;
                            let active_count = Arc::clone(&active_count);

                            active_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                            tokio::spawn(async move {
                                // Mark as running.
                                let _ = task_registry
                                    .update_status(task_id, TaskStatus::Running, None, None)
                                    .await;

                                // Execute the task.
                                let result = tokio::time::timeout(
                                    timeout,
                                    Self::execute_task(&llm, &task.name, &task.params),
                                )
                                .await;

                                match result {
                                    Ok(Ok(output)) => {
                                        let _ = task_registry
                                            .update_status(
                                                task_id,
                                                TaskStatus::Completed,
                                                Some(serde_json::json!({ "output": output })),
                                                None,
                                            )
                                            .await;
                                        hooks.fire_task_completed(task_id).await;
                                    }
                                    Ok(Err(e)) => {
                                        let _ = task_registry
                                            .update_status(
                                                task_id,
                                                TaskStatus::Failed,
                                                None,
                                                Some(e.to_string()),
                                            )
                                            .await;
                                        hooks.fire_task_failed(task_id).await;
                                    }
                                    Err(_) => {
                                        let _ = task_registry
                                            .update_status(
                                                task_id,
                                                TaskStatus::Failed,
                                                None,
                                                Some("task execution timed out".into()),
                                            )
                                            .await;
                                        hooks.fire_task_failed(task_id).await;
                                    }
                                }

                                active_count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!("TaskExecutor: failed to list pending tasks: {}", e);
                    }
                }

                tokio::time::sleep(config.poll_interval).await;
            }

            tracing::info!("TaskExecutor stopped");
        })
    }

    /// Stop the polling loop.
    pub fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Execute a single task via LLM.
    async fn execute_task(
        llm: &Arc<dyn LlmProvider>,
        task_name: &str,
        params: &serde_json::Value,
    ) -> Result<String, String> {
        let params_str = serde_json::to_string_pretty(params).unwrap_or_default();
        let prompt = format!(
            "Execute the following task:\n\n\
             Task: {task_name}\n\
             Parameters: {params_str}\n\n\
             Provide the result."
        );

        let messages = vec![ChatMessage::user(&prompt)];
        let request = CompletionRequest::new(messages);

        let response = llm
            .complete(request)
            .await
            .map_err(|e| format!("LLM error: {e}"))?;

        Ok(response.content)
    }

    /// Get the number of currently executing tasks.
    pub fn active_count(&self) -> usize {
        self.active_count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = TaskExecutorConfig::default();
        assert_eq!(config.max_concurrent, 3);
        assert_eq!(config.poll_interval, Duration::from_secs(5));
        assert_eq!(config.timeout, Duration::from_secs(300));
    }
}
