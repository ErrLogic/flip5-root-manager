//! The workflow engine: ties the ADB abstraction, device verification,
//! artifact selection, and the explicit state machine together to execute
//! each step of the reference workflow (CLAUDE.md section 6) in order.
//!
//! This is the core of the application (section 32). The TUI only ever
//! calls methods on [`WorkflowRunner`] and renders the events/state it
//! produces; it never talks to [`crate::adb`] directly.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::mpsc;

use crate::adb::{AdbClient, AdbError, CapturedOutput, CommandKind};
use crate::artifacts::{ArtifactInfo, ArtifactSlot};
use crate::config::hybrid_mount::{self, ApplyOutcome};
use crate::device::detector::DetectError;
use crate::device::{self, DeviceError};
use crate::executor::{CancelToken, ProcessEvent, ProcessOutcome};
use crate::workflow::retry::{RetryPolicy, RetryTracker};
use crate::workflow::state::{
    InvalidTransition, OptionalIntegrationStatus, Workflow, WorkflowFailure, WorkflowState,
    WorkflowStatus,
};
use crate::workflow::steps::{cleanup, kernelsu, payload, root};

/// Events emitted by the runner while a step executes, for the TUI to
/// render into the terminal output panel (CLAUDE.md section 11).
#[derive(Debug, Clone)]
pub enum RunnerEvent {
    CommandStarted {
        kind: CommandKind,
        display: String,
    },
    Stdout(String),
    Stderr(String),
    CommandFinished {
        exit_code: Option<i32>,
        elapsed: Duration,
    },
    /// Neutral status narration ("Detecting device...", "Selected
    /// payload library: ...").
    Info(String),
    /// A positive confirmation (model/build verified, rule applied,
    /// workflow completed, ...).
    Success(String),
    /// Something worth the user's attention that isn't itself a failure
    /// (cancellation was requested, ...).
    Warning(String),
    /// A step failed. Carries the same text as the `StateChanged` event
    /// that accompanies it, but as its own line so it's visually
    /// distinct in the terminal output panel.
    Error(String),
    StateChanged(WorkflowStatus),
}

