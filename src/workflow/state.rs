//! Explicit workflow state machine (CLAUDE.md sections 7-8).
//!
//! Deliberately not a collection of boolean flags: the current status is
//! always exactly one of "running at step X" or "failed at step X with
//! reason Y", and every transition is validated against the fixed linear
//! order defined below.

use thiserror::Error;

/// The normal, successful **mandatory root workflow** states, in the
/// fixed order defined by CLAUDE.md section 8.
///
/// `hybrid_mount`/ViPER4Android configuration is deliberately *not* part
/// of this chain: it is a personal/environment-specific post-root
/// integration, not every user has those modules installed, and rooting
/// must succeed without ever touching them. It is tracked completely
/// separately — see [`OptionalIntegrationStatus`] — and is only ever
/// offered once this mandatory chain reaches `Completed`. There is no way
/// to skip ahead in this chain: `advance_to` only accepts the one state
/// that immediately follows the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkflowState {
    Disconnected,
    DeviceDetected,
    DeviceVerified,
    PayloadSelected,
    PayloadPushed,
    PayloadExecuting,
    PayloadSucceeded,
    KernelSuSelected,
    KernelSuPushed,
    KernelSuStaged,
    KernelSuLoaded,
    KernelSuVerified,
    RootVerified,
    /// Mandatory cleanup of the temporary bind mount used during
    /// KernelSU late-load (`umount /system/bin/logcat`). This has
    /// nothing to do with `hybrid_mount`/ViPER4Android and must never be
    /// made conditional on them.
    TemporaryMountCleaned,
    /// The mandatory root workflow is done. Any `hybrid_mount`/
    /// ViPER4Android integration is a separate, optional step offered
    /// *after* this.
    Completed,
}

pub const WORKFLOW_ORDER: [WorkflowState; 15] = [
    WorkflowState::Disconnected,
    WorkflowState::DeviceDetected,
    WorkflowState::DeviceVerified,
    WorkflowState::PayloadSelected,
    WorkflowState::PayloadPushed,
    WorkflowState::PayloadExecuting,
    WorkflowState::PayloadSucceeded,
    WorkflowState::KernelSuSelected,
    WorkflowState::KernelSuPushed,
    WorkflowState::KernelSuStaged,
    WorkflowState::KernelSuLoaded,
    WorkflowState::KernelSuVerified,
    WorkflowState::RootVerified,
    WorkflowState::TemporaryMountCleaned,
    WorkflowState::Completed,
];

/// Number of real, user-visible *mandatory* workflow steps —
/// `WORKFLOW_ORDER` minus the `Disconnected` starting pseudostate and the
/// `Completed` terminal marker, neither of which the TUI renders as its
/// own checklist row (see `tui::widgets::render_workflow`); `Completed`
/// is announced as a banner instead.
pub const TOTAL_REAL_STEPS: usize = WORKFLOW_ORDER.len() - 2;

impl WorkflowState {
    pub fn index(&self) -> usize {
        WORKFLOW_ORDER
            .iter()
            .position(|s| s == self)
            .expect("WORKFLOW_ORDER must contain every WorkflowState variant")
    }

    /// The single state that normally follows this one, or `None` if this
    /// is the terminal `Completed` state.
    pub fn normal_next(&self) -> Option<WorkflowState> {
        WORKFLOW_ORDER.get(self.index() + 1).copied()
    }

    pub fn label(&self) -> &'static str {
        match self {
            WorkflowState::Disconnected => "Disconnected",
            WorkflowState::DeviceDetected => "Device detection",
            WorkflowState::DeviceVerified => "Device verification",
            WorkflowState::PayloadSelected => "Payload selection",
            WorkflowState::PayloadPushed => "Payload push",
            WorkflowState::PayloadExecuting => "Payload execution",
            WorkflowState::PayloadSucceeded => "Payload succeeded",
            WorkflowState::KernelSuSelected => "KernelSU selection",
            WorkflowState::KernelSuPushed => "KernelSU push",
            WorkflowState::KernelSuStaged => "KernelSU staged",
            WorkflowState::KernelSuLoaded => "KernelSU load",
            WorkflowState::KernelSuVerified => "KernelSU verification",
            WorkflowState::RootVerified => "Root verification",
            WorkflowState::TemporaryMountCleaned => "Cleanup",
            WorkflowState::Completed => "Root workflow completed",
        }
    }
}

