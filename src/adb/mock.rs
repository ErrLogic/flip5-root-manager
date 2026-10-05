//! Mocked ADB layer used by tests and for offline TUI development without a
//! physical device attached.
//!
//! The mock inspects the text of the command it is asked to run (exactly as
//! a real device's shell would receive it) and returns a scripted response.
//! This lets workflow/state-machine tests exercise the exact same code path
//! that talks to [`crate::adb::AdbClient`] in production.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::executor::{ProcessEvent, ProcessHandle, ProcessOutcome};

use super::client::{
    AdbClient, AdbError, CapturedOutput, CommandKind, DescribedCommand, DeviceEntry,
};

/// A scripted outcome for a single command.
#[derive(Debug, Clone)]
pub enum Scripted {
    Success {
        stdout: String,
    },
    Failure {
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
    Timeout,
}

impl Scripted {
    pub fn ok(stdout: impl Into<String>) -> Self {
        Scripted::Success {
            stdout: stdout.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MockState {
    pub serial: String,
    /// `None` means no device is attached / device is disconnected.
    pub device_state: Option<String>,
    pub model: String,
    pub build_id: String,
    pub run_payload: Scripted,
    pub kernelsu_load: Scripted,
    pub kernelsu_modules_output: String,
    pub root_id_output: String,
    pub hybrid_mount_config: String,
    pub cleanup_result: Scripted,
    pub push_result: Scripted,
    pub stage_kernelsu_result: Scripted,
    /// Simulates the remote staging file left behind by `push` so a
    /// subsequent `mv` command can "apply" it to `hybrid_mount_config`,
    /// letting tests exercise the full read-modify-write-reread-verify
    /// cycle without a real device.
    pub staged_push_content: Option<String>,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            serial: "R58N00000XX".to_string(),
            device_state: Some("device".to_string()),
            model: "SM-F731B".to_string(),
            build_id: "BP4A.251205.006.F731BXXS7GZG1".to_string(),
            run_payload: Scripted::ok("payload complete"),
            kernelsu_load: Scripted::ok("late-load complete"),
            kernelsu_modules_output: "kernelsu 16384 0 - Live 0x0000000000000000\n".to_string(),
            root_id_output: "uid=0(root) gid=0(root) groups=0(root)\n".to_string(),
            hybrid_mount_config: "[rules.other]\ndefault_mode = \"normal\"\n".to_string(),
            cleanup_result: Scripted::ok(""),
            push_result: Scripted::ok("1 file pushed"),
            stage_kernelsu_result: Scripted::ok(""),
            staged_push_content: None,
        }
    }
}

/// Named presets mirroring CLAUDE.md section 26.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockScenario {
    DeviceConnected,
    DeviceMismatch,
    BuildMismatch,
    PayloadSuccess,
    PayloadFailure,
    PayloadTimeout,
    KernelSuLoadSuccess,
    KernelSuLoadFailure,
    RootSuccess,
    RootFailure,
    DeviceDisconnect,
    CleanupFailure,
    InvalidConfig,
}

pub struct MockAdbClient {
    state: Mutex<MockState>,
}

impl MockAdbClient {
    pub fn new(state: MockState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }

    pub fn scenario(scenario: MockScenario) -> Self {
        let mut state = MockState::default();
        match scenario {
            MockScenario::DeviceConnected => {}
            MockScenario::DeviceMismatch => state.model = "SM-OTHER".to_string(),
            MockScenario::BuildMismatch => state.build_id = "WRONG.BUILD.ID".to_string(),
            MockScenario::PayloadSuccess => {}
            MockScenario::PayloadFailure => {
                state.run_payload = Scripted::Failure {
                    stdout: "attempt 1\nattempt 2\nattempt 3\n".to_string(),
                    stderr: "exploit failed\n".to_string(),
                    exit_code: 1,
                }
            }
            MockScenario::PayloadTimeout => state.run_payload = Scripted::Timeout,
            MockScenario::KernelSuLoadSuccess => {}
            MockScenario::KernelSuLoadFailure => {
                state.kernelsu_load = Scripted::Failure {
                    stdout: String::new(),
                    stderr: "mount failed\n".to_string(),
                    exit_code: 1,
                };
                state.kernelsu_modules_output = String::new();
            }
            MockScenario::RootSuccess => {}
            MockScenario::RootFailure => {
                state.root_id_output = "uid=2000(shell) gid=2000(shell)\n".to_string()
            }
            MockScenario::DeviceDisconnect => state.device_state = None,
            MockScenario::CleanupFailure => {
                state.cleanup_result = Scripted::Failure {
                    stdout: String::new(),
                    stderr: "umount: busy\n".to_string(),
                    exit_code: 1,
                }
            }
            MockScenario::InvalidConfig => {
                state.hybrid_mount_config = "not [ valid toml =".to_string()
            }
        }
        Self::new(state)
    }

    pub fn set_device_state(&self, value: Option<String>) {
        self.state.lock().unwrap().device_state = value;
    }

    pub fn with_hybrid_mount_config(self, config: impl Into<String>) -> Self {
        self.state.lock().unwrap().hybrid_mount_config = config.into();
        self
    }

    pub fn hybrid_mount_config(&self) -> String {
        self.state.lock().unwrap().hybrid_mount_config.clone()
    }

    pub fn set_hybrid_mount_config(&self, config: impl Into<String>) {
        self.state.lock().unwrap().hybrid_mount_config = config.into();
    }
}

fn scripted_to_capture(
    command: DescribedCommand,
    scripted: &Scripted,
) -> Result<CapturedOutput, AdbError> {
    match scripted {
        Scripted::Success { stdout } => Ok(CapturedOutput {
            command,
            stdout: stdout.clone(),
            stderr: String::new(),
            exit_code: Some(0),
        }),
        Scripted::Failure {
            stdout,
            stderr,
            exit_code,
        } => Ok(CapturedOutput {
            command,
            stdout: stdout.clone(),
            stderr: stderr.clone(),
            exit_code: Some(*exit_code),
        }),
        Scripted::Timeout => Err(AdbError::CommandFailed {
            exit_code: None,
            stderr: "timed out".into(),
        }),
    }
}

#[async_trait]
impl AdbClient for MockAdbClient {
    async fn devices(&self) -> Result<Vec<DeviceEntry>, AdbError> {
        let state = self.state.lock().unwrap();
        match &state.device_state {
            Some(s) => Ok(vec![DeviceEntry {
                serial: state.serial.clone(),
                state: s.clone(),
            }]),
            None => Ok(vec![]),
        }
    }

    async fn shell_capture(&self, serial: &str, args: &[&str]) -> Result<CapturedOutput, AdbError> {
        let joined = args.join(" ");
        self.shell_capture_raw(serial, &joined).await
    }

