//! Deadline tracking for kernel tasks.
//!
//! This module provides the ability to register deadlines for kernel tasks.
//! When a deadline arrives, the associated task is automatically woken
//! (moved to Runnable state).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use log::debug;

use crate::executor;

/// Deadline tracker for kernel tasks.
///
/// Uses a BTreeMap for efficient sorted access to deadlines.
/// Multiple tasks can share the same deadline time.
pub struct DeadlineTracker {
    /// Maps deadline_ms -> list of tasks to wake
    deadlines: BTreeMap<u64, Vec<executor::TaskId>>,
}

impl Default for DeadlineTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DeadlineTracker {
    /// Create a new deadline tracker.
    pub fn new() -> Self {
        Self {
            deadlines: BTreeMap::new(),
        }
    }

    /// Register a deadline for a kernel task.
    ///
    /// When the deadline arrives (checked via `wake_expired`), the task will
    /// be added to the returned list for the caller to wake.
    pub fn register(&mut self, task_id: executor::TaskId, deadline_ms: u64) {
        self.deadlines
            .entry(deadline_ms)
            .or_insert_with(Vec::new)
            .push(task_id);
    }

    /// Collect tasks whose deadlines have expired.
    ///
    /// Returns a list of task IDs that should be woken. The caller is
    /// responsible for actually changing their state.
    pub fn collect_expired(&mut self, now_ms: u64) -> Vec<executor::TaskId> {
        let mut tasks_to_wake = Vec::new();
        let mut expired_deadlines = Vec::new();

        // Collect expired deadlines and tasks (BTreeMap is sorted)
        for (&deadline, tasks) in &self.deadlines {
            if deadline <= now_ms {
                tasks_to_wake.extend(tasks.iter().copied());
                expired_deadlines.push(deadline);
            } else {
                break; // BTreeMap is sorted, no more expired deadlines
            }
        }

        // Remove expired deadlines
        for deadline in expired_deadlines {
            self.deadlines.remove(&deadline);
        }

        if !tasks_to_wake.is_empty() {
            debug!(
                "Collected {} expired tasks at {}",
                tasks_to_wake.len(),
                now_ms
            );
        }

        tasks_to_wake
    }

    /// Get the next deadline time (for timer calculation).
    ///
    /// Returns `None` if no deadlines are registered.
    pub fn next_deadline(&self) -> Option<u64> {
        self.deadlines.keys().next().copied()
    }
}
