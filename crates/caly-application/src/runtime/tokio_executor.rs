//! Tokio task ownership adapter with bounded registration and awaited shutdown.

use core::future::Future;
use std::sync::{Arc, Mutex};

use caly_domain::{BoundedText, BoundedVec};
use tokio::task::JoinHandle;

use super::{CancellationToken, FatalFault, MAX_OWNED_TASKS, RuntimeGuard, TaskName};

/// Task body failure or Tokio join failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokioTaskFailure {
    Task {
        name: TaskName,
        reason: BoundedText<512>,
    },
    Join {
        name: TaskName,
    },
}

struct TokioTask {
    name: TaskName,
    handle: JoinHandle<Result<(), TokioTaskFailure>>,
}

/// Bounded owner of all Tokio tasks in one daemon runtime.
pub struct TokioTaskGroup {
    cancellation: CancellationToken,
    fatal_guard: Option<Arc<Mutex<RuntimeGuard>>>,
    tasks: BoundedVec<TokioTask, MAX_OWNED_TASKS>,
}

impl TokioTaskGroup {
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            fatal_guard: None,
            tasks: BoundedVec::new(),
        }
    }

    /// Creates a task group sharing the RuntimeGuard's root cancellation and
    /// first-cause fault owner.
    pub fn with_runtime_guard(guard: Arc<Mutex<RuntimeGuard>>) -> Self {
        let cancellation = guard
            .lock()
            .map_or_else(|_| CancellationToken::new(), |guard| guard.cancellation());
        Self {
            cancellation,
            fatal_guard: Some(guard),
            tasks: BoundedVec::new(),
        }
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Spawns with the generic task/join fault category.
    pub fn spawn_owned<F>(&mut self, name: TaskName, future: F) -> Result<(), TokioSpawnError>
    where
        F: Future<Output = Result<(), TokioTaskFailure>> + Send + 'static,
    {
        self.spawn_owned_with_fault(name, FatalFault::TaskJoin, future)
    }

    /// Spawns only after capacity is reserved and records the supplied first-
    /// cause category when the task body or Tokio join fails.
    pub fn spawn_owned_with_fault<F>(
        &mut self,
        name: TaskName,
        fault: FatalFault,
        future: F,
    ) -> Result<(), TokioSpawnError>
    where
        F: Future<Output = Result<(), TokioTaskFailure>> + Send + 'static,
    {
        if self.tasks.len() == self.tasks.capacity() {
            return Err(TokioSpawnError::CapacityReached);
        }
        let monitor_name = name.clone();
        let cancellation = self.cancellation.clone();
        let fatal_guard = self.fatal_guard.clone();
        let handle = tokio::spawn(async move {
            // Monitor both task-body errors and Tokio join failures. The first
            // failing owned task requests root cancellation immediately so
            // sibling actors do not continue in a partially dead topology.
            let worker = tokio::spawn(future);
            let result = match worker.await {
                Ok(result) => result,
                Err(_) => Err(TokioTaskFailure::Join { name: monitor_name }),
            };
            if result.is_err() {
                record_fatal(fatal_guard.as_ref(), &cancellation, fault);
            }
            result
        });
        let task = TokioTask { name, handle };
        self.tasks.try_push(task).map_err(|error| {
            error.into_value().handle.abort();
            TokioSpawnError::CapacityReached
        })
    }

    /// Requests cooperative cancellation and awaits every registered task.
    pub async fn shutdown(
        self,
    ) -> Result<BoundedVec<TokioTaskFailure, MAX_OWNED_TASKS>, TokioSpawnError> {
        self.cancellation.cancel();
        let mut failures = Vec::new();
        for task in self.tasks.into_vec() {
            match task.handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(_) => failures.push(TokioTaskFailure::Join { name: task.name }),
            }
        }
        BoundedVec::try_from_vec(failures).map_err(|_| TokioSpawnError::CapacityReached)
    }
}

fn record_fatal(
    guard: Option<&Arc<Mutex<RuntimeGuard>>>,
    cancellation: &CancellationToken,
    fault: FatalFault,
) {
    if let Some(guard) = guard
        && let Ok(mut guard) = guard.lock()
    {
        guard.record_fatal(fault);
        return;
    }
    // A poisoned/unavailable guard must still stop sibling tasks.
    cancellation.cancel();
}

impl Default for TokioTaskGroup {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokioSpawnError {
    CapacityReached,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text<const MAX: usize>(value: &str) -> BoundedText<MAX> {
        BoundedText::new(value.to_owned()).unwrap()
    }

    #[tokio::test]
    async fn guarded_task_records_first_cause() -> Result<(), TokioSpawnError> {
        let guard = Arc::new(Mutex::new(RuntimeGuard::new()));
        let mut tasks = TokioTaskGroup::with_runtime_guard(Arc::clone(&guard));
        let name = text("dispatcher");
        let task_name = name.clone();
        tasks.spawn_owned_with_fault(name, FatalFault::CommandDispatch, async move {
            Err(TokioTaskFailure::Task {
                name: task_name,
                reason: text("dispatch failed"),
            })
        })?;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let first = guard.lock().map_or(None, |guard| guard.first_fault());
        assert_eq!(first, Some(FatalFault::CommandDispatch));
        let failures = tasks.shutdown().await?;
        assert_eq!(failures.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn task_failure_requests_root_cancellation() -> Result<(), TokioSpawnError> {
        let mut tasks = TokioTaskGroup::new();
        let cancellation = tasks.cancellation();
        let name = text("failing-task");
        let task_name = name.clone();
        tasks.spawn_owned(name, async move {
            Err(TokioTaskFailure::Task {
                name: task_name,
                reason: text("intentional test failure"),
            })
        })?;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert!(cancellation.is_cancelled());
        let failures = tasks.shutdown().await?;
        assert_eq!(failures.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_returns_task_body_failures() -> Result<(), TokioSpawnError> {
        let mut tasks = TokioTaskGroup::new();
        let name = text("failing-task");
        let task_name = name.clone();
        tasks.spawn_owned(name, async move {
            Err(TokioTaskFailure::Task {
                name: task_name,
                reason: text("intentional test failure"),
            })
        })?;
        let failures = tasks.shutdown().await?;
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            TokioTaskFailure::Task { reason, .. }
                if reason.as_str() == "intentional test failure"
        ));
        Ok(())
    }
}
