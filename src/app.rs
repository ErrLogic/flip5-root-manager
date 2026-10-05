//! Application state and update logic. Owns the [`WorkflowRunner`], the
//! terminal log buffer, artifact discovery, and the one currently-running
//! step task (if any). The `tui` module only reads this state to render;
//! all mutation happens here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::adb::{AdbClient, DryRunAdbClient, DryRunScenario};
use crate::artifacts::{ArtifactSlot, GitInfo, PayloadRepository};
use crate::device;
use crate::terminal::{LogBuffer, ScrollState};
use crate::tui::events::AppAction;
use crate::workflow::state::TOTAL_REAL_STEPS;
use crate::workflow::{
    OptionalIntegrationStatus, RunnerError, RunnerEvent, WorkflowRunner, WorkflowState,
    WorkflowStatus,
};

/// Which `AdbClient` backend the workflow runner is currently wired to.
/// Both variants drive the exact same [`WorkflowRunner`] state machine —
/// only the backend behind it differs (see [`crate::adb::dryrun`]).
/// Defaults to `Real`, matching the app's existing behavior; switching to
/// `DryRun` is always an explicit user action (the `D` key), and it never
/// switches back on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    #[default]
    Real,
    DryRun,
}

impl RunMode {
    pub fn label(&self) -> &'static str {
        match self {
            RunMode::Real => "REAL RUN",
            RunMode::DryRun => "DRY RUN",
        }
    }
}

/// A read-only probe of ADB/device connectivity shown in the header,
/// independent of (and prior to) the formal workflow state machine
/// (CLAUDE.md section 30: starting the TUI must never itself begin the
/// workflow). Refreshed continuously in the background (hotplug) so the
/// header reflects connect/disconnect without restarting the app, while
/// the workflow itself only ever advances when the user presses Enter.
#[derive(Debug, Clone)]
pub enum DeviceProbe {
    NotChecked,
    NoDevice,
    MultipleDevices(Vec<String>),
    Connected {
        serial: String,
        model: String,
        build: String,
        compatible: bool,
        detail: Option<String>,
    },
    AdbError(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Workflow,
    Log,
}

pub struct App {
    /// Always the real backend, regardless of `mode` — used for the
    /// background hotplug header probe (which stays real in both modes;
    /// it's purely informational, read-only, and outside "the workflow"
    /// per CLAUDE.md section 30) and reused verbatim for `RunMode::Real`.
    real_adb: Arc<dyn AdbClient>,
    staging_dir: PathBuf,
    dryrun_scenario: DryRunScenario,
    mode: RunMode,
    pub runner: Arc<WorkflowRunner>,
    runner_events: mpsc::UnboundedReceiver<RunnerEvent>,
    pub payload_repo: PayloadRepository,
    /// Artifact lookups are cached once at startup (CLAUDE.md section 20:
    /// checksum calculation must be read-only; it should also not be
    /// redone on every render frame).
    pub payload_lib: ArtifactSlot,
    pub payload_runner_artifact: ArtifactSlot,
    pub kernelsu_artifact: ArtifactSlot,
    pub git_info: Option<GitInfo>,
    pub log: LogBuffer,
    /// Independent bounded scroll state for the Workflow panel's step
    /// list (separate from `log`'s own scroll state — scrolling one must
    /// never affect the other).
    pub workflow_scroll: ScrollState,
    /// Rendered viewport heights, recorded by `tui::draw` each frame so
    /// keyboard handling (which runs outside of rendering) always pages
    /// by the *actual* current panel size rather than a guessed/fixed
    /// constant. Default to a small positive number before the first
    /// render so an early keypress can't divide by/page by zero.
    pub terminal_viewport_height: usize,
    pub workflow_viewport_height: usize,
    pub device_probe: DeviceProbe,
    pub focus: Focus,
    pub last_error: Option<String>,
    pub should_quit: bool,
    active_task: Option<JoinHandle<Result<(), RunnerError>>>,
    pub active_step: Option<(WorkflowState, Instant)>,
}

fn build_runner(
    adb: Arc<dyn AdbClient>,
    staging_dir: PathBuf,
) -> (Arc<WorkflowRunner>, mpsc::UnboundedReceiver<RunnerEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (Arc::new(WorkflowRunner::new(adb, staging_dir, tx)), rx)
}

impl App {
    pub fn new(
        adb: Arc<dyn AdbClient>,
        payload_repo: PayloadRepository,
        staging_dir: PathBuf,
        dryrun_scenario: DryRunScenario,
    ) -> Self {
        let (runner, runner_events) = build_runner(adb.clone(), staging_dir.clone());
        let payload_lib = payload_repo.payload_library();
        let payload_runner_artifact = payload_repo.payload_runner();
        let kernelsu_artifact = payload_repo.kernelsu_artifact();
        let git_info = payload_repo.git_info();
        Self {
            real_adb: adb,
            staging_dir,
            dryrun_scenario,
            mode: RunMode::Real,
            runner,
            runner_events,
            payload_repo,
            payload_lib,
            payload_runner_artifact,
            kernelsu_artifact,
            git_info,
            log: LogBuffer::new(),
            workflow_scroll: ScrollState::default(),
            terminal_viewport_height: 10,
            workflow_viewport_height: 10,
            device_probe: DeviceProbe::NotChecked,
            focus: Focus::Workflow,
            last_error: None,
            should_quit: false,
            active_task: None,
            active_step: None,
        }
    }

