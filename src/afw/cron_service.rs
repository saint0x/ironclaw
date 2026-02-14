//! CronService — runtime scheduler for cron jobs.
//!
//! Manages timer-based execution of jobs with three schedule types:
//! - **At**: One-shot at a specific time
//! - **Every**: Recurring interval
//! - **Cron**: Full cron expressions
//!
//! The CronService is the runtime layer; the CronBridge syncs definitions
//! from the Aria CronFunctionRegistry into CronService jobs.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::afw::schedule::compute_next_run;
use crate::aria::types::{CronPayload, CronSchedule, CronSessionTarget};

/// A scheduled cron job.
#[derive(Debug, Clone)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub schedule: CronSchedule,
    pub session_target: CronSessionTarget,
    pub payload: CronPayload,
    pub enabled: bool,
    pub next_run_at_ms: Option<i64>,
    pub running_at_ms: Option<i64>,
    pub last_run_at_ms: Option<i64>,
    pub delete_after_run: bool,
    pub agent_id: Option<String>,
}

/// Input for creating a cron job.
#[derive(Debug, Clone)]
pub struct CronJobCreate {
    pub name: String,
    pub schedule: CronSchedule,
    pub session_target: CronSessionTarget,
    pub payload: CronPayload,
    pub agent_id: Option<String>,
    pub delete_after_run: bool,
}

/// Dependencies injected into the CronService for executing jobs.
pub struct CronServiceDeps {
    /// Enqueue a system event into a session.
    pub enqueue_system_event: Box<
        dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync,
    >,
    /// Request an immediate heartbeat.
    pub request_heartbeat_now: Box<
        dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
    >,
    /// Run an isolated agent job.
    pub run_isolated_agent_job: Box<
        dyn Fn(
                CronPayload,
                Option<String>,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<String, String>> + Send>,
            > + Send
            + Sync,
    >,
    /// Callback on job events (started, completed, failed).
    pub on_event: Box<
        dyn Fn(CronEvent) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync,
    >,
}

/// Events emitted by the cron service.
#[derive(Debug, Clone)]
pub enum CronEvent {
    Started {
        job_id: String,
        job_name: String,
    },
    Completed {
        job_id: String,
        job_name: String,
    },
    Failed {
        job_id: String,
        job_name: String,
        error: String,
    },
}

/// The cron service runtime.
pub struct CronService {
    jobs: Arc<RwLock<HashMap<String, CronJob>>>,
    deps: Arc<CronServiceDeps>,
    timer_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

/// Maximum time a job can be "running" before we consider it stuck (2 hours).
const STUCK_THRESHOLD_MS: i64 = 2 * 60 * 60 * 1000;

impl CronService {
    /// Create a new cron service.
    pub fn new(deps: CronServiceDeps) -> Self {
        Self {
            jobs: Arc::new(RwLock::new(HashMap::new())),
            deps: Arc::new(deps),
            timer_handle: Mutex::new(None),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Start the cron service (arms the timer loop).
    pub async fn start(&self) {
        if self.running.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return; // Already running
        }

        let jobs = Arc::clone(&self.jobs);
        let deps = Arc::clone(&self.deps);
        let running = Arc::clone(&self.running);

        let handle = tokio::spawn(async move {
            while running.load(std::sync::atomic::Ordering::SeqCst) {
                let delay = Self::run_due_jobs_inner(&jobs, &deps).await;
                tokio::time::sleep(delay).await;
            }
        });

        let mut timer = self.timer_handle.lock().await;
        *timer = Some(handle);
    }

    /// Stop the cron service.
    pub async fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let mut timer = self.timer_handle.lock().await;
        if let Some(handle) = timer.take() {
            handle.abort();
        }
    }

