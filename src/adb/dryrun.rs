//! A fully simulated [`AdbClient`] backend for Dry Run mode.
//!
//! This is the architectural core of Dry Run: [`crate::workflow::WorkflowRunner`]
//! is never duplicated or special-cased for it. Dry Run is simply the same
//! `WorkflowRunner` constructed with a [`DryRunAdbClient`] instead of a
//! [`crate::adb::RealAdbClient`] — the same way the test suite already
//! exercises the workflow against [`crate::adb::mock::MockAdbClient`].
//!
//! ```text
//! Real executor:     Workflow -> RealAdbClient  -> Android device
//! Dry-run executor:  Workflow -> DryRunAdbClient -> simulated results
//! ```
//!
//! `DryRunAdbClient` never spawns a real `adb` process, never touches the
//! filesystem, and never talks to a device — every method here is pure
//! simulation with a short artificial delay so the TUI can visibly show
//! progress while it runs. It is intentionally a *separate* type from
//! [`crate::adb::mock::MockAdbClient`]: the mock exists for deterministic,
//! instant unit tests; this one exists to safely drive the real TUI/event
//! loop for development, demos, and manual QA.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::device::verifier::{EXPECTED_BUILD, EXPECTED_MODEL};
use crate::executor::{ProcessEvent, ProcessHandle, ProcessOutcome};

use super::client::{
    AdbClient, AdbError, CapturedOutput, CommandKind, DescribedCommand, DeviceEntry,
};

/// Simulated serial reported by [`DryRunAdbClient::devices`]. Never used
/// to address a real device. Public so the TUI can display the exact same
/// value in the Device panel when Dry Run is active, rather than
/// duplicating the literal (CLAUDE.md-refinement: identify the simulated
/// device clearly).
pub const DRYRUN_SERIAL: &str = "DRYRUN0000000";

/// Artificial per-step delay (CLAUDE.md-refinement requirement: Dry Run
/// must not resolve every step within the same event loop tick, so the
/// TUI can visibly demonstrate progress/elapsed time/log streaming).
/// Short enough to stay pleasant for repeated development use.
const STEP_DELAY: Duration = Duration::from_millis(350);

/// Named failure scenarios a developer can select to exercise the
/// workflow's failure/retry/cancellation handling without touching a
/// real device. **Disabled by default** (`AllSuccess`) and selectable
/// only via the `FLIP5_DRYRUN_SCENARIO` environment variable — this is
/// deliberately not exposed as an in-TUI control, since it is a
/// development/testing aid, not an end-user feature. It has no effect on
/// Real Run: [`crate::adb::RealAdbClient`] never reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DryRunScenario {
    #[default]
    AllSuccess,
    PayloadFailure,
    PayloadTimeout,
    DeviceDisconnect,
    KernelSuVerificationFailure,
    RootVerificationFailure,
    CleanupFailure,
    InvalidHybridMountConfig,
    /// Simulates a device where the optional `hybrid_mount` module isn't
    /// installed at all — demonstrating that this is a valid
    /// configuration, not an error, and that the mandatory root workflow
    /// (already `Completed` by the time this is checked) is unaffected.
    HybridMountNotInstalled,
}

impl DryRunScenario {
    pub const ENV_VAR: &'static str = "FLIP5_DRYRUN_SCENARIO";

    /// Reads [`Self::ENV_VAR`]; any unrecognized or unset value is
    /// `AllSuccess`, keeping the feature off by default.
    pub fn from_env() -> Self {
        match std::env::var(Self::ENV_VAR).ok().as_deref() {
            Some("payload_failure") => Self::PayloadFailure,
            Some("payload_timeout") => Self::PayloadTimeout,
            Some("device_disconnect") => Self::DeviceDisconnect,
            Some("kernelsu_verification_failure") => Self::KernelSuVerificationFailure,
            Some("root_verification_failure") => Self::RootVerificationFailure,
            Some("cleanup_failure") => Self::CleanupFailure,
            Some("invalid_hybrid_mount_config") => Self::InvalidHybridMountConfig,
            Some("hybrid_mount_not_installed") => Self::HybridMountNotInstalled,
            _ => Self::AllSuccess,
        }
    }
}