/// Failure states (CLAUDE.md section 7). Carries enough detail for a
/// human-readable error report (section 27).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowFailure {
    DeviceMismatch { detail: String },
    DeviceDisconnected,
    PayloadFailure { detail: String },
    Timeout { detail: String },
    VerificationFailed { detail: String },
    CleanupFailed { detail: String },
    ConfigurationInvalid { detail: String },
    UserCancelled,
}

impl WorkflowFailure {
    pub fn label(&self) -> &'static str {
        match self {
            WorkflowFailure::DeviceMismatch { .. } => "DeviceMismatch",
            WorkflowFailure::DeviceDisconnected => "DeviceDisconnected",
            WorkflowFailure::PayloadFailure { .. } => "PayloadFailure",
            WorkflowFailure::Timeout { .. } => "Timeout",
            WorkflowFailure::VerificationFailed { .. } => "VerificationFailed",
            WorkflowFailure::CleanupFailed { .. } => "CleanupFailed",
            WorkflowFailure::ConfigurationInvalid { .. } => "ConfigurationInvalid",
            WorkflowFailure::UserCancelled => "UserCancelled",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            WorkflowFailure::DeviceMismatch { detail }
            | WorkflowFailure::PayloadFailure { detail }
            | WorkflowFailure::Timeout { detail }
            | WorkflowFailure::VerificationFailed { detail }
            | WorkflowFailure::CleanupFailed { detail }
            | WorkflowFailure::ConfigurationInvalid { detail } => Some(detail),
            WorkflowFailure::DeviceDisconnected | WorkflowFailure::UserCancelled => None,
        }
    }
}

/// The workflow is always in exactly one of these two shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowStatus {
    Running(WorkflowState),
    Failed {
        last_good: WorkflowState,
        failure: WorkflowFailure,
    },
}

impl WorkflowStatus {
    pub fn current_or_last_good(&self) -> WorkflowState {
        match self {
            WorkflowStatus::Running(s) => *s,
            WorkflowStatus::Failed { last_good, .. } => *last_good,
        }
    }

    pub fn is_completed(&self) -> bool {
        matches!(self, WorkflowStatus::Running(WorkflowState::Completed))
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, WorkflowStatus::Failed { .. })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InvalidTransition {
    #[error("expected to be at {expected:?} but workflow is at {actual:?}")]
    WrongCurrentState {
        expected: WorkflowState,
        actual: WorkflowState,
    },
    #[error(
        "{from:?} cannot advance directly to {to:?}; only the next sequential state is allowed"
    )]
    NotSequential {
        from: WorkflowState,
        to: WorkflowState,
    },
    #[error("cannot advance while workflow is in a failed state; retry first")]
    CannotAdvanceWhileFailed,
    #[error("workflow is not in a failed state; nothing to retry")]
    NotFailed,
    #[error(
        "cannot resume at {requested:?}, which is ahead of the last known-good state {last_good:?}"
    )]
    CannotResumeAheadOfLastGood {
        last_good: WorkflowState,
        requested: WorkflowState,
    },
}

/// The workflow state machine itself. Holds the current status plus a
/// linear history for display/recovery purposes (CLAUDE.md section 17/24).
#[derive(Debug, Clone)]
pub struct Workflow {
    status: WorkflowStatus,
    history: Vec<WorkflowStatus>,
}

impl Default for Workflow {
    fn default() -> Self {
        Self::new()
    }
}

impl Workflow {
    pub fn new() -> Self {
        Self {
            status: WorkflowStatus::Running(WorkflowState::Disconnected),
            history: Vec::new(),
        }
    }

