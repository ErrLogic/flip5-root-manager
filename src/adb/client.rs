//! `AdbClient`: the single abstraction through which this application talks
//! to `adb`. No other module should call `Command::new("adb")` directly.
//!
//! Every method is tagged with a [`CommandKind`] describing which side of
//! the host/device boundary it crosses (see CLAUDE.md section 13), and every
//! method returns (or streams) the exact command line that was executed so
//! the TUI can display it.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

use crate::executor::{self, ProcessHandle, ProcessOutcome};

/// Which side of the host/device boundary a command operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// Runs on the WSL/Linux host (`adb devices`, `adb push`, ...).
    Host,
    /// Runs inside the Android device's shell (`adb shell ...`).
    Device,
    /// Runs inside the Android device's shell as root (`adb shell su -c ...`).
    RootDevice,
}

/// A command about to be (or that was) executed, in a form suitable for
/// display in the TUI log.
#[derive(Debug, Clone)]
pub struct DescribedCommand {
    pub kind: CommandKind,
    /// Human-readable command line, e.g. `adb shell getprop ro.product.model`.
    pub display: String,
}

/// The fully captured result of a short-lived command (one that runs to
/// completion rather than being streamed).
#[derive(Debug, Clone)]
pub struct CapturedOutput {
    pub command: DescribedCommand,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

impl CapturedOutput {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub fn stdout_trimmed(&self) -> &str {
        self.stdout.trim()
    }
}

#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub serial: String,
    /// Raw state string as reported by `adb devices` (e.g. "device",
    /// "unauthorized", "offline").
    pub state: String,
}

impl DeviceEntry {
    pub fn is_ready(&self) -> bool {
        self.state == "device"
    }
}

#[derive(Debug, Error)]
pub enum AdbError {
    #[error("failed to spawn adb: {0}")]
    SpawnFailed(String),
    #[error("adb command failed (exit {exit_code:?}): {stderr}")]
    CommandFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
    #[error("no device connected")]
    NoDevice,
    #[error("multiple devices connected: {0:?}")]
    MultipleDevices(Vec<String>),
    #[error("device disconnected")]
    DeviceDisconnected,
}

/// The ADB abstraction used throughout the application.
///
/// Implemented by [`RealAdbClient`] (shells out to the real `adb` binary)
/// and by [`crate::adb::mock::MockAdbClient`] (for tests and offline TUI
/// development).
#[async_trait]
pub trait AdbClient: Send + Sync {
    /// `adb devices` — host command.
    async fn devices(&self) -> Result<Vec<DeviceEntry>, AdbError>;

    /// Run a short device-shell command to completion and capture its
    /// output. Use for quick verification/inspection commands, never for
    /// long-running operations.
    async fn shell_capture(&self, serial: &str, args: &[&str]) -> Result<CapturedOutput, AdbError>;

    /// Run a single literal string through the device shell (`adb shell
    /// "<command>"`) and capture output. Used only for the handful of
    /// reference commands that intentionally rely on device-shell syntax
    /// (pipes, `su -c '...'`, etc.) per CLAUDE.md section 12.
    async fn shell_capture_raw(
        &self,
        serial: &str,
        raw_command: &str,
    ) -> Result<CapturedOutput, AdbError>;

    /// `adb push <local> <remote>` — host command.
    async fn push(
        &self,
        serial: &str,
        local: &Path,
        remote: &str,
    ) -> Result<CapturedOutput, AdbError>;

    /// Start a long-running device-shell command and stream its output.
    /// Used for the payload execution and KernelSU load steps.
    fn shell_stream_raw(
        &self,
        serial: &str,
        raw_command: &str,
        timeout: Option<Duration>,
    ) -> ProcessHandle;
}

/// Real implementation: shells out to the `adb` binary on PATH.
pub struct RealAdbClient {
    adb_binary: String,
}

impl RealAdbClient {
    pub fn new() -> Self {
        Self {
            adb_binary: "adb".to_string(),
        }
    }
}