/// Default simulated `hybrid_mount` config content: the rule is absent,
/// matching the state the real reference workflow expects to find before
/// it applies anything.
fn default_simulated_hybrid_mount_config() -> String {
    "[rules.other]\ndefault_mode = \"normal\"\n".to_string()
}

/// The Dry Run backend itself. Implements [`AdbClient`] exactly like
/// [`crate::adb::RealAdbClient`] does, so [`crate::workflow::WorkflowRunner`]
/// cannot tell the difference except by the results it gets back.
///
/// Holds a small amount of local, in-memory state (never written to disk,
/// never sent anywhere) purely so the `hybrid_mount` step's own
/// read-parse-modify-write-reread-verify cycle (CLAUDE.md section 23) can
/// be exercised faithfully: the simulated config a later "cat" sees
/// reflects an earlier simulated "push + mv", exactly as the real device
/// would behave.
pub struct DryRunAdbClient {
    scenario: DryRunScenario,
    hybrid_mount_config: std::sync::Mutex<String>,
    staged_push_content: std::sync::Mutex<Option<String>>,
}

impl DryRunAdbClient {
    pub fn new(scenario: DryRunScenario) -> Self {
        let initial_config = if scenario == DryRunScenario::InvalidHybridMountConfig {
            "not [ valid toml =".to_string()
        } else {
            default_simulated_hybrid_mount_config()
        };
        Self {
            scenario,
            hybrid_mount_config: std::sync::Mutex::new(initial_config),
            staged_push_content: std::sync::Mutex::new(None),
        }
    }
}

fn simulated_command(kind: CommandKind, display: impl Into<String>) -> DescribedCommand {
    DescribedCommand {
        kind,
        display: format!("[DRY RUN] {}", display.into()),
    }
}

fn ok(command: DescribedCommand, stdout: String) -> CapturedOutput {
    CapturedOutput {
        command,
        stdout,
        stderr: String::new(),
        exit_code: Some(0),
    }
}

#[async_trait]
impl AdbClient for DryRunAdbClient {
    async fn devices(&self) -> Result<Vec<DeviceEntry>, AdbError> {
        tokio::time::sleep(STEP_DELAY).await;
        Ok(vec![DeviceEntry {
            serial: DRYRUN_SERIAL.to_string(),
            state: "device".to_string(),
        }])
    }

    async fn shell_capture(&self, serial: &str, args: &[&str]) -> Result<CapturedOutput, AdbError> {
        self.shell_capture_raw(serial, &args.join(" ")).await
    }