    pub fn status(&self) -> &WorkflowStatus {
        &self.status
    }

    pub fn history(&self) -> &[WorkflowStatus] {
        &self.history
    }

    /// Advances from `expected_current` to the next sequential state only.
    /// Fails if the workflow isn't currently running at `expected_current`,
    /// if `next` isn't its immediate successor, or if the workflow is
    /// currently in a failed status.
    pub fn advance_to(
        &mut self,
        expected_current: WorkflowState,
        next: WorkflowState,
    ) -> Result<(), InvalidTransition> {
        match self.status {
            WorkflowStatus::Running(cur) if cur == expected_current => {
                if cur.normal_next() != Some(next) {
                    return Err(InvalidTransition::NotSequential {
                        from: cur,
                        to: next,
                    });
                }
                self.history.push(self.status.clone());
                self.status = WorkflowStatus::Running(next);
                Ok(())
            }
            WorkflowStatus::Running(cur) => Err(InvalidTransition::WrongCurrentState {
                expected: expected_current,
                actual: cur,
            }),
            WorkflowStatus::Failed { .. } => Err(InvalidTransition::CannotAdvanceWhileFailed),
        }
    }

    /// Transitions into a failure state. Always allowed while running;
    /// `last_good` is recorded as the state that was current when the
    /// failure occurred (i.e. the last state we know was actually
    /// reached). A failed verification or device mismatch therefore can
    /// never leave `last_good` pointing past the step that failed, which
    /// structurally prevents e.g. a device mismatch from ever being
    /// "retried" directly into payload execution.
    pub fn fail(&mut self, failure: WorkflowFailure) {
        let last_good = self.status.current_or_last_good();
        self.history.push(self.status.clone());
        self.status = WorkflowStatus::Failed { last_good, failure };
    }

    /// Resumes from a failed status at `resume_state`, which must be the
    /// last known-good state or earlier. Never allows resuming ahead of
    /// what was actually verified.
    pub fn retry_from(&mut self, resume_state: WorkflowState) -> Result<(), InvalidTransition> {
        match self.status {
            WorkflowStatus::Failed { last_good, .. } => {
                if resume_state.index() > last_good.index() {
                    return Err(InvalidTransition::CannotResumeAheadOfLastGood {
                        last_good,
                        requested: resume_state,
                    });
                }
                self.history.push(self.status.clone());
                self.status = WorkflowStatus::Running(resume_state);
                Ok(())
            }
            WorkflowStatus::Running(_) => Err(InvalidTransition::NotFailed),
        }
    }
}

/// Status of the **optional**, post-root `hybrid_mount`/ViPER4Android-RE
/// integration. Deliberately a completely separate type from
/// [`WorkflowState`]/[`WorkflowStatus`] — not a state within the
/// mandatory chain, not tracked by [`Workflow`], and never capable of
/// marking the mandatory root workflow as failed. See
/// `WorkflowRunner::hybrid_mount_status` /
/// `WorkflowRunner::run_hybrid_mount_integration`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OptionalIntegrationStatus {
    /// The mandatory root workflow hasn't completed yet, so this hasn't
    /// been offered.
    #[default]
    NotOffered,
    /// Root completed; waiting for the user to decide.
    AwaitingChoice,
    /// The user explicitly chose not to run it. A successful outcome,
    /// not a failure.
    Skipped,
    /// A read-only check found `hybrid_mount` is not installed on the
    /// device. A valid configuration, not an error — the integration is
    /// skipped automatically.
    NotInstalled,
    /// Currently inspecting/applying the configuration.
    Running,
    /// Applied (or already correct) and verified.
    Succeeded,
    /// Failed. This is surfaced to the user but never retroactively
    /// marks the mandatory root workflow (already `Completed`) as
    /// failed.
    Failed { detail: String },
}