    pub fn mode(&self) -> RunMode {
        self.mode
    }

    /// The workflow checklist's "effective" progress index: the real
    /// current/last-good `WorkflowState` index, except before the
    /// workflow has actually started (`Running(Disconnected)`), when it
    /// reflects the background hotplug probe's findings instead — so
    /// "Device detection"/"Device verification" don't look outstanding
    /// once we already know the answer. Display-only; never affects the
    /// real `WorkflowRunner` state. Shared by the checklist renderer and
    /// by `ensure_active_step_visible` so both agree on which row
    /// matters right now.
    pub fn effective_progress(&self) -> usize {
        let status = self.runner.status();
        let current_idx = match &status {
            WorkflowStatus::Running(s) => s.index(),
            WorkflowStatus::Failed { last_good, .. } => last_good.index(),
        };
        if !matches!(status, WorkflowStatus::Running(WorkflowState::Disconnected)) {
            return current_idx;
        }
        match &self.device_probe {
            DeviceProbe::Connected {
                compatible: true, ..
            } => WorkflowState::DeviceVerified.index(),
            DeviceProbe::Connected {
                compatible: false, ..
            } => WorkflowState::DeviceDetected.index(),
            _ => current_idx,
        }
    }

    /// Scrolls the Workflow panel's step list by the minimal amount
    /// needed to bring the currently active/next step into view, without
    /// otherwise disturbing a manual scroll (CLAUDE.md-refinement
    /// requirement: bring the active step into view when the workflow
    /// advances, but don't force the list to follow it beyond that).
    /// Call this whenever the workflow actually progresses — a new step
    /// starts, or one finishes/fails.
    pub fn ensure_active_step_visible(&mut self) {
        let display_idx = self.effective_progress();
        let focus_row = display_idx.min(TOTAL_REAL_STEPS.saturating_sub(1));
        self.workflow_scroll.ensure_visible(
            focus_row,
            TOTAL_REAL_STEPS,
            self.workflow_viewport_height.max(1),
        );
    }

    /// Switches between Real Run and Dry Run (the `D` key). Only allowed
    /// before the workflow has started and while nothing is running —
    /// CLAUDE.md-refinement requirement: mode is selected *before*
    /// starting, never auto-switched, and switching mid-run would orphan
    /// whatever step is currently in flight on the old runner.
    pub fn toggle_mode(&mut self) {
        if self.is_step_running()
            || !matches!(
                self.runner.status(),
                WorkflowStatus::Running(WorkflowState::Disconnected)
            )
        {
            self.last_error =
                Some("Mode can only be changed before the workflow starts.".to_string());
            return;
        }
        self.mode = match self.mode {
            RunMode::Real => RunMode::DryRun,
            RunMode::DryRun => RunMode::Real,
        };
        let adb: Arc<dyn AdbClient> = match self.mode {
            RunMode::Real => self.real_adb.clone(),
            RunMode::DryRun => Arc::new(DryRunAdbClient::new(self.dryrun_scenario)),
        };
        let (runner, runner_events) = build_runner(adb, self.staging_dir.clone());
        self.runner = runner;
        self.runner_events = runner_events;
        self.last_error = None;
        self.log
            .push_warning(format!("Mode switched to {}.", self.mode.label()));
    }

    /// Prefixes narration with `[DRY RUN]` when in Dry Run mode, so the
    /// terminal output panel clearly identifies simulated operations —
    /// a presentation-layer concern, deliberately kept out of
    /// `WorkflowRunner`/`AdbClient` so neither has to know which mode is
    /// active (CLAUDE.md-refinement: don't duplicate workflow logic).
    fn tag(&self, text: String) -> String {
        match self.mode {
            RunMode::DryRun => format!("[DRY RUN] {text}"),
            RunMode::Real => text,
        }
    }