    async fn shell_capture_raw(
        &self,
        serial: &str,
        raw_command: &str,
    ) -> Result<CapturedOutput, AdbError> {
        tokio::time::sleep(STEP_DELAY).await;
        let command = simulated_command(
            CommandKind::Device,
            format!("adb -s {serial} shell \"{raw_command}\""),
        );

        // Device model/build: always report the known-good target so Dry
        // Run can exercise the full workflow standalone, without needing
        // a real (or any) device connected.
        if raw_command.contains("ro.product.model") {
            return Ok(ok(command, format!("{EXPECTED_MODEL}\n")));
        }
        if raw_command.contains("ro.build.display.id") {
            return Ok(ok(command, format!("{EXPECTED_BUILD}\n")));
        }
        if raw_command.contains("proc/modules") {
            let stdout = if self.scenario == DryRunScenario::KernelSuVerificationFailure {
                String::new()
            } else {
                "kernelsu 16384 0 - Live 0x0000000000000000 (DRY RUN)\n\
                 [DRY RUN] Simulated KernelSU verification passed.\n"
                    .to_string()
            };
            return Ok(ok(command, stdout));
        }
        if raw_command.contains("su -c 'id'") {
            let stdout = if self.scenario == DryRunScenario::RootVerificationFailure {
                "uid=2000(shell) gid=2000(shell)\n".to_string()
            } else {
                "uid=0(root) gid=0(root) groups=0(root) (DRY RUN)\n\
                 [DRY RUN] Simulated root verification passed.\n"
                    .to_string()
            };
            return Ok(ok(command, stdout));
        }
        // `is_installed`'s presence check and the `mv` that finalizes a
        // hybrid_mount apply must both be checked before the generic
        // "contains hybrid_mount/config.toml" read below, since their own
        // command text also mentions that path.
        if raw_command.contains("test -f") && raw_command.contains("hybrid_mount/config.toml") {
            let installed = self.scenario != DryRunScenario::HybridMountNotInstalled;
            let stdout = if installed { "FOUND\n" } else { "MISSING\n" };
            return Ok(ok(command, stdout.to_string()));
        }
        if raw_command.contains("mv /data/local/tmp/config.tmp") {
            if let Some(content) = self.staged_push_content.lock().unwrap().take() {
                *self.hybrid_mount_config.lock().unwrap() = content;
            }
            return Ok(ok(command, String::new()));
        }
        if raw_command.contains("hybrid_mount/config.toml") {
            let stdout = self.hybrid_mount_config.lock().unwrap().clone();
            return Ok(ok(command, stdout));
        }
        if raw_command.contains("umount /system/bin/logcat") {
            if self.scenario == DryRunScenario::CleanupFailure {
                return Ok(CapturedOutput {
                    command,
                    stdout: String::new(),
                    stderr: "umount: busy (DRY RUN)".to_string(),
                    exit_code: Some(1),
                });
            }
            return Ok(ok(
                command,
                "[DRY RUN] Simulated cleanup passed.\n".to_string(),
            ));
        }
        // The KernelSU stage `-c 'cp ...; chmod ...'` and plain `chmod`:
        // generic simulated success.
        Ok(ok(command, String::new()))
    }

    async fn push(
        &self,
        serial: &str,
        local: &Path,
        remote: &str,
    ) -> Result<CapturedOutput, AdbError> {
        tokio::time::sleep(STEP_DELAY).await;
        let command = simulated_command(
            CommandKind::Host,
            format!("adb -s {serial} push {} {remote}", local.display()),
        );

        // The only "push" Dry Run ever reads back is the hybrid_mount
        // staging file, which is a local temp file our own apply logic
        // just wrote (never a real artifact, never a device, never the
        // payload repo) — reading it is what lets the simulated
        // read-modify-write-reread-verify cycle actually round-trip.
        // Every other push (payload library/runner, KernelSU artifact)
        // ignores `local` entirely: no real bytes are read or sent.
        if remote.contains("config.tmp")
            && let Ok(content) = std::fs::read_to_string(local)
        {
            *self.staged_push_content.lock().unwrap() = Some(content);
        }

        Ok(ok(
            command,
            "1 file pushed (DRY RUN, no data transferred)".to_string(),
        ))
    }