impl OptionalIntegrationStatus {
    pub fn label(&self) -> &'static str {
        match self {
            OptionalIntegrationStatus::NotOffered => "NotOffered",
            OptionalIntegrationStatus::AwaitingChoice => "AwaitingChoice",
            OptionalIntegrationStatus::Skipped => "Skipped",
            OptionalIntegrationStatus::NotInstalled => "NotInstalled",
            OptionalIntegrationStatus::Running => "Running",
            OptionalIntegrationStatus::Succeeded => "Succeeded",
            OptionalIntegrationStatus::Failed { .. } => "Failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_disconnected() {
        let wf = Workflow::new();
        assert_eq!(
            *wf.status(),
            WorkflowStatus::Running(WorkflowState::Disconnected)
        );
    }

    #[test]
    fn advances_sequentially() {
        let mut wf = Workflow::new();
        wf.advance_to(WorkflowState::Disconnected, WorkflowState::DeviceDetected)
            .unwrap();
        assert_eq!(
            *wf.status(),
            WorkflowStatus::Running(WorkflowState::DeviceDetected)
        );
    }

    #[test]
    fn rejects_skipping_states() {
        let mut wf = Workflow::new();
        let err = wf
            .advance_to(WorkflowState::Disconnected, WorkflowState::DeviceVerified)
            .unwrap_err();
        assert!(matches!(err, InvalidTransition::NotSequential { .. }));
    }

    #[test]
    fn rejects_advance_from_wrong_current_state() {
        let mut wf = Workflow::new();
        let err = wf
            .advance_to(WorkflowState::DeviceDetected, WorkflowState::DeviceVerified)
            .unwrap_err();
        assert!(matches!(err, InvalidTransition::WrongCurrentState { .. }));
    }

    #[test]
    fn device_mismatch_never_reaches_payload_execution() {
        let mut wf = Workflow::new();
        wf.advance_to(WorkflowState::Disconnected, WorkflowState::DeviceDetected)
            .unwrap();
        // Verification fails: device mismatch.
        wf.fail(WorkflowFailure::DeviceMismatch {
            detail: "wrong model".into(),
        });
        assert!(wf.status().is_failed());

        // Any attempt to resume at or beyond PayloadExecuting must be rejected.
        let err = wf.retry_from(WorkflowState::PayloadExecuting).unwrap_err();
        assert!(matches!(
            err,
            InvalidTransition::CannotResumeAheadOfLastGood { .. }
        ));

        // The only valid resume point is DeviceDetected (re-attempt verification).
        wf.retry_from(WorkflowState::DeviceDetected).unwrap();
        assert_eq!(
            *wf.status(),
            WorkflowStatus::Running(WorkflowState::DeviceDetected)
        );
    }

    #[test]
    fn cannot_advance_while_failed() {
        let mut wf = Workflow::new();
        wf.fail(WorkflowFailure::DeviceDisconnected);
        let err = wf
            .advance_to(WorkflowState::Disconnected, WorkflowState::DeviceDetected)
            .unwrap_err();
        assert!(matches!(err, InvalidTransition::CannotAdvanceWhileFailed));
    }

    #[test]
    fn retry_requires_failed_status() {
        let mut wf = Workflow::new();
        let err = wf.retry_from(WorkflowState::Disconnected).unwrap_err();
        assert!(matches!(err, InvalidTransition::NotFailed));
    }

    #[test]
    fn cancellation_is_a_distinct_failure() {
        let mut wf = Workflow::new();
        wf.advance_to(WorkflowState::Disconnected, WorkflowState::DeviceDetected)
            .unwrap();
        wf.fail(WorkflowFailure::UserCancelled);
        match wf.status() {
            WorkflowStatus::Failed { failure, .. } => assert_eq!(failure.label(), "UserCancelled"),
            _ => panic!("expected failed status"),
        }
    }

    #[test]
    fn full_happy_path_reaches_completed() {
        let mut wf = Workflow::new();
        for i in 0..WORKFLOW_ORDER.len() - 1 {
            let from = WORKFLOW_ORDER[i];
            let to = WORKFLOW_ORDER[i + 1];
            wf.advance_to(from, to).unwrap();
        }
        assert!(wf.status().is_completed());
    }
}