    /// Add a new job.
    pub async fn add(&self, input: CronJobCreate) -> CronJob {
        let now_ms = Utc::now().timestamp_millis();
        let next_run = compute_next_run(&input.schedule, now_ms);

        let job = CronJob {
            id: Uuid::new_v4().to_string(),
            name: input.name,
            schedule: input.schedule,
            session_target: input.session_target,
            payload: input.payload,
            enabled: true,
            next_run_at_ms: next_run,
            running_at_ms: None,
            last_run_at_ms: None,
            delete_after_run: input.delete_after_run,
            agent_id: input.agent_id,
        };

        let mut jobs = self.jobs.write().await;
        jobs.insert(job.id.clone(), job.clone());
        job
    }

    /// Update an existing job.
    pub async fn update(
        &self,
        id: &str,
        name: Option<String>,
        schedule: Option<CronSchedule>,
        enabled: Option<bool>,
    ) {
        let mut jobs = self.jobs.write().await;
        if let Some(job) = jobs.get_mut(id) {
            if let Some(name) = name {
                job.name = name;
            }
            if let Some(schedule) = schedule {
                job.schedule = schedule;
                let now_ms = Utc::now().timestamp_millis();
                job.next_run_at_ms = compute_next_run(&job.schedule, now_ms);
            }
            if let Some(enabled) = enabled {
                job.enabled = enabled;
            }
        }
    }

    /// Remove a job.
    pub async fn remove(&self, id: &str) -> bool {
        let mut jobs = self.jobs.write().await;
        jobs.remove(id).is_some()
    }

    /// List all jobs.
    pub async fn list(&self) -> Vec<CronJob> {
        let jobs = self.jobs.read().await;
        let mut list: Vec<CronJob> = jobs.values().cloned().collect();
        list.sort_by_key(|j| j.next_run_at_ms);
        list
    }

    /// Get the service status.
    pub async fn status(&self) -> CronServiceStatus {
        let jobs = self.jobs.read().await;
        let next_wake = jobs
            .values()
            .filter(|j| j.enabled)
            .filter_map(|j| j.next_run_at_ms)
            .min();

        CronServiceStatus {
            enabled: self.running.load(std::sync::atomic::Ordering::SeqCst),
            job_count: jobs.len(),
            next_wake_at_ms: next_wake,
        }
    }