    /// Startup probing (CLAUDE.md section 30): checks ADB/device
    /// connectivity and compatibility for display, without advancing the
    /// workflow state machine at all. Always logs the initial finding.
    pub async fn probe_device_at_startup(&mut self) {
        self.log
            .push_info("Probing for connected device (read-only; workflow has not started)...");
        self.probe_device_now().await;
        self.log_device_probe_state();
    }

    /// Re-probes device connectivity for the header display (hotplug:
    /// CLAUDE.md section 17/18 — detect reconnection without restarting
    /// the app). Only logs when the connectivity state actually changed,
    /// so this can be polled frequently without spamming the terminal
    /// output panel. Never touches the formal workflow state machine —
    /// the workflow still only advances when the user presses Enter.
    pub async fn refresh_device_probe(&mut self) {
        let previous = probe_signature(&self.device_probe);
        self.probe_device_now().await;
        if probe_signature(&self.device_probe) != previous {
            self.log_device_probe_state();
        }
    }

    async fn probe_device_now(&mut self) {
        let adb = self.real_adb.clone();
        self.device_probe = match device::detect_single_device(adb.as_ref()).await {
            Ok(entry) => match device::verify_device(adb.as_ref(), &entry.serial).await {
                Ok(v) => DeviceProbe::Connected {
                    serial: v.serial,
                    model: v.model,
                    build: v.build,
                    compatible: true,
                    detail: None,
                },
                Err(e) => DeviceProbe::Connected {
                    serial: entry.serial,
                    model: String::new(),
                    build: String::new(),
                    compatible: false,
                    detail: Some(e.to_string()),
                },
            },
            Err(device::detector::DetectError::NoDevice) => DeviceProbe::NoDevice,
            Err(device::detector::DetectError::MultipleDevices(serials)) => {
                DeviceProbe::MultipleDevices(serials)
            }
            Err(e) => DeviceProbe::AdbError(e.to_string()),
        };
    }

    fn log_device_probe_state(&mut self) {
        match &self.device_probe {
            DeviceProbe::NotChecked => {}
            DeviceProbe::NoDevice => self.log.push_warning("No device connected."),
            DeviceProbe::MultipleDevices(serials) => self.log.push_warning(format!(
                "Multiple devices connected: {}. Connect only the target device.",
                serials.join(", ")
            )),
            DeviceProbe::AdbError(e) => self
                .log
                .push_error(format!("ADB error while probing device: {e}")),
            DeviceProbe::Connected {
                serial,
                compatible: true,
                ..
            } => self.log.push_success(format!(
                "Device connected: {serial} (compatible). Press Enter to begin the workflow."
            )),
            DeviceProbe::Connected {
                serial,
                compatible: false,
                detail,
                ..
            } => self.log.push_error(format!(
                "Device connected but not compatible: {serial} ({})",
                detail.clone().unwrap_or_default()
            )),
        }
    }

    pub fn handle_action(&mut self, action: AppAction) {
        match action {
            AppAction::Enter => self.on_enter(),
            AppAction::ToggleMode => self.toggle_mode(),
            AppAction::Retry => self.on_retry(),
            AppAction::Stop => {
                self.runner.request_cancel();
                self.log.push_warning("Stop requested.");
            }
            AppAction::ToggleFocus => {
                self.focus = match self.focus {
                    Focus::Workflow => Focus::Log,
                    Focus::Log => Focus::Workflow,
                };
            }
            // Terminal Output and the Workflow step list have fully
            // independent scroll state; PageUp/PageDown/Home/End apply
            // to whichever panel currently has focus, and never touch
            // the other one.
            AppAction::ScrollUp => match self.focus {
                Focus::Log => self.log.page_up(self.terminal_viewport_height.max(1)),
                Focus::Workflow => self
                    .workflow_scroll
                    .page_up(TOTAL_REAL_STEPS, self.workflow_viewport_height.max(1)),
            },
            AppAction::ScrollDown => match self.focus {
                Focus::Log => self.log.page_down(self.terminal_viewport_height.max(1)),
                Focus::Workflow => self
                    .workflow_scroll
                    .page_down(TOTAL_REAL_STEPS, self.workflow_viewport_height.max(1)),
            },
            AppAction::ScrollToTop => match self.focus {
                Focus::Log => self.log.jump_to_top(),
                Focus::Workflow => self.workflow_scroll.jump_to_top(),
            },
            AppAction::ScrollToEnd => match self.focus {
                Focus::Log => self.log.jump_to_bottom(),
                Focus::Workflow => self
                    .workflow_scroll
                    .jump_to_bottom(TOTAL_REAL_STEPS, self.workflow_viewport_height.max(1)),
            },
            AppAction::ClearLog => self.log.clear(),
            AppAction::SkipOptionalIntegration => self.on_skip_optional_integration(),
            AppAction::Quit => self.should_quit = true,
            AppAction::None => {}
        }
    }

