//! Bounded, explicit TUI-level retry tracking (CLAUDE.md section 14).
//!
//! This is deliberately separate from the payload's own internal attempt
//! configuration (`EXPLOIT_ATTEMPTS=3`, etc., preserved verbatim in
//! [`crate::workflow::steps::payload`]). This tracker only counts how many
//! times the *user* has asked the TUI to retry a given workflow step, and
//! enforces a hard ceiling — there is no unbounded `while !success { retry
//! () }` anywhere in this codebase.

use std::collections::HashMap;

use crate::workflow::state::WorkflowState;

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // One initial attempt plus two retries, matching the "visible,
        // bounded" intent of CLAUDE.md section 14.
        Self { max_attempts: 3 }
    }
}

/// Tracks how many attempts have been made at each workflow step.
#[derive(Debug, Default)]
pub struct RetryTracker {
    policy: RetryPolicy,
    attempts: HashMap<WorkflowState, u32>,
}

impl RetryTracker {
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            policy,
            attempts: HashMap::new(),
        }
    }

    /// Records that an attempt is about to be made at `step`. Returns the
    /// new attempt count (1-based).
    pub fn record_attempt(&mut self, step: WorkflowState) -> u32 {
        let count = self.attempts.entry(step).or_insert(0);
        *count += 1;
        *count
    }

    pub fn attempts_for(&self, step: WorkflowState) -> u32 {
        self.attempts.get(&step).copied().unwrap_or(0)
    }

    /// Whether another attempt at `step` is still permitted under the
    /// policy's hard ceiling.
    pub fn can_retry(&self, step: WorkflowState) -> bool {
        self.attempts_for(step) < self.policy.max_attempts
    }

    pub fn remaining(&self, step: WorkflowState) -> u32 {
        self.policy
            .max_attempts
            .saturating_sub(self.attempts_for(step))
    }

    pub fn max_attempts(&self) -> u32 {
        self.policy.max_attempts
    }

    /// Clears the attempt count for a step, e.g. once it succeeds, so a
    /// later unrelated failure at the same step starts a fresh count.
    pub fn reset(&mut self, step: WorkflowState) {
        self.attempts.remove(&step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_are_bounded() {
        let mut tracker = RetryTracker::new(RetryPolicy { max_attempts: 3 });
        let step = WorkflowState::PayloadExecuting;

        assert!(tracker.can_retry(step));
        tracker.record_attempt(step);
        assert_eq!(tracker.attempts_for(step), 1);
        assert!(tracker.can_retry(step));

        tracker.record_attempt(step);
        assert!(tracker.can_retry(step));

        tracker.record_attempt(step);
        assert_eq!(tracker.attempts_for(step), 3);
        // Hard ceiling reached: no further retries permitted.
        assert!(!tracker.can_retry(step));
    }

    #[test]
    fn steps_are_tracked_independently() {
        let mut tracker = RetryTracker::default();
        tracker.record_attempt(WorkflowState::PayloadExecuting);
        tracker.record_attempt(WorkflowState::PayloadExecuting);
        assert_eq!(tracker.attempts_for(WorkflowState::KernelSuLoaded), 0);
        assert_eq!(tracker.attempts_for(WorkflowState::PayloadExecuting), 2);
    }

    #[test]
    fn reset_clears_count() {
        let mut tracker = RetryTracker::default();
        let step = WorkflowState::RootVerified;
        tracker.record_attempt(step);
        tracker.record_attempt(step);
        tracker.reset(step);
        assert_eq!(tracker.attempts_for(step), 0);
        assert!(tracker.can_retry(step));
    }
}