    /// Run all due jobs and return the delay until the next check.
    async fn run_due_jobs_inner(
        jobs: &Arc<RwLock<HashMap<String, CronJob>>>,
        deps: &Arc<CronServiceDeps>,
    ) -> Duration {
        let now_ms = Utc::now().timestamp_millis();
        let due_job_ids: Vec<String>;

        {
            let mut jobs_guard = jobs.write().await;

            // Clear stuck jobs.
            for job in jobs_guard.values_mut() {
                if let Some(running_at) = job.running_at_ms {
                    if now_ms - running_at > STUCK_THRESHOLD_MS {
                        tracing::warn!("Cron job '{}' stuck, clearing running marker", job.name);
                        job.running_at_ms = None;
                    }
                }
            }

            due_job_ids = jobs_guard
                .values()
                .filter(|j| {
                    j.enabled
                        && j.running_at_ms.is_none()
                        && j.next_run_at_ms.is_some_and(|next| next <= now_ms)
                })
                .map(|j| j.id.clone())
                .collect();

            // Mark as running.
            for id in &due_job_ids {
                if let Some(job) = jobs_guard.get_mut(id) {
                    job.running_at_ms = Some(now_ms);
                }
            }
        }

        // Execute due jobs sequentially.
        for job_id in &due_job_ids {
            let job = {
                let jobs_guard = jobs.read().await;
                jobs_guard.get(job_id).cloned()
            };

            if let Some(job) = job {
                (deps.on_event)(CronEvent::Started {
                    job_id: job.id.clone(),
                    job_name: job.name.clone(),
                })
                .await;

                let result = match job.session_target {
                    CronSessionTarget::Main => {
                        if let CronPayload::SystemEvent { ref text } = job.payload {
                            (deps.enqueue_system_event)(text.clone()).await;
                            (deps.request_heartbeat_now)().await;
                            Ok("system event enqueued".to_string())
                        } else {
                            Err("main session target requires system_event payload".to_string())
                        }
                    }
                    CronSessionTarget::Isolated => {
                        (deps.run_isolated_agent_job)(job.payload.clone(), job.agent_id.clone())
                            .await
                    }
                };

                match result {
                    Ok(_) => {
                        (deps.on_event)(CronEvent::Completed {
                            job_id: job.id.clone(),
                            job_name: job.name.clone(),
                        })
                        .await;
                    }
                    Err(ref e) => {
                        (deps.on_event)(CronEvent::Failed {
                            job_id: job.id.clone(),
                            job_name: job.name.clone(),
                            error: e.clone(),
                        })
                        .await;
                    }
                }

                // Finish: update state, compute next run.
                let mut jobs_guard = jobs.write().await;
                if let Some(job) = jobs_guard.get_mut(job_id) {
                    job.running_at_ms = None;
                    job.last_run_at_ms = Some(now_ms);

                    let next = compute_next_run(&job.schedule, now_ms);
                    job.next_run_at_ms = next;

                    // One-shot cleanup.
                    if matches!(job.schedule, CronSchedule::At { .. }) {
                        job.enabled = false;
                        if job.delete_after_run {
                            let id = job.id.clone();
                            drop(jobs_guard);
                            let mut jobs_guard = jobs.write().await;
                            jobs_guard.remove(&id);
                            continue;
                        }
                    }
                }
            }
        }

        // Compute sleep duration until next job.
        let jobs_guard = jobs.read().await;
        let next_wake = jobs_guard
            .values()
            .filter(|j| j.enabled && j.running_at_ms.is_none())
            .filter_map(|j| j.next_run_at_ms)
            .min();

        match next_wake {
            Some(next_ms) => {
                let now = Utc::now().timestamp_millis();
                let delay = (next_ms - now).max(100); // At least 100ms
                Duration::from_millis(delay as u64)
            }
            None => Duration::from_secs(60), // No jobs, check every minute
        }
    }
}

/// Status of the cron service.
#[derive(Debug, Clone)]
pub struct CronServiceStatus {
    pub enabled: bool,
    pub job_count: usize,
    pub next_wake_at_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_deps() -> CronServiceDeps {
        CronServiceDeps {
            enqueue_system_event: Box::new(|_| Box::pin(async {})),
            request_heartbeat_now: Box::new(|| Box::pin(async {})),
            run_isolated_agent_job: Box::new(|_, _| Box::pin(async { Ok("ok".to_string()) })),
            on_event: Box::new(|_| Box::pin(async {})),
        }
    }

    #[tokio::test]
    async fn add_and_list_jobs() {
        let service = CronService::new(noop_deps());

        service
            .add(CronJobCreate {
                name: "test-job".into(),
                schedule: CronSchedule::Every {
                    every_ms: 60_000,
                    anchor_ms: None,
                },
                session_target: CronSessionTarget::Main,
                payload: CronPayload::SystemEvent {
                    text: "hello".into(),
                },
                agent_id: None,
                delete_after_run: false,
            })
            .await;

        let jobs = service.list().await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "test-job");
        assert!(jobs[0].next_run_at_ms.is_some());
    }

    #[tokio::test]
    async fn remove_job() {
        let service = CronService::new(noop_deps());

        let job = service
            .add(CronJobCreate {
                name: "removable".into(),
                schedule: CronSchedule::At { at_ms: i64::MAX },
                session_target: CronSessionTarget::Main,
                payload: CronPayload::SystemEvent { text: "bye".into() },
                agent_id: None,
                delete_after_run: false,
            })
            .await;

        assert!(service.remove(&job.id).await);
        assert_eq!(service.list().await.len(), 0);
    }

    #[tokio::test]
    async fn status_reports_correctly() {
        let service = CronService::new(noop_deps());
        let status = service.status().await;
        assert!(!status.enabled);
        assert_eq!(status.job_count, 0);
    }
}