    fn on_enter(&mut self) {
        if self.active_task.is_some() {
            self.last_error =
                Some("A step is already running. Press S to stop it first.".to_string());
            return;
        }

        // The optional, post-root hybrid_mount/ViPER4Android-RE
        // integration takes priority over the generic dispatcher once
        // it's been offered — the mandatory workflow has nothing left
        // for Enter to do at that point anyway. It never runs on its
        // own; this is the one explicit user choice that starts it.
        if self.runner.hybrid_mount_status() == OptionalIntegrationStatus::AwaitingChoice {
            self.spawn_optional_integration();
            return;
        }

        match self.runner.status() {
            WorkflowStatus::Failed { .. } => {
                self.last_error =
                    Some("Workflow has failed. Press R to retry, or Q to quit.".to_string());
            }
            WorkflowStatus::Running(WorkflowState::Completed) => {
                self.log.push_info("Workflow already completed.");
            }
            WorkflowStatus::Running(_) => self.dispatch_current_step(),
        }
    }

    /// Runs whatever the current `WorkflowState` calls for next: either
    /// one of the two synchronous artifact-selection steps, or a spawned
    /// async step via [`App::spawn_step`]. Shared by the explicit `Enter`
    /// handler and by [`App::auto_continue`] so both ways of advancing
    /// the workflow dispatch identically. Callers are responsible for
    /// having already ruled out "no step running"/"not failed"/"not
    /// awaiting the optional integration choice" — this only matches on
    /// the current `WorkflowState` itself.
    fn dispatch_current_step(&mut self) {
        match self.runner.status() {
            WorkflowStatus::Running(WorkflowState::DeviceVerified) => {
                let lib = self.payload_lib.clone();
                let runner_bin = self.payload_runner_artifact.clone();
                if let Err(e) = self.runner.select_payload_artifacts(lib, runner_bin) {
                    self.last_error = Some(e.to_string());
                } else {
                    self.last_error = None;
                    // This selection advances the workflow synchronously
                    // (no spawned task), so bring the new active step
                    // into view here too.
                    self.ensure_active_step_visible();
                }
            }
            WorkflowStatus::Running(WorkflowState::PayloadSucceeded) => {
                let ksud = self.kernelsu_artifact.clone();
                if let Err(e) = self.runner.select_kernelsu_artifact(ksud) {
                    self.last_error = Some(e.to_string());
                } else {
                    self.last_error = None;
                    self.ensure_active_step_visible();
                }
            }
            WorkflowStatus::Running(WorkflowState::Completed) => {}
            WorkflowStatus::Running(state) => self.spawn_step(state),
            WorkflowStatus::Failed { .. } => {}
        }
    }

    /// Advances the workflow by itself once a step has finished
    /// successfully, so the user isn't required to press Enter after
    /// every single mandatory step — only once, to start it. Called once
    /// per main-loop tick; a no-op unless there is genuinely a next step
    /// ready to run unattended.
    ///
    /// Deliberately never fires:
    /// - at `Running(Disconnected)` — the very first step must stay an
    ///   explicit user action (CLAUDE.md sections 30/31: launching the
    ///   TUI, and reconnecting after it, must never themselves start the
    ///   workflow);
    /// - while a step is already running, or the previous one left an
    ///   error for the user to see;
    /// - once the workflow has failed (retry stays the explicit `R`
    ///   action — CLAUDE.md section 14: retries must be explicit) or has
    ///   completed;
    /// - while the optional, post-root hybrid_mount/ViPER4Android-RE
    ///   integration is awaiting the user's explicit choice (`Enter`/`K`)
    ///   — it must never run, or be skipped, on its own.
    pub fn auto_continue(&mut self) {
        if self.active_task.is_some() || self.last_error.is_some() {
            return;
        }
        if self.runner.hybrid_mount_status() == OptionalIntegrationStatus::AwaitingChoice {
            return;
        }
        match self.runner.status() {
            WorkflowStatus::Running(WorkflowState::Disconnected)
            | WorkflowStatus::Running(WorkflowState::Completed)
            | WorkflowStatus::Failed { .. } => {}
            WorkflowStatus::Running(_) => self.dispatch_current_step(),
        }
    }