    async fn shell_capture_raw(
        &self,
        serial: &str,
        raw_command: &str,
    ) -> Result<CapturedOutput, AdbError> {
        let state = self.state.lock().unwrap();
        if state.device_state.is_none() {
            return Err(AdbError::DeviceDisconnected);
        }
        let kind = if raw_command.contains("su ") || raw_command.contains("su -c") {
            CommandKind::RootDevice
        } else {
            CommandKind::Device
        };
        let command = DescribedCommand {
            kind,
            display: format!("adb -s {serial} shell \"{raw_command}\""),
        };

        if raw_command.contains("ro.product.model") {
            return Ok(CapturedOutput {
                command,
                stdout: format!("{}\n", state.model),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }
        if raw_command.contains("ro.build.display.id") {
            return Ok(CapturedOutput {
                command,
                stdout: format!("{}\n", state.build_id),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }
        if raw_command.contains("proc/modules") {
            return Ok(CapturedOutput {
                command,
                stdout: state.kernelsu_modules_output.clone(),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }
        if raw_command.contains("su -c 'id'") || raw_command.contains("su -c \"id\"") {
            return Ok(CapturedOutput {
                command,
                stdout: state.root_id_output.clone(),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }
        if raw_command.contains("test -f") && raw_command.contains("hybrid_mount/config.toml") {
            return Ok(CapturedOutput {
                command,
                stdout: "FOUND\n".to_string(),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }
        if raw_command.contains("hybrid_mount/config.toml") && raw_command.contains("tail") {
            return Ok(CapturedOutput {
                command,
                stdout: state.hybrid_mount_config.clone(),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }
        if raw_command.contains("hybrid_mount/config.toml") && raw_command.contains("cat") {
            return Ok(CapturedOutput {
                command,
                stdout: state.hybrid_mount_config.clone(),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }
        if raw_command.contains("mv /data/local/tmp/config.tmp") {
            drop(state);
            let mut state = self.state.lock().unwrap();
            if let Some(content) = state.staged_push_content.take() {
                state.hybrid_mount_config = content;
            }
            return Ok(CapturedOutput {
                command,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }
        if raw_command.contains(".ksud-stage") {
            return scripted_to_capture(command, &state.stage_kernelsu_result);
        }
        if raw_command.contains("umount /system/bin/logcat") {
            return scripted_to_capture(command, &state.cleanup_result);
        }
        if raw_command.starts_with("chmod 755") {
            return Ok(CapturedOutput {
                command,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
            });
        }

        Ok(CapturedOutput {
            command,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
        })
    }

    async fn push(
        &self,
        serial: &str,
        local: &Path,
        remote: &str,
    ) -> Result<CapturedOutput, AdbError> {
        let mut state = self.state.lock().unwrap();
        if state.device_state.is_none() {
            return Err(AdbError::DeviceDisconnected);
        }
        let command = DescribedCommand {
            kind: CommandKind::Host,
            display: format!("adb -s {serial} push {} {remote}", local.display()),
        };
        if remote.contains("config.tmp") {
            state.staged_push_content = std::fs::read_to_string(local).ok();
        }
        let result = state.push_result.clone();
        scripted_to_capture(command, &result)
    }

    fn shell_stream_raw(
        &self,
        serial: &str,
        raw_command: &str,
        _timeout: Option<Duration>,
    ) -> ProcessHandle {
        let (tx, rx) = mpsc::unbounded_channel();
        let scripted = {
            let state = self.state.lock().unwrap();
            if state.device_state.is_none() {
                let _ = tx.send(ProcessEvent::Finished(ProcessOutcome::SpawnFailed {
                    message: "device disconnected".to_string(),
                }));
                return ProcessHandle::from_parts(rx, None);
            }
            if raw_command.contains("--run-payload") {
                state.run_payload.clone()
            } else if raw_command.contains("late-load") {
                state.kernelsu_load.clone()
            } else {
                Scripted::ok("")
            }
        };
        let _ = serial;
        tokio::spawn(async move {
            match scripted {
                Scripted::Success { stdout } => {
                    for line in stdout.lines() {
                        let _ = tx.send(ProcessEvent::Stdout(line.to_string()));
                    }
                    let _ = tx.send(ProcessEvent::Finished(ProcessOutcome::Success));
                }
                Scripted::Failure {
                    stdout,
                    stderr,
                    exit_code,
                } => {
                    for line in stdout.lines() {
                        let _ = tx.send(ProcessEvent::Stdout(line.to_string()));
                    }
                    for line in stderr.lines() {
                        let _ = tx.send(ProcessEvent::Stderr(line.to_string()));
                    }
                    let _ = tx.send(ProcessEvent::Finished(ProcessOutcome::Failure {
                        exit_code,
                    }));
                }
                Scripted::Timeout => {
                    let _ = tx.send(ProcessEvent::Finished(ProcessOutcome::TimedOut {
                        elapsed: Duration::from_secs(0),
                    }));
                }
            }
        });
        ProcessHandle::from_parts(rx, None)
    }
}