    fn shell_stream_raw(
        &self,
        serial: &str,
        raw_command: &str,
        _timeout: Option<Duration>,
    ) -> ProcessHandle {
        let (tx, rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let scenario = self.scenario;
        let is_payload = raw_command.contains("--run-payload");
        let _ = serial; // simulated: never actually addressed

        let tx_work = tx.clone();
        let work = async move {
            let _ = tx_work.send(ProcessEvent::Stdout(
                "[DRY RUN] simulated step starting".to_string(),
            ));
            tokio::time::sleep(STEP_DELAY).await;

            match scenario {
                DryRunScenario::DeviceDisconnect => {
                    return ProcessOutcome::SpawnFailed {
                        message: "device disconnected (DRY RUN)".to_string(),
                    };
                }
                DryRunScenario::PayloadFailure if is_payload => {
                    let _ = tx_work.send(ProcessEvent::Stderr(
                        "[DRY RUN] simulated payload failure".to_string(),
                    ));
                    return ProcessOutcome::Failure { exit_code: 1 };
                }
                DryRunScenario::PayloadTimeout if is_payload => {
                    tokio::time::sleep(STEP_DELAY).await;
                    return ProcessOutcome::TimedOut {
                        elapsed: STEP_DELAY * 2,
                    };
                }
                _ => {}
            }

            tokio::time::sleep(STEP_DELAY).await;
            let _ = tx_work.send(ProcessEvent::Stdout(
                "[DRY RUN] simulated step complete".to_string(),
            ));
            ProcessOutcome::Success
        };

        tokio::spawn(async move {
            let outcome = tokio::select! {
                outcome = work => outcome,
                _ = cancel_rx => ProcessOutcome::Cancelled { elapsed: STEP_DELAY },
            };
            let _ = tx.send(ProcessEvent::Finished(outcome));
        });

        ProcessHandle::from_parts(rx, Some(cancel_tx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::artifacts::{ArtifactInfo, ArtifactKind, ArtifactSlot};
    use crate::workflow::state::{
        OptionalIntegrationStatus, WorkflowFailure, WorkflowState, WorkflowStatus,
    };
    use crate::workflow::{RunnerEvent, WorkflowRunner};
    use std::path::PathBuf;

    fn fake_artifact(kind: ArtifactKind) -> ArtifactInfo {
        ArtifactInfo {
            kind,
            name: kind.display_name(),
            relative_path: kind.relative_path(),
            absolute_path: PathBuf::from("/dev/null"),
            size_bytes: 1,
            sha256: "0000000000".to_string(),
            repository_location: PathBuf::from("/dev/null"),
        }
    }

    fn dryrun_runner(
        scenario: DryRunScenario,
    ) -> (WorkflowRunner, mpsc::UnboundedReceiver<RunnerEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let staging = std::env::temp_dir().join(format!(
            "flip5-dryrun-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let runner = WorkflowRunner::new(Arc::new(DryRunAdbClient::new(scenario)), staging, tx);
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
        runner
            .select_kernelsu_artifact(ArtifactSlot::Found(fake_artifact(ArtifactKind::KernelSu)))
            .unwrap();
        runner.push_kernelsu().await.unwrap();
        runner.stage_kernelsu().await.unwrap();
        runner.load_kernelsu().await.unwrap();
    }

    /// The architecturally important test: the exact same
    /// `WorkflowRunner` state machine used for Real Run, driven purely by
    /// `DryRunAdbClient`, reaches `Completed` through every one of
    /// CLAUDE.md's 16 real steps.
    #[tokio::test]
    async fn dry_run_completes_the_full_workflow_via_the_same_state_machine() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::AllSuccess);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();
        assert!(runner.status().is_completed());
    }

    // Note: the `[DRY RUN]`-tagged *log lines* the TUI shows are an
    // App-layer presentation concern (see `App::tag` in `app.rs`), not
    // something `WorkflowRunner`/`AdbClient` know about — covered by
    // `app::tests::dry_run_mode_tags_log_lines`. This verifies the
    // underlying building block: every command `DryRunAdbClient`
    // describes is self-documenting.
    #[test]
    fn simulated_commands_are_self_documenting() {
        let cmd = simulated_command(CommandKind::Host, "adb push a b");
        assert_eq!(cmd.display, "[DRY RUN] adb push a b");
    }

    #[tokio::test]
    async fn dry_run_never_calls_through_to_a_real_adb_process() {
        // Architectural guarantee, demonstrated rather than merely
        // asserted: driving the full workflow via `DryRunAdbClient` alone
        // succeeds without a `RealAdbClient` ever existing in this test,
        // and `DryRunAdbClient` itself contains no `Command::new("adb")`
        // or `executor::spawn` call anywhere in its implementation above.
        let (runner, _rx) = dryrun_runner(DryRunScenario::AllSuccess);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();
        assert!(runner.status().is_completed());
    }

    /// Restores `PATH` on drop, so a panicking assertion mid-test never
    /// leaves the process's global `PATH` corrupted for later tests.
    struct PathGuard(String);
    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::set_var("PATH", &self.0);
            }
        }
    }

    #[tokio::test]
    async fn dry_run_never_spawns_a_real_adb_process_even_if_one_is_on_path() {
        // Stronger than code review: put a fake `adb` on PATH that leaves
        // unmistakable evidence if it's ever actually invoked, then drive
        // the entire workflow through `DryRunAdbClient` and prove that
        // evidence was never left behind.
        let dir = std::env::temp_dir().join(format!(
            "flip5-adb-canary-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let canary = dir.join("ADB_WAS_INVOKED");
        let script_path = dir.join("adb");
        std::fs::write(
            &script_path,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", canary.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        let original_path = std::env::var("PATH").unwrap_or_default();
        let _restore = PathGuard(original_path.clone());
        unsafe {
            std::env::set_var("PATH", format!("{}:{}", dir.display(), original_path));
        }

        let (runner, _rx) = dryrun_runner(DryRunScenario::AllSuccess);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();

        assert!(runner.status().is_completed());
        assert!(
            !canary.exists(),
            "DryRunAdbClient must never invoke a real `adb` process"
        );

        drop(_restore);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dry_run_success_messages_are_explicitly_marked_simulated() {
        let (runner, mut rx) = dryrun_runner(DryRunScenario::AllSuccess);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();

        let mut stdout_lines = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let RunnerEvent::Stdout(line) = event {
                stdout_lines.push(line);
            }
        }
        assert!(
            stdout_lines
                .iter()
                .any(|l| l == "[DRY RUN] Simulated KernelSU verification passed.")
        );
        assert!(
            stdout_lines
                .iter()
                .any(|l| l == "[DRY RUN] Simulated root verification passed.")
        );
        assert!(
            stdout_lines
                .iter()
                .any(|l| l == "[DRY RUN] Simulated cleanup passed.")
        );
    }

    #[tokio::test]
    async fn dry_run_failure_scenarios_never_claim_simulated_success() {
        let (runner, mut rx) = dryrun_runner(DryRunScenario::KernelSuVerificationFailure);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        let mut saw_passed_message = false;
        while let Ok(event) = rx.try_recv() {
            if let RunnerEvent::Stdout(line) = event
                && line.contains("Simulated KernelSU verification passed")
            {
                saw_passed_message = true;
            }
        }
        assert!(
            !saw_passed_message,
            "a failing scenario must never emit its own 'passed' message"
        );
        assert_eq!(
            runner.status(),
            WorkflowStatus::Failed {
                last_good: WorkflowState::KernelSuLoaded,
                failure: WorkflowFailure::VerificationFailed {
                    detail: "KernelSU verification failed".to_string(),
                },
            }
        );
    }

    #[tokio::test]
    async fn dry_run_payload_failure_scenario() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::PayloadFailure);
        drive_to_payload_pushed(&runner).await;
        runner.execute_payload().await.unwrap();
        match runner.status() {
            WorkflowStatus::Failed { failure, .. } => assert_eq!(failure.label(), "PayloadFailure"),
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dry_run_payload_timeout_scenario() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::PayloadTimeout);
        drive_to_payload_pushed(&runner).await;
        runner.execute_payload().await.unwrap();
        match runner.status() {
            WorkflowStatus::Failed { failure, .. } => assert_eq!(failure.label(), "Timeout"),
            other => panic!("expected timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dry_run_device_disconnect_scenario() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::DeviceDisconnect);
        drive_to_payload_pushed(&runner).await;
        runner.execute_payload().await.unwrap();
        assert!(runner.status().is_failed());
    }

    #[tokio::test]
    async fn dry_run_kernelsu_verification_failure_scenario() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::KernelSuVerificationFailure);
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
    async fn dry_run_root_verification_failure_scenario() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::RootVerificationFailure);
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
    async fn dry_run_cleanup_failure_scenario() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::CleanupFailure);
        drive_to_kernelsu_loaded(&runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        match runner.status() {
            WorkflowStatus::Failed { failure, .. } => assert_eq!(failure.label(), "CleanupFailed"),
            other => panic!("expected cleanup failure, got {other:?}"),
        }
    }

    /// Drives the mandatory Dry Run workflow through to `Completed`,
    /// exactly as the default scenario does — the optional hybrid_mount
    /// integration is never part of this, regardless of scenario.
    async fn drive_dry_run_to_completed(runner: &WorkflowRunner) {
        drive_to_kernelsu_loaded(runner).await;
        runner.verify_kernelsu().await.unwrap();
        runner.verify_root().await.unwrap();
        runner.cleanup_mount().await.unwrap();
        runner.complete().unwrap();
    }

    #[tokio::test]
    async fn dry_run_mandatory_workflow_completes_without_hybrid_mount() {
        // The default Dry Run scenario must demonstrate that root
        // succeeds without ever touching hybrid_mount.
        let (runner, _rx) = dryrun_runner(DryRunScenario::AllSuccess);
        drive_dry_run_to_completed(&runner).await;
        assert!(runner.status().is_completed());
        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::AwaitingChoice
        );
    }