    fn on_retry(&mut self) {
        match self.runner.prepare_retry() {
            Ok(()) => {
                self.last_error = None;
                self.log.push_info("Retrying...");
                self.ensure_active_step_visible();
            }
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    fn spawn_step(&mut self, state: WorkflowState) {
        self.last_error = None;
        // `state` is the *precondition* the dispatcher matched on (the
        // workflow's current state); what's actually being attempted —
        // and what the compact status line and checklist "▶" should
        // name — is the step that follows it. Every step only advances
        // the formal state on success, so this is always the correct
        // "currently executing" label even for steps like payload
        // execution that pass through an intermediate state on the way.
        let active = state.normal_next().unwrap_or(state);
        self.active_step = Some((active, Instant::now()));
        self.ensure_active_step_visible();
        let runner = self.runner.clone();
        self.active_task = Some(tokio::spawn(async move { runner.run_next_step().await }));
    }

    /// Starts the optional, post-root `hybrid_mount`/ViPER4Android-RE
    /// integration. Only ever called in response to the user's explicit
    /// choice (Enter while it's awaiting a choice) — never automatically.
    fn spawn_optional_integration(&mut self) {
        self.last_error = None;
        let runner = self.runner.clone();
        self.active_task = Some(tokio::spawn(async move {
            runner.run_hybrid_mount_integration().await
        }));
    }

    /// The user explicitly declines the optional integration (`K`). A
    /// successful outcome, not a failure — the root workflow already
    /// succeeded regardless.
    fn on_skip_optional_integration(&mut self) {
        match self.runner.skip_hybrid_mount_integration() {
            Ok(()) => self.last_error = None,
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    /// Checks whether the currently spawned step task has finished and, if
    /// so, reaps it and surfaces any error. Call once per event loop tick.
    pub async fn poll_active_task(&mut self) {
        let finished = matches!(&self.active_task, Some(h) if h.is_finished());
        if !finished {
            return;
        }
        if let Some(handle) = self.active_task.take() {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => self.last_error = Some(e.to_string()),
                Err(join_err) => self.last_error = Some(format!("internal task error: {join_err}")),
            }
        }
        self.active_step = None;
    }

    pub fn drain_runner_events(&mut self) {
        while let Ok(event) = self.runner_events.try_recv() {
            self.apply_runner_event(event);
        }
    }

    pub async fn recv_runner_event(&mut self) -> Option<RunnerEvent> {
        self.runner_events.recv().await
    }

    pub fn apply_runner_event(&mut self, event: RunnerEvent) {
        match event {
            RunnerEvent::CommandStarted { kind, display } => {
                let display = self.tag(display);
                self.log.push_command_start(kind, &display);
            }
            RunnerEvent::Stdout(l) => self.log.push_stdout(l),
            RunnerEvent::Stderr(l) => self.log.push_stderr(l),
            RunnerEvent::CommandFinished { exit_code, elapsed } => {
                self.log.push_exit(exit_code, elapsed)
            }
            RunnerEvent::Info(msg) => {
                let msg = self.tag(msg);
                self.log.push_info(msg);
            }
            RunnerEvent::Success(msg) => {
                let msg = self.tag(msg);
                self.log.push_success(msg);
            }
            RunnerEvent::Warning(msg) => {
                let msg = self.tag(msg);
                self.log.push_warning(msg);
            }
            RunnerEvent::Error(msg) => {
                let msg = self.tag(msg);
                self.log.push_error(msg);
            }
            RunnerEvent::StateChanged(_) => self.ensure_active_step_visible(),
        }
    }

    pub fn is_step_running(&self) -> bool {
        self.active_task.is_some()
    }
}

/// A cheap, comparable fingerprint of a [`DeviceProbe`], used only to
/// decide whether connectivity actually changed since the last poll (so
/// `refresh_device_probe` doesn't log on every tick).
fn probe_signature(probe: &DeviceProbe) -> String {
    match probe {
        DeviceProbe::NotChecked => "not_checked".to_string(),
        DeviceProbe::NoDevice => "no_device".to_string(),
        DeviceProbe::MultipleDevices(serials) => format!("multiple:{}", serials.join(",")),
        DeviceProbe::AdbError(e) => format!("adb_error:{e}"),
        DeviceProbe::Connected {
            serial, compatible, ..
        } => format!("connected:{serial}:{compatible}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::mock::{MockAdbClient, MockState};
    use crate::artifacts::PayloadRepository;

    fn test_app_with_handle(state: MockState) -> (App, Arc<MockAdbClient>) {
        let mock = Arc::new(MockAdbClient::new(state));
        let adb: Arc<dyn AdbClient> = mock.clone();
        let staging = std::env::temp_dir().join(format!(
            "flip5-app-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let app = App::new(
            adb,
            PayloadRepository::new("/tmp/flip5-app-test-nonexistent-repo"),
            staging,
            DryRunScenario::AllSuccess,
        );
        (app, mock)
    }

    #[tokio::test]
    async fn hotplug_refresh_detects_connect_and_disconnect_without_restarting_app() {
        let (mut app, mock) = test_app_with_handle(MockState {
            device_state: None,
            ..Default::default()
        });

        app.probe_device_at_startup().await;
        assert!(matches!(app.device_probe, DeviceProbe::NoDevice));
        // Workflow must still be untouched by a mere probe.
        assert_eq!(
            app.runner.status(),
            WorkflowStatus::Running(WorkflowState::Disconnected)
        );

        // Simulate the user plugging the device in: the SAME app instance
        // (no reload) should pick this up on the next refresh.
        mock.set_device_state(Some("device".to_string()));
        app.refresh_device_probe().await;
        match &app.device_probe {
            DeviceProbe::Connected {
                compatible: true, ..
            } => {}
            other => panic!("expected connected+compatible, got {other:?}"),
        }
        // Still never auto-started: only Enter may advance the workflow.
        assert_eq!(
            app.runner.status(),
            WorkflowStatus::Running(WorkflowState::Disconnected)
        );

        // Unplug again.
        mock.set_device_state(None);
        app.refresh_device_probe().await;
        assert!(matches!(app.device_probe, DeviceProbe::NoDevice));
        assert_eq!(
            app.runner.status(),
            WorkflowStatus::Running(WorkflowState::Disconnected)
        );
    }

    #[tokio::test]
    async fn refresh_without_a_change_does_not_duplicate_log_lines() {
        let (mut app, _mock) = test_app_with_handle(MockState {
            device_state: None,
            ..Default::default()
        });
        app.probe_device_at_startup().await;
        let len_after_startup = app.log.len();
        app.refresh_device_probe().await;
        app.refresh_device_probe().await;
        app.refresh_device_probe().await;
        assert_eq!(
            app.log.len(),
            len_after_startup,
            "no new log lines when connectivity is unchanged"
        );
    }

    #[tokio::test]
    async fn mode_switch_before_start_rebuilds_a_fresh_disconnected_runner() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        assert_eq!(app.mode(), RunMode::Real);

        app.toggle_mode();
        assert_eq!(app.mode(), RunMode::DryRun);
        assert_eq!(
            app.runner.status(),
            WorkflowStatus::Running(WorkflowState::Disconnected)
        );
        assert!(app.last_error.is_none());

        app.toggle_mode();
        assert_eq!(app.mode(), RunMode::Real);
    }

    #[tokio::test]
    async fn mode_cannot_change_once_the_workflow_has_started() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        app.runner.detect_device().await.unwrap();
        assert_eq!(
            app.runner.status(),
            WorkflowStatus::Running(WorkflowState::DeviceDetected)
        );

        app.toggle_mode();

        assert_eq!(
            app.mode(),
            RunMode::Real,
            "mode must not change once the workflow has started"
        );
        assert!(app.last_error.is_some());
    }

    #[tokio::test]
    async fn mode_cannot_change_while_a_step_is_running() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        // Simulate a step being mid-flight without depending on the real
        // spawn_step internals: active_step alone is enough to observe,
        // but is_step_running() is driven by active_task, so drive a real
        // one instead for a faithful test.
        app.handle_action(crate::tui::events::AppAction::Enter);
        assert!(app.is_step_running());

        app.toggle_mode();
        assert_eq!(app.mode(), RunMode::Real);
        assert!(app.last_error.is_some());

        app.poll_active_task().await;
    }

    #[tokio::test]
    async fn dry_run_mode_tags_log_lines() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        app.toggle_mode();
        assert_eq!(app.mode(), RunMode::DryRun);

        // Drive a real step on the (now dry-run-backed) runner directly,
        // then pull its events into the log exactly as the main loop
        // would via `drain_runner_events`.
        app.runner.detect_device().await.unwrap();
        app.drain_runner_events();

        assert!(
            app.log.all().iter().any(|l| l.text.contains("[DRY RUN]")),
            "dry run log lines must be clearly marked"
        );
    }

    #[tokio::test]
    async fn real_mode_never_tags_log_lines() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        assert_eq!(app.mode(), RunMode::Real);
        app.runner.detect_device().await.unwrap();
        app.drain_runner_events();
        assert!(!app.log.all().iter().any(|l| l.text.contains("[DRY RUN]")));
    }

    #[tokio::test]
    async fn active_step_label_names_the_step_actually_running_not_the_precondition() {
        use crate::artifacts::{ArtifactInfo, ArtifactKind, ArtifactSlot};
        use std::path::PathBuf;

        fn fake_artifact(kind: ArtifactKind) -> ArtifactInfo {
            ArtifactInfo {
                kind,
                name: kind.display_name(),
                relative_path: kind.relative_path(),
                absolute_path: PathBuf::from("/dev/null"),
                size_bytes: 1,
                sha256: "0".repeat(10),
                repository_location: PathBuf::from("/dev/null"),
            }
        }

        let (mut app, _mock) = test_app_with_handle(MockState::default());
        app.runner.detect_device().await.unwrap();
        app.runner.verify_device().await.unwrap();
        app.runner
            .select_payload_artifacts(
                ArtifactSlot::Found(fake_artifact(ArtifactKind::PayloadLibrary)),
                ArtifactSlot::Found(fake_artifact(ArtifactKind::PayloadRunner)),
            )
            .unwrap();
        assert_eq!(
            app.runner.status(),
            WorkflowStatus::Running(WorkflowState::PayloadSelected)
        );

        // Enter now spawns `push_payload`, whose precondition is
        // `PayloadSelected` — the compact status line/checklist must name
        // the step actually running (`PayloadPushed`), not that
        // precondition.
        app.handle_action(crate::tui::events::AppAction::Enter);
        assert_eq!(
            app.active_step.map(|(s, _)| s),
            Some(WorkflowState::PayloadPushed)
        );

        app.poll_active_task().await;
    }

    #[tokio::test]
    async fn terminal_and_workflow_panels_scroll_independently() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        app.terminal_viewport_height = 5;
        app.workflow_viewport_height = 5;
        for i in 0..50 {
            app.log.push_info(format!("line {i}"));
        }

        // Scroll only the terminal (it's focused by default via Tab's
        // starting state being Workflow — switch explicitly to be sure).
        app.focus = Focus::Log;
        app.handle_action(crate::tui::events::AppAction::ScrollUp);
        assert!(
            !app.log.is_at_bottom(app.terminal_viewport_height),
            "terminal should have scrolled"
        );
        assert_eq!(
            app.workflow_scroll.offset, 0,
            "scrolling the terminal must not move the workflow list"
        );
        let terminal_top_before = app.log.visible_window(app.terminal_viewport_height)[0]
            .text
            .clone();

        // Now scroll only the workflow list.
        app.focus = Focus::Workflow;
        app.handle_action(crate::tui::events::AppAction::ScrollDown);
        assert!(
            app.workflow_scroll.offset > 0,
            "workflow list should have scrolled"
        );

        // The terminal's position must be untouched by that.
        let terminal_top_after = app.log.visible_window(app.terminal_viewport_height)[0]
            .text
            .clone();
        assert_eq!(
            terminal_top_before, terminal_top_after,
            "terminal scroll position must be unaffected by workflow scrolling"
        );
    }

