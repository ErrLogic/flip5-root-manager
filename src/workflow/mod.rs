//! The workflow engine (CLAUDE.md sections 7-9, 32): an explicit state
//! machine, bounded retry tracking, and the step orchestration that drives
//! both against the [`crate::adb::AdbClient`] abstraction.

pub mod retry;
pub mod runner;
pub mod state;
pub mod steps;

pub use retry::{RetryPolicy, RetryTracker};
pub use runner::{RunnerError, RunnerEvent, WorkflowRunner};
pub use state::{
    InvalidTransition, OptionalIntegrationStatus, Workflow, WorkflowFailure, WorkflowState,
    WorkflowStatus,
};