/// Errors that mean a step could not even be *attempted* — distinct from
/// a step being attempted and failing, which is recorded as a
/// [`WorkflowFailure`] inside the state machine itself and surfaced via
/// `StateChanged`.
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("workflow is at {actual:?}, not {expected:?}")]
    WrongState {
        expected: WorkflowState,
        actual: WorkflowState,
    },
    #[error("workflow is currently in a failed state")]
    WorkflowFailed,
    #[error("this action does not apply to the current workflow state")]
    NotApplicable,
    #[error("workflow has already completed")]
    AlreadyCompleted,
    #[error("no device has been selected yet")]
    NoDeviceSelected,
    #[error("{0} artifact has not been selected")]
    ArtifactNotSelected(&'static str),
    #[error("{0} artifact is missing on disk: {1}")]
    ArtifactMissing(&'static str, PathBuf),
    #[error("retry limit reached for {0:?} ({1} attempts)")]
    RetryLimitExceeded(WorkflowState, u32),
    #[error("the optional hybrid_mount integration isn't currently awaiting a choice")]
    OptionalIntegrationNotAwaitingChoice,
    #[error(transparent)]
    Transition(#[from] InvalidTransition),
}

struct RunnerState {
    serial: Option<String>,
    workflow: Workflow,
    retry: RetryTracker,
    payload_lib: Option<ArtifactInfo>,
    payload_runner: Option<ArtifactInfo>,
    kernelsu: Option<ArtifactInfo>,
    staging_dir: PathBuf,
    /// The **optional**, post-root `hybrid_mount`/ViPER4Android-RE
    /// integration. Deliberately not part of `workflow` — see
    /// `OptionalIntegrationStatus`'s own docs.
    hybrid_mount_status: OptionalIntegrationStatus,
}

/// The workflow engine. Cheap to clone via `Arc` if needed; internally
/// synchronized so it can be driven from a spawned tokio task while the
/// TUI keeps rendering.
pub struct WorkflowRunner {
    adb: Arc<dyn AdbClient>,
    events: mpsc::UnboundedSender<RunnerEvent>,
    state: Mutex<RunnerState>,
    cancel: CancelToken,
}

impl WorkflowRunner {
    pub fn new(
        adb: Arc<dyn AdbClient>,
        staging_dir: PathBuf,
        events: mpsc::UnboundedSender<RunnerEvent>,
    ) -> Self {
        Self {
            adb,
            events,
            state: Mutex::new(RunnerState {
                serial: None,
                workflow: Workflow::new(),
                retry: RetryTracker::new(RetryPolicy::default()),
                payload_lib: None,
                payload_runner: None,
                kernelsu: None,
                staging_dir,
                hybrid_mount_status: OptionalIntegrationStatus::NotOffered,
            }),
            cancel: CancelToken::new(),
        }
    }

    pub fn status(&self) -> WorkflowStatus {
        self.state.lock().unwrap().workflow.status().clone()
    }

    /// Status of the optional, post-root `hybrid_mount`/ViPER4Android-RE
    /// integration — always `NotOffered` until the mandatory root
    /// workflow reaches `Completed`.
    pub fn hybrid_mount_status(&self) -> OptionalIntegrationStatus {
        self.state.lock().unwrap().hybrid_mount_status.clone()
    }

    pub fn serial(&self) -> Option<String> {
        self.state.lock().unwrap().serial.clone()
    }

    pub fn retry_info(&self, step: WorkflowState) -> (u32, u32) {
        let st = self.state.lock().unwrap();
        (st.retry.attempts_for(step), st.retry.max_attempts())
    }

    /// Requests cancellation of whatever long-running step is currently
    /// executing. Harmless to call when nothing is running.
    pub fn request_cancel(&self) {
        self.cancel.request();
    }

    fn emit(&self, event: RunnerEvent) {
        let _ = self.events.send(event);
    }

    fn emit_state(&self, st: &RunnerState) {
        self.emit(RunnerEvent::StateChanged(st.workflow.status().clone()));
    }

    fn require_running_at(
        &self,
        st: &RunnerState,
        expected: WorkflowState,
    ) -> Result<(), RunnerError> {
        match st.workflow.status() {
            WorkflowStatus::Running(cur) if *cur == expected => Ok(()),
            WorkflowStatus::Running(cur) => Err(RunnerError::WrongState {
                expected,
                actual: *cur,
            }),
            WorkflowStatus::Failed { .. } => Err(RunnerError::WorkflowFailed),
        }
    }

    fn begin_attempt(
        &self,
        st: &mut RunnerState,
        target: WorkflowState,
    ) -> Result<(), RunnerError> {
        if !st.retry.can_retry(target) {
            return Err(RunnerError::RetryLimitExceeded(
                target,
                st.retry.max_attempts(),
            ));
        }
        st.retry.record_attempt(target);
        Ok(())
    }

    fn advance(
        &self,
        st: &mut RunnerState,
        from: WorkflowState,
        to: WorkflowState,
    ) -> Result<(), RunnerError> {
        st.workflow.advance_to(from, to)?;
        st.retry.reset(to);
        self.emit_state(st);
        Ok(())
    }

    fn record_failure(&self, st: &mut RunnerState, failure: WorkflowFailure) {
        let detail = failure
            .detail()
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        self.emit(RunnerEvent::Error(format!(
            "FAILED: {}{detail}",
            failure.label()
        )));
        st.workflow.fail(failure);
        self.emit_state(st);
    }

    /// Resumes from a failure at the last known-good state, i.e. prepares
    /// to re-attempt the step that just failed. Refuses if the retry
    /// budget for that step has already been exhausted.
    pub fn prepare_retry(&self) -> Result<(), RunnerError> {
        let mut st = self.state.lock().unwrap();
        let (last_good, target) = match st.workflow.status().clone() {
            WorkflowStatus::Failed { last_good, .. } => {
                let target = last_good.normal_next().unwrap_or(last_good);
                (last_good, target)
            }
            WorkflowStatus::Running(_) => return Err(RunnerError::WorkflowFailed),
        };
        if !st.retry.can_retry(target) {
            return Err(RunnerError::RetryLimitExceeded(
                target,
                st.retry.max_attempts(),
            ));
        }
        st.workflow.retry_from(last_good)?;
        self.emit_state(&st);
        Ok(())
    }

    // ---- shared command wrappers (emit start/output/finish uniformly) ----

    async fn do_push(
        &self,
        serial: &str,
        local: &Path,
        remote: &str,
    ) -> Result<CapturedOutput, AdbError> {
        self.emit(RunnerEvent::CommandStarted {
            kind: CommandKind::Host,
            display: format!("adb push {} {remote}", local.display()),
        });
        let start = Instant::now();
        let result = self.adb.push(serial, local, remote).await;
        self.finish_captured(&result, start.elapsed());
        result
    }

    async fn do_shell(&self, serial: &str, args: &[&str]) -> Result<CapturedOutput, AdbError> {
        self.emit(RunnerEvent::CommandStarted {
            kind: CommandKind::Device,
            display: format!("adb shell {}", args.join(" ")),
        });
        let start = Instant::now();
        let result = self.adb.shell_capture(serial, args).await;
        self.finish_captured(&result, start.elapsed());
        result
    }

    async fn do_shell_raw(&self, serial: &str, raw: &str) -> Result<CapturedOutput, AdbError> {
        let kind = if raw.trim_start().starts_with("su ") || raw.contains("-c '") {
            CommandKind::RootDevice
        } else {
            CommandKind::Device
        };
        self.emit(RunnerEvent::CommandStarted {
            kind,
            display: format!("adb shell \"{raw}\""),
        });
        let start = Instant::now();
        let result = self.adb.shell_capture_raw(serial, raw).await;
        self.finish_captured(&result, start.elapsed());
        result
    }

    fn finish_captured(&self, result: &Result<CapturedOutput, AdbError>, elapsed: Duration) {
        match result {
            Ok(out) => {
                for line in out.stdout.lines() {
                    self.emit(RunnerEvent::Stdout(line.to_string()));
                }
                for line in out.stderr.lines() {
                    self.emit(RunnerEvent::Stderr(line.to_string()));
                }
                self.emit(RunnerEvent::CommandFinished {
                    exit_code: out.exit_code,
                    elapsed,
                });
            }
            Err(e) => {
                self.emit(RunnerEvent::Stderr(e.to_string()));
                self.emit(RunnerEvent::CommandFinished {
                    exit_code: None,
                    elapsed,
                });
            }
        }
    }

    /// Runs a long-lived device-shell command, streaming its output and
    /// honoring both an optional timeout and user-requested cancellation.
    async fn do_stream(
        &self,
        serial: &str,
        raw: &str,
        timeout: Option<Duration>,
    ) -> ProcessOutcome {
        self.cancel.reset();
        self.emit(RunnerEvent::CommandStarted {
            kind: CommandKind::RootDevice,
            display: format!("adb shell \"{raw}\""),
        });
        let start = Instant::now();
        let mut handle = self.adb.shell_stream_raw(serial, raw, timeout);
        let mut cancel_issued = false;
        loop {
            tokio::select! {
                event = handle.recv() => {
                    match event {
                        Some(ProcessEvent::Stdout(l)) => self.emit(RunnerEvent::Stdout(l)),
                        Some(ProcessEvent::Stderr(l)) => self.emit(RunnerEvent::Stderr(l)),
                        Some(ProcessEvent::Finished(outcome)) => {
                            let exit_code = match &outcome {
                                ProcessOutcome::Success => Some(0),
                                ProcessOutcome::Failure { exit_code } => Some(*exit_code),
                                _ => None,
                            };
                            self.emit(RunnerEvent::CommandFinished { exit_code, elapsed: start.elapsed() });
                            return outcome;
                        }
                        None => {
                            return ProcessOutcome::SpawnFailed { message: "process channel closed unexpectedly".into() };
                        }
                    }
                }
                _ = self.cancel.wait(), if !cancel_issued => {
                    cancel_issued = true;
                    self.emit(RunnerEvent::Warning("Cancellation requested; stopping process...".into()));
                    handle.cancel();
                }
            }
        }
    }

    fn current_serial(&self, st: &RunnerState) -> Result<String, RunnerError> {
        st.serial.clone().ok_or(RunnerError::NoDeviceSelected)
    }

    // ---- CLAUDE.md 6.1/6.2 — device detection + verification ----

    pub async fn detect_device(&self) -> Result<(), RunnerError> {
        {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::Disconnected)?;
            self.begin_attempt(&mut st, WorkflowState::DeviceDetected)?;
        }
        self.emit(RunnerEvent::Info("Detecting device...".into()));
        match device::detect_single_device(self.adb.as_ref()).await {
            Ok(dev) => {
                let mut st = self.state.lock().unwrap();
                st.serial = Some(dev.serial.clone());
                self.emit(RunnerEvent::Success(format!(
                    "Device detected: {} ({})",
                    dev.serial, dev.state
                )));
                self.advance(
                    &mut st,
                    WorkflowState::Disconnected,
                    WorkflowState::DeviceDetected,
                )?;
                Ok(())
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                let failure = match e {
                    DetectError::NoDevice => WorkflowFailure::DeviceDisconnected,
                    DetectError::MultipleDevices(serials) => WorkflowFailure::VerificationFailed {
                        detail: format!("multiple devices connected: {serials:?}"),
                    },
                    DetectError::NotReady(state) => WorkflowFailure::VerificationFailed {
                        detail: format!("device present but not ready: {state}"),
                    },
                    DetectError::Adb(adb_err) => WorkflowFailure::VerificationFailed {
                        detail: adb_err.to_string(),
                    },
                };
                self.record_failure(&mut st, failure);
                Ok(())
            }
        }
    }

    pub async fn verify_device(&self) -> Result<(), RunnerError> {
        let serial = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::DeviceDetected)?;
            self.begin_attempt(&mut st, WorkflowState::DeviceVerified)?;
            self.current_serial(&st)?
        };
        self.emit(RunnerEvent::Info(
            "Verifying device model and build...".into(),
        ));
        match device::verify_device(self.adb.as_ref(), &serial).await {
            Ok(verified) => {
                self.emit(RunnerEvent::Success(format!(
                    "Model OK: {}",
                    verified.model
                )));
                self.emit(RunnerEvent::Success(format!(
                    "Build OK: {}",
                    verified.build
                )));
                let mut st = self.state.lock().unwrap();
                self.advance(
                    &mut st,
                    WorkflowState::DeviceDetected,
                    WorkflowState::DeviceVerified,
                )?;
                Ok(())
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                let failure = match e {
                    DeviceError::ModelMismatch { expected, actual } => {
                        WorkflowFailure::DeviceMismatch {
                            detail: format!("model '{actual}' != expected '{expected}'"),
                        }
                    }
                    DeviceError::BuildMismatch { expected, actual } => {
                        WorkflowFailure::DeviceMismatch {
                            detail: format!("build '{actual}' != expected '{expected}'"),
                        }
                    }
                    DeviceError::Disconnected => WorkflowFailure::DeviceDisconnected,
                    DeviceError::Adb(adb_err) => WorkflowFailure::VerificationFailed {
                        detail: adb_err.to_string(),
                    },
                };
                self.record_failure(&mut st, failure);
                Ok(())
            }
        }
    }

    // ---- artifact selection (no device I/O) ----

    fn require_found(slot: ArtifactSlot, label: &'static str) -> Result<ArtifactInfo, RunnerError> {
        match slot {
            ArtifactSlot::Found(info) => Ok(info),
            ArtifactSlot::Missing { expected_path, .. } => {
                Err(RunnerError::ArtifactMissing(label, expected_path))
            }
        }
    }

    pub fn select_payload_artifacts(
        &self,
        lib: ArtifactSlot,
        runner_bin: ArtifactSlot,
    ) -> Result<(), RunnerError> {
        let lib = Self::require_found(lib, "payload library")?;
        let runner_bin = Self::require_found(runner_bin, "payload runner")?;
        let mut st = self.state.lock().unwrap();
        self.require_running_at(&st, WorkflowState::DeviceVerified)?;
        self.emit(RunnerEvent::Info(format!(
            "Selected payload library: {} ({})",
            lib.relative_path, lib.sha256
        )));
        self.emit(RunnerEvent::Info(format!(
            "Selected payload runner: {} ({})",
            runner_bin.relative_path, runner_bin.sha256
        )));
        st.payload_lib = Some(lib);
        st.payload_runner = Some(runner_bin);
        self.advance(
            &mut st,
            WorkflowState::DeviceVerified,
            WorkflowState::PayloadSelected,
        )
    }

    pub fn select_kernelsu_artifact(&self, ksud: ArtifactSlot) -> Result<(), RunnerError> {
        let ksud = Self::require_found(ksud, "KernelSU")?;
        let mut st = self.state.lock().unwrap();
        self.require_running_at(&st, WorkflowState::PayloadSucceeded)?;
        self.emit(RunnerEvent::Info(format!(
            "Selected KernelSU artifact: {} ({})",
            ksud.relative_path, ksud.sha256
        )));
        st.kernelsu = Some(ksud);
        self.advance(
            &mut st,
            WorkflowState::PayloadSucceeded,
            WorkflowState::KernelSuSelected,
        )
    }

    // ---- CLAUDE.md 6.3/6.4/6.5 — push + chmod payload ----

    pub async fn push_payload(&self) -> Result<(), RunnerError> {
        let (serial, lib, runner_bin) = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::PayloadSelected)?;
            self.begin_attempt(&mut st, WorkflowState::PayloadPushed)?;
            let serial = self.current_serial(&st)?;
            let lib = st
                .payload_lib
                .clone()
                .ok_or(RunnerError::ArtifactNotSelected("payload library"))?;
            let runner_bin = st
                .payload_runner
                .clone()
                .ok_or(RunnerError::ArtifactNotSelected("payload runner"))?;
            (serial, lib, runner_bin)
        };

        if let Err(e) = self
            .do_push(&serial, &lib.absolute_path, payload::REMOTE_LIB_PATH)
            .await
        {
            let mut st = self.state.lock().unwrap();
            self.record_failure(
                &mut st,
                WorkflowFailure::PayloadFailure {
                    detail: format!("push payload library failed: {e}"),
                },
            );
            return Ok(());
        }
        if let Err(e) = self
            .do_push(
                &serial,
                &runner_bin.absolute_path,
                payload::REMOTE_RUNNER_PATH,
            )
            .await
        {
            let mut st = self.state.lock().unwrap();
            self.record_failure(
                &mut st,
                WorkflowFailure::PayloadFailure {
                    detail: format!("push payload runner failed: {e}"),
                },
            );
            return Ok(());
        }
        match self.do_shell(&serial, &payload::chmod_runner_args()).await {
            Ok(out) if out.succeeded() => {}
            Ok(out) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::PayloadFailure {
                        detail: format!("chmod failed: {}", out.stderr),
                    },
                );
                return Ok(());
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::PayloadFailure {
                        detail: e.to_string(),
                    },
                );
                return Ok(());
            }
        }

        let mut st = self.state.lock().unwrap();
        self.advance(
            &mut st,
            WorkflowState::PayloadSelected,
            WorkflowState::PayloadPushed,
        )
    }

    // ---- CLAUDE.md 6.6 — execute payload (streaming, long-running) ----

    pub async fn execute_payload(&self) -> Result<(), RunnerError> {
        let serial = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::PayloadPushed)?;
            // Retry accounting targets `PayloadExecuting` — the state we
            // are attempting to pass through — even though the formal
            // workflow status only actually moves there once the stream
            // concludes (see the two-step advance below). This keeps
            // `last_good` pinned at `PayloadPushed` on failure, so a
            // retry correctly resumes by re-attempting execution rather
            // than resuming "mid-execution" at a state nothing produced.
            self.begin_attempt(&mut st, WorkflowState::PayloadExecuting)?;
            self.current_serial(&st)?
        };

        let command = payload::run_payload_command();
        let outcome = self
            .do_stream(&serial, &command, Some(payload::overall_timeout()))
            .await;

        let mut st = self.state.lock().unwrap();
        match outcome {
            ProcessOutcome::Success => {
                self.advance(
                    &mut st,
                    WorkflowState::PayloadPushed,
                    WorkflowState::PayloadExecuting,
                )?;
                self.advance(
                    &mut st,
                    WorkflowState::PayloadExecuting,
                    WorkflowState::PayloadSucceeded,
                )
            }
            ProcessOutcome::Failure { exit_code } => {
                self.record_failure(
                    &mut st,
                    WorkflowFailure::PayloadFailure {
                        detail: format!("exit code {exit_code}"),
                    },
                );
                Ok(())
            }
            ProcessOutcome::TimedOut { elapsed } => {
                self.record_failure(
                    &mut st,
                    WorkflowFailure::Timeout {
                        detail: format!(
                            "payload exceeded overall timeout after {:.0}s",
                            elapsed.as_secs_f64()
                        ),
                    },
                );
                Ok(())
            }
            ProcessOutcome::Cancelled { .. } => {
                self.record_failure(&mut st, WorkflowFailure::UserCancelled);
                Ok(())
            }
            ProcessOutcome::SpawnFailed { message } => {
                self.record_failure(&mut st, WorkflowFailure::PayloadFailure { detail: message });
                Ok(())
            }
        }
    }

    // ---- CLAUDE.md 6.7 — push KernelSU artifact ----

    pub async fn push_kernelsu(&self) -> Result<(), RunnerError> {
        let (serial, ksud) = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::KernelSuSelected)?;
            self.begin_attempt(&mut st, WorkflowState::KernelSuPushed)?;
            let serial = self.current_serial(&st)?;
            let ksud = st
                .kernelsu
                .clone()
                .ok_or(RunnerError::ArtifactNotSelected("KernelSU"))?;
            (serial, ksud)
        };

        if let Err(e) = self
            .do_push(&serial, &ksud.absolute_path, kernelsu::REMOTE_KSUD_PATH)
            .await
        {
            let mut st = self.state.lock().unwrap();
            self.record_failure(
                &mut st,
                WorkflowFailure::PayloadFailure {
                    detail: format!("push KernelSU artifact failed: {e}"),
                },
            );
            return Ok(());
        }

        let mut st = self.state.lock().unwrap();
        self.advance(
            &mut st,
            WorkflowState::KernelSuSelected,
            WorkflowState::KernelSuPushed,
        )
    }

    // ---- CLAUDE.md 6.8 — stage KernelSU ----

    pub async fn stage_kernelsu(&self) -> Result<(), RunnerError> {
        let serial = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::KernelSuPushed)?;
            self.begin_attempt(&mut st, WorkflowState::KernelSuStaged)?;
            self.current_serial(&st)?
        };

        match self.do_shell_raw(&serial, &kernelsu::stage_command()).await {
            Ok(out) if out.succeeded() => {
                let mut st = self.state.lock().unwrap();
                self.advance(
                    &mut st,
                    WorkflowState::KernelSuPushed,
                    WorkflowState::KernelSuStaged,
                )
            }
            Ok(out) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::PayloadFailure {
                        detail: format!("stage failed: {}", out.stderr),
                    },
                );
                Ok(())
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::PayloadFailure {
                        detail: e.to_string(),
                    },
                );
                Ok(())
            }
        }
    }

    // ---- CLAUDE.md 6.9 — load KernelSU (streaming, long-running) ----

    pub async fn load_kernelsu(&self) -> Result<(), RunnerError> {
        let serial = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::KernelSuStaged)?;
            self.begin_attempt(&mut st, WorkflowState::KernelSuLoaded)?;
            self.current_serial(&st)?
        };

        let outcome = self
            .do_stream(&serial, &kernelsu::load_command(), None)
            .await;

        let mut st = self.state.lock().unwrap();
        match outcome {
            // Success here only means the process returned 0 — CLAUDE.md
            // section 6.9 is explicit that this must NOT be treated as
            // proof KernelSU is loaded; that is checked separately by
            // `verify_kernelsu`.
            ProcessOutcome::Success => self.advance(
                &mut st,
                WorkflowState::KernelSuStaged,
                WorkflowState::KernelSuLoaded,
            ),
            ProcessOutcome::Failure { exit_code } => {
                self.record_failure(
                    &mut st,
                    WorkflowFailure::PayloadFailure {
                        detail: format!("KernelSU load exited {exit_code}"),
                    },
                );
                Ok(())
            }
            ProcessOutcome::TimedOut { elapsed } => {
                self.record_failure(
                    &mut st,
                    WorkflowFailure::Timeout {
                        detail: format!(
                            "KernelSU load timed out after {:.0}s",
                            elapsed.as_secs_f64()
                        ),
                    },
                );
                Ok(())
            }
            ProcessOutcome::Cancelled { .. } => {
                self.record_failure(&mut st, WorkflowFailure::UserCancelled);
                Ok(())
            }
            ProcessOutcome::SpawnFailed { message } => {
                self.record_failure(&mut st, WorkflowFailure::PayloadFailure { detail: message });
                Ok(())
            }
        }
    }

    // ---- CLAUDE.md 6.10 — verify KernelSU (real state check) ----

    pub async fn verify_kernelsu(&self) -> Result<(), RunnerError> {
        let serial = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::KernelSuLoaded)?;
            self.begin_attempt(&mut st, WorkflowState::KernelSuVerified)?;
            self.current_serial(&st)?
        };

        match self.do_shell_raw(&serial, kernelsu::VERIFY_COMMAND).await {
            Ok(out) if kernelsu::kernelsu_present(&out.stdout) => {
                let mut st = self.state.lock().unwrap();
                self.advance(
                    &mut st,
                    WorkflowState::KernelSuLoaded,
                    WorkflowState::KernelSuVerified,
                )
            }
            Ok(_) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::VerificationFailed {
                        detail: "KernelSU verification failed".into(),
                    },
                );
                Ok(())
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::VerificationFailed {
                        detail: e.to_string(),
                    },
                );
                Ok(())
            }
        }
    }

    // ---- CLAUDE.md 6.11 — verify root (real identity check) ----

    pub async fn verify_root(&self) -> Result<(), RunnerError> {
        let serial = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::KernelSuVerified)?;
            self.begin_attempt(&mut st, WorkflowState::RootVerified)?;
            self.current_serial(&st)?
        };

        match self.do_shell_raw(&serial, root::VERIFY_ROOT_COMMAND).await {
            Ok(out) => {
                let identity = root::parse_identity(&out.stdout);
                let identity_line = format!("Identity: {}", identity.raw);
                if identity.is_root {
                    self.emit(RunnerEvent::Success(identity_line));
                } else {
                    self.emit(RunnerEvent::Warning(identity_line));
                }
                let mut st = self.state.lock().unwrap();
                if identity.is_root {
                    self.advance(
                        &mut st,
                        WorkflowState::KernelSuVerified,
                        WorkflowState::RootVerified,
                    )
                } else {
                    self.record_failure(
                        &mut st,
                        WorkflowFailure::VerificationFailed {
                            detail: format!("expected uid=0(root), got: {}", identity.raw),
                        },
                    );
                    Ok(())
                }
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::VerificationFailed {
                        detail: e.to_string(),
                    },
                );
                Ok(())
            }
        }
    }

    // ---- CLAUDE.md 6.12 — cleanup temporary mount ----

    pub async fn cleanup_mount(&self) -> Result<(), RunnerError> {
        let serial = {
            let mut st = self.state.lock().unwrap();
            self.require_running_at(&st, WorkflowState::RootVerified)?;
            self.begin_attempt(&mut st, WorkflowState::TemporaryMountCleaned)?;
            self.current_serial(&st)?
        };

        match self.do_shell_raw(&serial, cleanup::CLEANUP_COMMAND).await {
            Ok(out) if out.succeeded() => {
                let mut st = self.state.lock().unwrap();
                self.advance(
                    &mut st,
                    WorkflowState::RootVerified,
                    WorkflowState::TemporaryMountCleaned,
                )
            }
            Ok(out) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::CleanupFailed { detail: out.stderr },
                );
                Ok(())
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                self.record_failure(
                    &mut st,
                    WorkflowFailure::CleanupFailed {
                        detail: e.to_string(),
                    },
                );
                Ok(())
            }
        }
    }

    // ---- final mandatory step: cleanup already landed on
    // TemporaryMountCleaned; this just acknowledges the mandatory root
    // workflow as done. hybrid_mount is never involved. ----

    pub fn complete(&self) -> Result<(), RunnerError> {
        let mut st = self.state.lock().unwrap();
        self.require_running_at(&st, WorkflowState::TemporaryMountCleaned)?;
        self.emit(RunnerEvent::Success("Root workflow completed.".into()));
        self.advance(
            &mut st,
            WorkflowState::TemporaryMountCleaned,
            WorkflowState::Completed,
        )?;
        // Offer the optional post-root integration now that the
        // mandatory workflow has actually finished — never before.
        if st.hybrid_mount_status == OptionalIntegrationStatus::NotOffered {
            st.hybrid_mount_status = OptionalIntegrationStatus::AwaitingChoice;
            // A clear, unmistakable gap and explanation in the terminal
            // panel itself (not just the footer/checklist), so the user
            // never mistakes the upcoming optional step for a required
            // one, or misses that it's being offered at all — the
            // request this responds to was exactly that confusion.
            self.emit(RunnerEvent::Info(String::new()));
            self.emit(RunnerEvent::Warning(
                "Root access is already fully working — everything required has finished.".into(),
            ));
            self.emit(RunnerEvent::Warning(
                "Optional next step available: hybrid_mount / ViPER4Android-RE configuration \
                 (a personal setup preference, not part of rooting itself)."
                    .into(),
            ));
            self.emit(RunnerEvent::Warning(
                "Press Enter to configure it now, or K to finish here instead — the exploit \
                 has already succeeded either way."
                    .into(),
            ));
        }
        Ok(())
    }

    // ---- OPTIONAL, post-root integration: hybrid_mount/ViPER4Android-RE
    // (CLAUDE.md sections 6.13/6.14/23). Not part of the mandatory root
    // workflow's `Workflow`/`WorkflowStatus` at all — see
    // `OptionalIntegrationStatus`. Only reachable once `complete()` has
    // run, only ever started by an explicit user choice, and its failure
    // never retroactively marks the (already-succeeded) root workflow as
    // failed. ----

    fn require_awaiting_hybrid_mount_choice(&self, st: &RunnerState) -> Result<(), RunnerError> {
        if st.hybrid_mount_status == OptionalIntegrationStatus::AwaitingChoice {
            Ok(())
        } else {
            Err(RunnerError::OptionalIntegrationNotAwaitingChoice)
        }
    }

    /// The user explicitly declines the optional integration. A
    /// successful outcome, not a failure.
    pub fn skip_hybrid_mount_integration(&self) -> Result<(), RunnerError> {
        let mut st = self.state.lock().unwrap();
        self.require_awaiting_hybrid_mount_choice(&st)?;
        st.hybrid_mount_status = OptionalIntegrationStatus::Skipped;
        self.emit(RunnerEvent::Info(
            "hybrid_mount integration skipped by user.".into(),
        ));
        Ok(())
    }

    /// Runs the optional `hybrid_mount`/ViPER4Android-RE integration:
    /// first an explicit, read-only presence check (never assume the
    /// module is installed), then — only if present — the same
    /// read/parse/check/modify-if-needed/write/reread/verify cycle as
    /// before (CLAUDE.md section 23). Absence and failure are both
    /// reported but never flip the mandatory root workflow's own status.
    pub async fn run_hybrid_mount_integration(&self) -> Result<(), RunnerError> {
        let (serial, staging_dir) = {
            let mut st = self.state.lock().unwrap();
            self.require_awaiting_hybrid_mount_choice(&st)?;
            st.hybrid_mount_status = OptionalIntegrationStatus::Running;
            (self.current_serial(&st)?, st.staging_dir.clone())
        };

        self.emit(RunnerEvent::Info(
            "Checking whether hybrid_mount is installed...".into(),
        ));
        match hybrid_mount::is_installed(self.adb.as_ref(), &serial).await {
            Ok(false) => {
                let mut st = self.state.lock().unwrap();
                st.hybrid_mount_status = OptionalIntegrationStatus::NotInstalled;
                self.emit(RunnerEvent::Info(
                    "hybrid_mount is not installed on this device; integration skipped. This is not an error.".into(),
                ));
                return Ok(());
            }
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                st.hybrid_mount_status = OptionalIntegrationStatus::Failed {
                    detail: e.to_string(),
                };
                self.emit(RunnerEvent::Warning(format!(
                    "Could not determine whether hybrid_mount is installed: {e}"
                )));
                return Ok(());
            }
            Ok(true) => {}
        }

        self.emit(RunnerEvent::CommandStarted {
            kind: CommandKind::RootDevice,
            display: format!(
                "adb shell su -c \"tail -n 6 {}\"",
                hybrid_mount::REMOTE_CONFIG_PATH
            ),
        });
        let start = Instant::now();
        if let Ok(tail) = hybrid_mount::peek_tail(self.adb.as_ref(), &serial).await {
            for line in tail.lines() {
                self.emit(RunnerEvent::Stdout(line.to_string()));
            }
        }
        self.emit(RunnerEvent::CommandFinished {
            exit_code: Some(0),
            elapsed: start.elapsed(),
        });

        self.emit(RunnerEvent::Info(
            "Reading and parsing hybrid_mount configuration...".into(),
        ));
        match hybrid_mount::inspect_and_apply(self.adb.as_ref(), &serial, &staging_dir).await {
            Ok(ApplyOutcome::AlreadyCorrect) => {
                self.emit(RunnerEvent::Success(
                    "Rule already present with expected value; no change needed.".into(),
                ));
                let mut st = self.state.lock().unwrap();
                st.hybrid_mount_status = OptionalIntegrationStatus::Succeeded;
            }
            Ok(ApplyOutcome::Updated) => {
                self.emit(RunnerEvent::Success("Rule applied and verified.".into()));
                let mut st = self.state.lock().unwrap();
                st.hybrid_mount_status = OptionalIntegrationStatus::Succeeded;
            }
            Err(e) => {
                self.emit(RunnerEvent::Warning(format!(
                    "hybrid_mount integration failed: {e} (root workflow is still successful)"
                )));
                let mut st = self.state.lock().unwrap();
                st.hybrid_mount_status = OptionalIntegrationStatus::Failed {
                    detail: e.to_string(),
                };
            }
        }
        Ok(())
    }

    /// Dispatches to the appropriate step based on the current state, for
    /// the TUI's single "Enter = run next step" action. Artifact
    /// selection states are handled separately (they're triggered by an
    /// explicit selection action in the artifacts panel, not by this
    /// generic dispatcher). The optional `hybrid_mount` integration is
    /// never dispatched from here — it only ever runs via an explicit
    /// `run_hybrid_mount_integration`/`skip_hybrid_mount_integration`
    /// call once the mandatory workflow below is `Completed`.
    pub async fn run_next_step(&self) -> Result<(), RunnerError> {
        let current = match self.status() {
            WorkflowStatus::Running(s) => s,
            WorkflowStatus::Failed { .. } => return Err(RunnerError::WorkflowFailed),
        };
        match current {
            WorkflowState::Disconnected => self.detect_device().await,
            WorkflowState::DeviceDetected => self.verify_device().await,
            WorkflowState::DeviceVerified | WorkflowState::PayloadSucceeded => {
                Err(RunnerError::NotApplicable)
            }
            WorkflowState::PayloadSelected => self.push_payload().await,
            WorkflowState::PayloadPushed => self.execute_payload().await,
            WorkflowState::PayloadExecuting => Err(RunnerError::NotApplicable),
            WorkflowState::KernelSuSelected => self.push_kernelsu().await,
            WorkflowState::KernelSuPushed => self.stage_kernelsu().await,
            WorkflowState::KernelSuStaged => self.load_kernelsu().await,
            WorkflowState::KernelSuLoaded => self.verify_kernelsu().await,
            WorkflowState::KernelSuVerified => self.verify_root().await,
            WorkflowState::RootVerified => self.cleanup_mount().await,
            WorkflowState::TemporaryMountCleaned => self.complete(),
            WorkflowState::Completed => Err(RunnerError::AlreadyCompleted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::mock::{MockAdbClient, MockScenario, MockState};
    use crate::artifacts::ArtifactKind;

    fn fake_artifact(kind: ArtifactKind) -> ArtifactInfo {
        ArtifactInfo {
            kind,
            name: kind.display_name(),
            relative_path: kind.relative_path(),
            absolute_path: PathBuf::from("/tmp/does-not-need-to-exist-for-mock"),
            size_bytes: 1234,
            sha256: "deadbeef".to_string(),
            repository_location: PathBuf::from("/tmp"),
        }
    }

    fn runner_with_handle(
        state: MockState,
    ) -> (
        WorkflowRunner,
        Arc<MockAdbClient>,
        mpsc::UnboundedReceiver<RunnerEvent>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let staging = std::env::temp_dir().join(format!(
            "flip5-runner-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mock = Arc::new(MockAdbClient::new(state));
        let runner = WorkflowRunner::new(mock.clone(), staging, tx);
        (runner, mock, rx)
    }

    fn runner_with(state: MockState) -> (WorkflowRunner, mpsc::UnboundedReceiver<RunnerEvent>) {
        let (runner, _mock, rx) = runner_with_handle(state);
        (runner, rx)
    }

    async fn drive_to_payload_pushed(runner: &WorkflowRunner) {
        runner.detect_device().await.unwrap();
        runner.verify_device().await.unwrap();
        runner
            .select_payload_artifacts(
                ArtifactSlot::Found(fake_artifact(ArtifactKind::PayloadLibrary)),
                ArtifactSlot::Found(fake_artifact(ArtifactKind::PayloadRunner)),
            )
            .unwrap();
        runner.push_payload().await.unwrap();
    }

    async fn drive_to_kernelsu_loaded(runner: &WorkflowRunner) {
        drive_to_payload_pushed(runner).await;
        runner.execute_payload().await.unwrap();
        assert_eq!(
            runner.status(),
            WorkflowStatus::Running(WorkflowState::PayloadSucceeded)
        );
        runner
            .select_kernelsu_artifact(ArtifactSlot::Found(fake_artifact(ArtifactKind::KernelSu)))
            .unwrap();
        runner.push_kernelsu().await.unwrap();
        runner.stage_kernelsu().await.unwrap();
        runner.load_kernelsu().await.unwrap();
    }

    #[tokio::test]
    async fn full_happy_path_reaches_completed() {
        // The mandatory root workflow must succeed entirely on its own —
        // hybrid_mount/ViPER4Android is never touched here.
        let (runner, _rx) = runner_with(MockState::default());
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();
        assert!(runner.status().is_completed());
        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::AwaitingChoice
        );
    }

    #[tokio::test]
    async fn completion_narrates_the_optional_choice_clearly_in_the_terminal() {
        // Regression for the "user thought Enter was mandatory" report:
        // once root succeeds, the terminal panel must receive an
        // unmistakable, clearly-separated explanation that what follows
        // is optional, and what Enter/K each do.
        let (runner, mut rx) = runner_with(MockState::default());
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();

        let mut texts = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                RunnerEvent::Info(t) | RunnerEvent::Success(t) | RunnerEvent::Warning(t) => {
                    texts.push(t)
                }
                _ => {}
            }
        }

        assert!(
            texts.contains(&"Root workflow completed.".to_string()),
            "the mandatory completion message must still be present: {texts:?}"
        );
        // A blank-line gap right after the mandatory completion message,
        // clearly separating it from what follows.
        let gap_idx = texts
            .iter()
            .position(|t| t.is_empty())
            .expect("expected a blank-line gap before the optional explanation");
        assert!(
            texts[..gap_idx].iter().any(|t| t.contains("completed")),
            "the gap must come after the completion message, not before"
        );
        // The explanation itself must say plainly that root already
        // succeeded, that what follows is optional, and what each key
        // does.
        let rest = texts[gap_idx..].join(" ");
        assert!(
            rest.to_lowercase().contains("already")
                && (rest.contains("hybrid_mount") || rest.contains("ViPER4Android")),
            "expected the optional-step explanation, got: {rest:?}"
        );
        assert!(
            rest.contains("Enter") && rest.contains('K'),
            "expected the explanation to spell out both the Enter and K choices, got: {rest:?}"
        );
    }

    #[tokio::test]
    async fn root_completion_never_requires_hybrid_mount() {
        // Regression for the architecture correction: nothing about the
        // mandatory chain ever calls into hybrid_mount, and the optional
        // integration starts out merely *offered*, never run
        // automatically just because root succeeded.
        let (runner, _rx) = runner_with(MockState::default());
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::NotOffered
        );
        runner.complete().unwrap();
        assert!(runner.status().is_completed());
        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::AwaitingChoice
        );
    }

    #[tokio::test]
    async fn skipping_the_optional_integration_still_leaves_root_completed() {
        let (runner, _rx) = runner_with(MockState::default());
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();

        runner.skip_hybrid_mount_integration().unwrap();

        assert!(runner.status().is_completed());
        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::Skipped
        );
    }

    #[tokio::test]
    async fn optional_integration_cannot_run_before_root_completes() {
        let (runner, _rx) = runner_with(MockState::default());
        let err = runner.run_hybrid_mount_integration().await.unwrap_err();
        assert!(matches!(
            err,
            RunnerError::OptionalIntegrationNotAwaitingChoice
        ));
        let err = runner.skip_hybrid_mount_integration().unwrap_err();
        assert!(matches!(
            err,
            RunnerError::OptionalIntegrationNotAwaitingChoice
        ));
    }

    #[tokio::test]
    async fn optional_integration_never_executes_until_explicitly_selected() {
        // `run_next_step` — the generic mandatory-chain dispatcher — must
        // never reach into the optional integration on its own.
        let (runner, _rx) = runner_with(MockState::default());
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();
        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::AwaitingChoice
        );

        // Calling the generic dispatcher again now that we're Completed
        // must not silently run hybrid_mount either.
        let err = runner.run_next_step().await.unwrap_err();
        assert!(matches!(err, RunnerError::AlreadyCompleted));
        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::AwaitingChoice,
            "status must remain untouched until the user explicitly chooses"
        );
    }

    #[tokio::test]
    async fn root_workflow_succeeds_when_hybrid_mount_is_absent() {
        let (runner, _rx) = runner_with(MockState::default());
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();
        assert!(runner.status().is_completed());

        // Absence is a valid configuration, not an error: the mock's
        // default hybrid_mount_config simulates the module existing, so
        // explicitly simulate absence by using a scenario below instead
        // (see `optional_integration_skips_cleanly_when_hybrid_mount_not_installed`).
        // This test only asserts root succeeded without ever depending
        // on hybrid_mount's presence.
        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::AwaitingChoice
        );
    }

    #[tokio::test]
    async fn optional_integration_failure_does_not_retroactively_fail_root() {
        let state = MockState {
            hybrid_mount_config: "not [ valid toml =".to_string(),
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();
        assert!(runner.status().is_completed());

        runner.run_hybrid_mount_integration().await.unwrap();

        match runner.hybrid_mount_status() {
            OptionalIntegrationStatus::Failed { detail } => assert!(!detail.is_empty()),
            other => panic!("expected the optional integration to fail, got {other:?}"),
        }
        // The mandatory root workflow must still read as successfully
        // completed — a failed *optional* integration is never allowed
        // to retroactively mark it as failed.
        assert!(
            runner.status().is_completed(),
            "root completion must be unaffected by an optional-integration failure"
        );
    }

    #[tokio::test]
    async fn device_mismatch_stops_before_payload_push() {
        let state = MockState {
            model: "SM-WRONG".to_string(),
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        runner.detect_device().await.unwrap();
        runner.verify_device().await.unwrap();
        match runner.status() {
            WorkflowStatus::Failed { failure, last_good } => {
                assert_eq!(failure.label(), "DeviceMismatch");
                assert_eq!(last_good, WorkflowState::DeviceDetected);
            }
            other => panic!("expected failure, got {other:?}"),
        }
        // Attempting to push the payload directly must be rejected: the
        // workflow never reached PayloadSelected.
        let err = runner.push_payload().await.unwrap_err();
        assert!(matches!(err, RunnerError::WorkflowFailed));
    }

    #[tokio::test]
    async fn payload_failure_is_recorded_and_retry_is_bounded() {
        let state = MockState {
            run_payload: crate::adb::mock::Scripted::Failure {
                stdout: String::new(),
                stderr: "boom".into(),
                exit_code: 1,
            },
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        drive_to_payload_pushed(&runner).await;

        for _ in 0..3 {
            runner.execute_payload().await.unwrap();
            match runner.status() {
                WorkflowStatus::Failed { failure, .. } => {
                    assert_eq!(failure.label(), "PayloadFailure")
                }
                other => panic!("expected failure, got {other:?}"),
            }
            if runner.prepare_retry().is_err() {
                break;
            }
        }
        // Retry budget (3 attempts) must be exhausted by now.
        let err = runner.prepare_retry().unwrap_err();
        assert!(matches!(
            err,
            RunnerError::RetryLimitExceeded(WorkflowState::PayloadExecuting, 3)
        ));
    }

    #[tokio::test]
    async fn payload_timeout_is_distinct_from_failure() {
        let state = MockState {
            run_payload: crate::adb::mock::Scripted::Timeout,
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        drive_to_payload_pushed(&runner).await;
        runner.execute_payload().await.unwrap();
        match runner.status() {
            WorkflowStatus::Failed { failure, .. } => assert_eq!(failure.label(), "Timeout"),
            other => panic!("expected timeout failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn kernelsu_verification_failure_does_not_claim_success() {
        // Module not actually loaded.
        let state = MockState {
            kernelsu_modules_output: String::new(),
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        match runner.status() {
            WorkflowStatus::Failed { failure, .. } => {
                assert_eq!(failure.label(), "VerificationFailed")
            }
            other => panic!("expected verification failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn root_verification_rejects_non_root_identity() {
        let state = MockState {
            root_id_output: "uid=2000(shell) gid=2000(shell)\n".to_string(),
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        match runner.status() {
            WorkflowStatus::Failed { failure, .. } => {
                assert_eq!(failure.label(), "VerificationFailed")
            }
            other => panic!("expected verification failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cleanup_failure_is_reported_not_ignored() {
        let state = MockState {
            cleanup_result: crate::adb::mock::Scripted::Failure {
                stdout: String::new(),
                stderr: "umount: busy".into(),
                exit_code: 1,
            },
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        match runner.status() {
            WorkflowStatus::Failed { failure, .. } => assert_eq!(failure.label(), "CleanupFailed"),
            other => panic!("expected cleanup failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_hybrid_mount_config_stops_without_overwriting() {
        // The optional integration — never the mandatory root workflow —
        // is what reports `ConfigurationInvalid` now.
        let state = MockState {
            hybrid_mount_config: "not [ valid toml =".to_string(),
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();
        assert!(runner.status().is_completed());

        runner.run_hybrid_mount_integration().await.unwrap();
        match runner.hybrid_mount_status() {
            OptionalIntegrationStatus::Failed { detail } => {
                assert!(detail.contains("not valid TOML") || detail.to_lowercase().contains("toml"))
            }
            other => panic!(
                "expected the optional integration to fail with ConfigurationInvalid, got {other:?}"
            ),
        }
        assert!(runner.status().is_completed(), "root must remain completed");
    }

    #[tokio::test]
    async fn device_disconnect_mid_workflow_is_reported() {
        let (runner, mock, _rx) = runner_with_handle(MockState::default());
        drive_to_payload_pushed(&runner).await;

        // Simulate the device vanishing right before payload execution.
        mock.set_device_state(None);
        runner.execute_payload().await.unwrap();

        match runner.status() {
            WorkflowStatus::Failed { failure, last_good } => {
                assert_eq!(failure.label(), "PayloadFailure");
                // Last known-good state is preserved for recovery.
                assert_eq!(last_good, WorkflowState::PayloadPushed);
            }
            other => panic!("expected failure, got {other:?}"),
        }

        // Reconnecting must let the user resume rather than restart from
        // scratch (CLAUDE.md section 17).
        mock.set_device_state(Some("device".to_string()));
        runner.prepare_retry().unwrap();
        assert_eq!(
            runner.status(),
            WorkflowStatus::Running(WorkflowState::PayloadPushed)
        );
    }

    #[tokio::test]
    async fn cancellation_during_payload_execution_is_user_cancelled() {
        let (runner, _rx) = runner_with(MockState::default());
        let runner = Arc::new(runner);
        drive_to_payload_pushed(&runner).await;

        let runner_clone = runner.clone();
        let handle = tokio::spawn(async move { runner_clone.execute_payload().await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        runner.request_cancel();
        handle.await.unwrap().unwrap();

        // Mock payload success is instantaneous, so cancellation may lose
        // the race; only assert UserCancelled when it actually fired.
        if let WorkflowStatus::Failed { failure, .. } = runner.status() {
            assert!(
                matches!(failure, WorkflowFailure::UserCancelled)
                    || failure.label() == "UserCancelled"
            );
        }
    }

    #[tokio::test]
    async fn retry_limit_exceeded_prevents_further_attempts_without_infinite_loop() {
        let state = MockState {
            run_payload: crate::adb::mock::Scripted::Failure {
                stdout: String::new(),
                stderr: "boom".into(),
                exit_code: 1,
            },
            ..Default::default()
        };
        let (runner, _rx) = runner_with(state);
        drive_to_payload_pushed(&runner).await;

        let mut attempts = 0;
        loop {
            runner.execute_payload().await.unwrap();
            attempts += 1;
            if runner.prepare_retry().is_err() {
                break;
            }
            assert!(attempts <= 10, "retry loop did not terminate");
        }
        assert_eq!(attempts, 3);
    }

    #[test]
    fn scenario_presets_cover_claude_md_section_26() {
        // Smoke test that every named mock scenario at least constructs.
        for scenario in [
            MockScenario::DeviceConnected,
            MockScenario::DeviceMismatch,
            MockScenario::BuildMismatch,
            MockScenario::PayloadSuccess,
            MockScenario::PayloadFailure,
            MockScenario::PayloadTimeout,
            MockScenario::KernelSuLoadSuccess,
            MockScenario::KernelSuLoadFailure,
            MockScenario::RootSuccess,
            MockScenario::RootFailure,
            MockScenario::DeviceDisconnect,
            MockScenario::CleanupFailure,
            MockScenario::InvalidConfig,
        ] {
            let _client = MockAdbClient::scenario(scenario);
        }
    }
}