    #[tokio::test]
    async fn workflow_page_down_and_up_are_bounded() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        app.focus = Focus::Workflow;
        app.workflow_viewport_height = 5; // 13 steps, viewport 5 -> max_scroll = 8

        for _ in 0..10 {
            app.handle_action(crate::tui::events::AppAction::ScrollDown);
        }
        assert_eq!(
            app.workflow_scroll.offset, 8,
            "must clamp at max_scroll, never scroll past the last step"
        );

        for _ in 0..10 {
            app.handle_action(crate::tui::events::AppAction::ScrollUp);
        }
        assert_eq!(
            app.workflow_scroll.offset, 0,
            "must clamp at zero, never scroll above the first step"
        );
    }

    #[tokio::test]
    async fn active_step_is_brought_into_view_when_the_workflow_advances() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        // A small viewport and a scroll position that does NOT include
        // the step about to become active (index 0, "Device detection").
        app.workflow_viewport_height = 3;
        app.workflow_scroll.offset = 10;

        app.runner.detect_device().await.unwrap();
        app.drain_runner_events();

        // After `detect_device` succeeds, the workflow is at
        // `DeviceDetected` (list row index 1, 0-based) — that row must
        // now be back in view.
        assert_eq!(
            app.workflow_scroll.offset, 1,
            "the newly reached step must be scrolled back into view"
        );
    }

    #[tokio::test]
    async fn clear_log_resets_terminal_scroll_without_touching_workflow_scroll() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        app.workflow_viewport_height = 5;
        app.workflow_scroll.offset = 3;
        for i in 0..20 {
            app.log.push_info(format!("line {i}"));
        }
        app.focus = Focus::Log;
        app.handle_action(crate::tui::events::AppAction::ScrollUp);
        assert!(!app.log.is_at_bottom(5));

        app.handle_action(crate::tui::events::AppAction::ClearLog);

        assert!(app.log.is_empty());
        assert!(app.log.is_at_bottom(5));
        assert_eq!(
            app.workflow_scroll.offset, 3,
            "clearing the log must not touch workflow scroll state"
        );
    }

    /// Drives the spawned-task/event-drain cycle exactly as the real main
    /// loop does, until the currently running step (if any) settles and
    /// no more progress is being made.
    async fn pump_until_idle(app: &mut App) {
        loop {
            // Let a just-spawned task actually get polled.
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            app.poll_active_task().await;
            app.drain_runner_events();
            app.auto_continue();
            if app.active_task.is_none() {
                // One more drain in case `auto_continue` just spawned a
                // step that finishes essentially instantly (the mock
                // backend is synchronous), then check again.
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                app.poll_active_task().await;
                app.drain_runner_events();
                if app.active_task.is_none() {
                    let status_before = app.runner.status();
                    app.auto_continue();
                    if app.runner.status() == status_before && app.active_task.is_none() {
                        break;
                    }
                }
            }
        }
    }

    /// Installs fake "found" artifacts onto `app`, exactly like a real
    /// payload repository would via `PayloadRepository`, so a driven
    /// workflow can get past the (synchronous, but still real)
    /// artifact-selection steps instead of failing on `ArtifactMissing`.
    fn install_fake_artifacts(app: &mut App) {
        use crate::artifacts::{ArtifactInfo, ArtifactKind, ArtifactSlot};
        use std::path::PathBuf;

        fn fake_artifact(kind: ArtifactKind) -> ArtifactInfo {
            ArtifactInfo {
                kind,
                name: kind.display_name(),
                relative_path: kind.relative_path(),
                absolute_path: PathBuf::from("/dev/null"),
                size_bytes: 1,
                sha256: "0".repeat(10),
                repository_location: PathBuf::from("/dev/null"),
            }
        }
        app.payload_lib = ArtifactSlot::Found(fake_artifact(ArtifactKind::PayloadLibrary));
        app.payload_runner_artifact =
            ArtifactSlot::Found(fake_artifact(ArtifactKind::PayloadRunner));
        app.kernelsu_artifact = ArtifactSlot::Found(fake_artifact(ArtifactKind::KernelSu));
    }

    #[tokio::test]
    async fn auto_continue_runs_the_whole_workflow_after_a_single_enter() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        install_fake_artifacts(&mut app);

        // Only the device probe + a single Enter are explicit user
        // actions; `auto_continue` (as the main loop would call it every
        // tick) must carry the rest of the mandatory chain all the way
        // to completion without any further Enter presses.
        app.probe_device_at_startup().await;
        app.handle_action(crate::tui::events::AppAction::Enter);

        for _ in 0..200 {
            pump_until_idle(&mut app).await;
            if app.runner.status().is_completed() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        assert!(
            app.runner.status().is_completed(),
            "expected the workflow to reach Completed on its own, got {:?} (last_error: {:?})",
            app.runner.status(),
            app.last_error
        );
        // The optional integration must still be left for an explicit
        // choice, never run automatically.
        assert_eq!(
            app.runner.hybrid_mount_status(),
            OptionalIntegrationStatus::AwaitingChoice
        );
    }

    #[tokio::test]
    async fn auto_continue_never_starts_the_workflow_on_its_own() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        app.probe_device_at_startup().await;

        // No Enter pressed yet: repeated auto_continue calls (as the main
        // loop would make every tick before any key is pressed) must
        // never advance past `Disconnected` on their own.
        for _ in 0..10 {
            app.auto_continue();
        }
        assert_eq!(
            app.runner.status(),
            WorkflowStatus::Running(WorkflowState::Disconnected)
        );
    }

    #[tokio::test]
    async fn auto_continue_never_runs_the_optional_integration_unattended() {
        let (mut app, _mock) = test_app_with_handle(MockState::default());
        install_fake_artifacts(&mut app);
        app.probe_device_at_startup().await;
        app.handle_action(crate::tui::events::AppAction::Enter);

        for _ in 0..200 {
            pump_until_idle(&mut app).await;
            if app.runner.status().is_completed() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(app.runner.status().is_completed());

        // Completed, and now repeatedly ticking auto_continue must never
        // start/skip the optional hybrid_mount integration by itself.
        for _ in 0..10 {
            app.auto_continue();
        }
        assert_eq!(
            app.runner.hybrid_mount_status(),
            OptionalIntegrationStatus::AwaitingChoice
        );
    }
}