impl Default for RealAdbClient {
    fn default() -> Self {
        Self::new()
    }
}

async fn run_capture(
    adb_binary: &str,
    args: Vec<String>,
    kind: CommandKind,
    display: String,
) -> Result<CapturedOutput, AdbError> {
    let mut handle = executor::spawn(adb_binary.to_string(), args, None);
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut outcome = None;
    while let Some(event) = handle.recv().await {
        match event {
            executor::ProcessEvent::Stdout(l) => {
                stdout.push_str(&l);
                stdout.push('\n');
            }
            executor::ProcessEvent::Stderr(l) => {
                stderr.push_str(&l);
                stderr.push('\n');
            }
            executor::ProcessEvent::Finished(o) => outcome = Some(o),
        }
    }
    let command = DescribedCommand { kind, display };
    match outcome {
        Some(ProcessOutcome::Success) => Ok(CapturedOutput {
            command,
            stdout,
            stderr,
            exit_code: Some(0),
        }),
        Some(ProcessOutcome::Failure { exit_code }) => Ok(CapturedOutput {
            command,
            stdout,
            stderr,
            exit_code: Some(exit_code),
        }),
        Some(ProcessOutcome::SpawnFailed { message }) => Err(AdbError::SpawnFailed(message)),
        Some(ProcessOutcome::TimedOut { .. }) | Some(ProcessOutcome::Cancelled { .. }) | None => {
            Err(AdbError::CommandFailed {
                exit_code: None,
                stderr: "command did not complete".into(),
            })
        }
    }
}

#[async_trait]
impl AdbClient for RealAdbClient {
    async fn devices(&self) -> Result<Vec<DeviceEntry>, AdbError> {
        let out = run_capture(
            &self.adb_binary,
            vec!["devices".into()],
            CommandKind::Host,
            "adb devices".into(),
        )
        .await?;
        let entries = out
            .stdout
            .lines()
            .skip(1) // header: "List of devices attached"
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let serial = parts.next()?;
                let state = parts.next()?;
                Some(DeviceEntry {
                    serial: serial.to_string(),
                    state: state.to_string(),
                })
            })
            .collect();
        Ok(entries)
    }

    async fn shell_capture(&self, serial: &str, args: &[&str]) -> Result<CapturedOutput, AdbError> {
        let mut full_args = vec!["-s".to_string(), serial.to_string(), "shell".to_string()];
        full_args.extend(args.iter().map(|s| s.to_string()));
        let display = format!("adb -s {serial} shell {}", args.join(" "));
        run_capture(&self.adb_binary, full_args, CommandKind::Device, display).await
    }

    async fn shell_capture_raw(
        &self,
        serial: &str,
        raw_command: &str,
    ) -> Result<CapturedOutput, AdbError> {
        let full_args = vec![
            "-s".to_string(),
            serial.to_string(),
            "shell".to_string(),
            raw_command.to_string(),
        ];
        let kind = if raw_command.trim_start().starts_with("su ") {
            CommandKind::RootDevice
        } else {
            CommandKind::Device
        };
        let display = format!("adb -s {serial} shell \"{raw_command}\"");
        run_capture(&self.adb_binary, full_args, kind, display).await
    }

    async fn push(
        &self,
        serial: &str,
        local: &Path,
        remote: &str,
    ) -> Result<CapturedOutput, AdbError> {
        let local_display = local.display().to_string();
        let args = vec![
            "-s".to_string(),
            serial.to_string(),
            "push".to_string(),
            local_display.clone(),
            remote.to_string(),
        ];
        let display = format!("adb -s {serial} push {local_display} {remote}");
        run_capture(&self.adb_binary, args, CommandKind::Host, display).await
    }

    fn shell_stream_raw(
        &self,
        serial: &str,
        raw_command: &str,
        timeout: Option<Duration>,
    ) -> ProcessHandle {
        let args = vec![
            "-s".to_string(),
            serial.to_string(),
            "shell".to_string(),
            raw_command.to_string(),
        ];
        executor::spawn(self.adb_binary.clone(), args, timeout)
    }
}
