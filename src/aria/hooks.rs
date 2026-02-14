//! Aria Hooks — cross-module event propagation.
//!
//! Hooks are fire-and-forget callbacks that wire registry mutations to runtime
//! systems (FeedScheduler, CronBridge, TaskEventBridge). They are set during
//! server initialization and called from registry operations / API route handlers.
//!
//! Hooks never block the caller — errors are logged and swallowed.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::aria::cron_registry::AriaCronFunction;
use crate::aria::feed_registry::AriaFeed;

/// Type alias for async hook callbacks.
type AsyncHook<T> = Arc<dyn Fn(T) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Feed lifecycle hooks.
struct FeedHooksInner {
    on_uploaded: Option<AsyncHook<AriaFeed>>,
    on_deleted: Option<AsyncHook<Uuid>>,
}

/// Cron lifecycle hooks.
struct CronHooksInner {
    on_uploaded: Option<AsyncHook<AriaCronFunction>>,
    on_deleted: Option<AsyncHook<Uuid>>,
}

/// Task lifecycle hooks.
struct TaskHooksInner {
    on_completed: Option<AsyncHook<Uuid>>,
    on_failed: Option<AsyncHook<Uuid>>,
}

/// Central hook registry for cross-module event propagation.
///
/// Set once during initialization, then read from hot paths.
pub struct AriaHooks {
    feed: RwLock<FeedHooksInner>,
    cron: RwLock<CronHooksInner>,
    task: RwLock<TaskHooksInner>,
}

impl Default for AriaHooks {
    fn default() -> Self {
        Self::new()
    }
}

impl AriaHooks {
    pub fn new() -> Self {
        Self {
            feed: RwLock::new(FeedHooksInner {
                on_uploaded: None,
                on_deleted: None,
            }),
            cron: RwLock::new(CronHooksInner {
                on_uploaded: None,
                on_deleted: None,
            }),
            task: RwLock::new(TaskHooksInner {
                on_completed: None,
                on_failed: None,
            }),
        }
    }

    // ── Feed hooks ──────────────────────────────────────────────

    /// Register the feed hooks (called once during init).
    pub async fn set_feed_hooks<F1, F2>(&self, on_uploaded: F1, on_deleted: F2)
    where
        F1: Fn(AriaFeed) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
        F2: Fn(Uuid) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    {
        let mut inner = self.feed.write().await;
        inner.on_uploaded = Some(Arc::new(on_uploaded));
        inner.on_deleted = Some(Arc::new(on_deleted));
    }

    /// Fire the feed-uploaded hook (fire-and-forget).
    pub async fn fire_feed_uploaded(&self, feed: AriaFeed) {
        let inner = self.feed.read().await;
        if let Some(ref hook) = inner.on_uploaded {
            let hook = Arc::clone(hook);
            tokio::spawn(async move {
                hook(feed).await;
            });
        }
    }

    /// Fire the feed-deleted hook (fire-and-forget).
    pub async fn fire_feed_deleted(&self, feed_id: Uuid) {
        let inner = self.feed.read().await;
        if let Some(ref hook) = inner.on_deleted {
            let hook = Arc::clone(hook);
            tokio::spawn(async move {
                hook(feed_id).await;
            });
        }
    }

    // ── Cron hooks ──────────────────────────────────────────────

    /// Register the cron hooks (called once during init).
    pub async fn set_cron_hooks<F1, F2>(&self, on_uploaded: F1, on_deleted: F2)
    where
        F1: Fn(AriaCronFunction) -> Pin<Box<dyn Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
        F2: Fn(Uuid) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    {
        let mut inner = self.cron.write().await;
        inner.on_uploaded = Some(Arc::new(on_uploaded));
        inner.on_deleted = Some(Arc::new(on_deleted));
    }

    /// Fire the cron-uploaded hook (fire-and-forget).
    pub async fn fire_cron_uploaded(&self, cron_func: AriaCronFunction) {
        let inner = self.cron.read().await;
        if let Some(ref hook) = inner.on_uploaded {
            let hook = Arc::clone(hook);
            tokio::spawn(async move {
                hook(cron_func).await;
            });
        }
    }

    /// Fire the cron-deleted hook (fire-and-forget).
    pub async fn fire_cron_deleted(&self, cron_func_id: Uuid) {
        let inner = self.cron.read().await;
        if let Some(ref hook) = inner.on_deleted {
            let hook = Arc::clone(hook);
            tokio::spawn(async move {
                hook(cron_func_id).await;
            });
        }
    }

    // ── Task hooks ──────────────────────────────────────────────

    /// Register the task hooks (called once during init).
    pub async fn set_task_hooks<F1, F2>(&self, on_completed: F1, on_failed: F2)
    where
        F1: Fn(Uuid) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
        F2: Fn(Uuid) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
    {
        let mut inner = self.task.write().await;
        inner.on_completed = Some(Arc::new(on_completed));
        inner.on_failed = Some(Arc::new(on_failed));
    }

    /// Fire the task-completed hook (fire-and-forget).
    pub async fn fire_task_completed(&self, task_id: Uuid) {
        let inner = self.task.read().await;
        if let Some(ref hook) = inner.on_completed {
            let hook = Arc::clone(hook);
            tokio::spawn(async move {
                hook(task_id).await;
            });
        }
    }

    /// Fire the task-failed hook (fire-and-forget).
    pub async fn fire_task_failed(&self, task_id: Uuid) {
        let inner = self.task.read().await;
        if let Some(ref hook) = inner.on_failed {
            let hook = Arc::clone(hook);
            tokio::spawn(async move {
                hook(task_id).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn hooks_are_initially_empty() {
        let hooks = AriaHooks::new();
        // These should not panic even with no hooks registered.
        hooks.fire_feed_deleted(Uuid::new_v4()).await;
        hooks.fire_cron_deleted(Uuid::new_v4()).await;
        hooks.fire_task_completed(Uuid::new_v4()).await;
        hooks.fire_task_failed(Uuid::new_v4()).await;
    }

    #[tokio::test]
    async fn task_hooks_fire() {
        let hooks = AriaHooks::new();
        let counter = Arc::new(AtomicU32::new(0));

        let c1 = Arc::clone(&counter);
        let c2 = Arc::clone(&counter);

        hooks
            .set_task_hooks(
                move |_id| {
                    let c = Arc::clone(&c1);
                    Box::pin(async move {
                        c.fetch_add(1, Ordering::SeqCst);
                    })
                },
                move |_id| {
                    let c = Arc::clone(&c2);
                    Box::pin(async move {
                        c.fetch_add(10, Ordering::SeqCst);
                    })
                },
            )
            .await;

        hooks.fire_task_completed(Uuid::new_v4()).await;
        hooks.fire_task_failed(Uuid::new_v4()).await;

        // Give spawned tasks time to complete.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 11);
    }
}