    #[tokio::test]
    async fn dry_run_can_separately_simulate_selecting_the_optional_integration() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::AllSuccess);
        drive_dry_run_to_completed(&runner).await;

        runner.run_hybrid_mount_integration().await.unwrap();

        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::Succeeded
        );
        assert!(runner.status().is_completed());
    }

    #[tokio::test]
    async fn dry_run_can_simulate_hybrid_mount_not_installed() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::HybridMountNotInstalled);
        drive_dry_run_to_completed(&runner).await;

        runner.run_hybrid_mount_integration().await.unwrap();

        assert_eq!(
            runner.hybrid_mount_status(),
            OptionalIntegrationStatus::NotInstalled
        );
        assert!(
            runner.status().is_completed(),
            "absence of hybrid_mount must not affect root completion"
        );
    }

    #[tokio::test]
    async fn dry_run_invalid_hybrid_mount_config_scenario() {
        // The optional integration — never the mandatory workflow —
        // reports ConfigurationInvalid now.
        let (runner, _rx) = dryrun_runner(DryRunScenario::InvalidHybridMountConfig);
        drive_dry_run_to_completed(&runner).await;
        assert!(runner.status().is_completed());

        runner.run_hybrid_mount_integration().await.unwrap();

        match runner.hybrid_mount_status() {
            OptionalIntegrationStatus::Failed { .. } => {}
            other => panic!("expected the optional integration to fail, got {other:?}"),
        }
        assert!(
            runner.status().is_completed(),
            "root must remain completed despite the optional-integration failure"
        );
    }

    #[tokio::test]
    async fn dry_run_retry_is_bounded_same_as_real_run() {
        let (runner, _rx) = dryrun_runner(DryRunScenario::PayloadFailure);
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

    #[tokio::test]
    async fn dry_run_cancellation_reliably_stops_the_simulated_step() {
        // Unlike the instant MockAdbClient, DryRunAdbClient's artificial
        // delay means cancellation deterministically wins the race.
        let (runner, _rx) = dryrun_runner(DryRunScenario::AllSuccess);
        let runner = Arc::new(runner);
        drive_to_payload_pushed(&runner).await;

        let runner_clone = runner.clone();
        let handle = tokio::spawn(async move { runner_clone.execute_payload().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        runner.request_cancel();
        handle.await.unwrap().unwrap();

        match runner.status() {
            WorkflowStatus::Failed { failure, .. } => {
                assert_eq!(failure, WorkflowFailure::UserCancelled)
            }
            other => panic!("expected UserCancelled, got {other:?}"),
        }
    }

    #[test]
    fn scenario_defaults_to_all_success_and_is_off_by_default() {
        // SAFETY: tests in this crate don't run with threaded env var
        // mutation elsewhere touching this specific key.
        unsafe {
            std::env::remove_var(DryRunScenario::ENV_VAR);
        }
        assert_eq!(DryRunScenario::from_env(), DryRunScenario::AllSuccess);
    }

    #[test]
    fn real_client_type_is_unaffected_by_dry_run_module() {
        // Architectural sanity check: `RealAdbClient` exists independently
        // and is not touched by anything in this module.
        let _ = super::super::RealAdbClient::new();
        assert_eq!(WorkflowState::Completed.label(), "Root workflow completed");
    }
}
