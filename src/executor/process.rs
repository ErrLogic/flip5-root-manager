//! Generic process executor: spawns a command with structured arguments,
//! streams stdout/stderr line-by-line, and supports timeout and cancellation.
//!
//! This module has no knowledge of ADB, Android, or the payload workflow.
//! It is a pure host-process abstraction used by [`crate::adb`].

use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

/// A single event emitted while a spawned process runs.
#[derive(Debug, Clone)]
pub enum ProcessEvent {
    /// One line of stdout, without the trailing newline.
    Stdout(String),
    /// One line of stderr, without the trailing newline.
    Stderr(String),
    /// The process has fully terminated (or could not be started).
    Finished(ProcessOutcome),
}

/// How a spawned process finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// Exited with status code 0.
    Success,
    /// Exited with a non-zero status code.
    Failure { exit_code: i32 },
    /// Killed because it exceeded the configured timeout.
    TimedOut { elapsed: Duration },
    /// Killed because the caller requested cancellation.
    Cancelled { elapsed: Duration },
    /// The process could not even be spawned (e.g. binary not found).
    SpawnFailed { message: String },
}

impl ProcessOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, ProcessOutcome::Success)
    }
}

/// A handle to a running (or just-finished) process.
///
/// Drop the handle's `cancel` sender implicitly by dropping the handle, or
/// call [`ProcessHandle::cancel`] explicitly to request termination.
pub struct ProcessHandle {
    events: mpsc::UnboundedReceiver<ProcessEvent>,
    cancel_tx: Option<oneshot::Sender<()>>,
}

impl ProcessHandle {
    /// Build a handle directly from a receiver, for test doubles (see
    /// [`crate::adb::mock`]) that don't spawn a real OS process. Passing
    /// `None` for `cancel_tx` means [`ProcessHandle::cancel`] is a no-op.
    pub fn from_parts(
        events: mpsc::UnboundedReceiver<ProcessEvent>,
        cancel_tx: Option<oneshot::Sender<()>>,
    ) -> Self {
        Self { events, cancel_tx }
    }

    /// Receive the next event. Returns `None` once the process has finished
    /// and all events have been delivered.
    pub async fn recv(&mut self) -> Option<ProcessEvent> {
        self.events.recv().await
    }

    /// Request cancellation of the running process. Safe to call multiple
    /// times; only the first call has an effect.
    pub fn cancel(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            // Receiver may already be gone if the process already finished;
            // that's fine, it just means cancellation is moot.
            let _ = tx.send(());
        }
    }
}

/// Spawn `program` with `args`, streaming output and honoring `timeout`.
///
/// Uses structured arguments (`Command::args`) rather than shell strings.
pub fn spawn(
    program: impl Into<String>,
    args: Vec<String>,
    timeout: Option<Duration>,
) -> ProcessHandle {
    let program = program.into();
    let (tx, rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = oneshot::channel();

    tokio::spawn(async move {
        run(program, args, timeout, tx, cancel_rx).await;
    });

    ProcessHandle {
        events: rx,
        cancel_tx: Some(cancel_tx),
    }
}

async fn run(
    program: String,
    args: Vec<String>,
    timeout: Option<Duration>,
    tx: mpsc::UnboundedSender<ProcessEvent>,
    mut cancel_rx: oneshot::Receiver<()>,
) {
    let start = Instant::now();

    let mut child = match Command::new(&program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            let _ = tx.send(ProcessEvent::Finished(ProcessOutcome::SpawnFailed {
                message: format!("failed to spawn `{program}`: {e}"),
            }));
            return;
        }
    };

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let stdout_tx = tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = stdout_tx.send(ProcessEvent::Stdout(line));
        }
    });

    let stderr_tx = tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = stderr_tx.send(ProcessEvent::Stderr(line));
        }
    });

    // Every branch below settles the outcome on its first and only
    // resolution, so this is a single `select!`, not a loop: either the
    // child exits on its own, or we observe a cancel/timeout signal and
    // kill it ourselves. There is nothing left to wait for afterwards.
    let timeout_fut = async {
        match timeout {
            Some(d) => {
                tokio::time::sleep(d).await;
            }
            None => {
                // Never resolves when there is no configured timeout.
                std::future::pending::<()>().await;
            }
        }
    };

    let outcome = tokio::select! {
        status = child.wait() => {
            match status {
                Ok(status) => match status.code() {
                    Some(0) => ProcessOutcome::Success,
                    Some(code) => ProcessOutcome::Failure { exit_code: code },
                    None => ProcessOutcome::Failure { exit_code: -1 },
                },
                Err(e) => ProcessOutcome::SpawnFailed { message: format!("wait failed: {e}") },
            }
        }
        _ = &mut cancel_rx => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            ProcessOutcome::Cancelled { elapsed: start.elapsed() }
        }
        _ = timeout_fut => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            ProcessOutcome::TimedOut { elapsed: start.elapsed() }
        }
    };

    // Make sure all buffered output has been forwarded before announcing
    // completion, so consumers never see `Finished` race ahead of output.
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    let _ = tx.send(ProcessEvent::Finished(outcome));
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn drain(mut handle: ProcessHandle) -> (Vec<String>, Vec<String>, ProcessOutcome) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        loop {
            match handle.recv().await {
                Some(ProcessEvent::Stdout(l)) => stdout.push(l),
                Some(ProcessEvent::Stderr(l)) => stderr.push(l),
                Some(ProcessEvent::Finished(outcome)) => return (stdout, stderr, outcome),
                None => panic!("channel closed without Finished event"),
            }
        }
    }

    #[tokio::test]
    async fn captures_stdout_and_success_exit() {
        let handle = spawn("echo", vec!["hello".into()], None);
        let (stdout, _stderr, outcome) = drain(handle).await;
        assert_eq!(stdout, vec!["hello".to_string()]);
        assert_eq!(outcome, ProcessOutcome::Success);
    }

    #[tokio::test]
    async fn captures_nonzero_exit_code() {
        let handle = spawn("sh", vec!["-c".into(), "exit 7".into()], None);
        let (_stdout, _stderr, outcome) = drain(handle).await;
        assert_eq!(outcome, ProcessOutcome::Failure { exit_code: 7 });
    }

    #[tokio::test]
    async fn captures_stderr() {
        let handle = spawn("sh", vec!["-c".into(), "echo oops 1>&2".into()], None);
        let (_stdout, stderr, outcome) = drain(handle).await;
        assert_eq!(stderr, vec!["oops".to_string()]);
        assert_eq!(outcome, ProcessOutcome::Success);
    }

    #[tokio::test]
    async fn times_out_long_running_process() {
        let handle = spawn("sleep", vec!["5".into()], Some(Duration::from_millis(100)));
        let (_stdout, _stderr, outcome) = drain(handle).await;
        assert!(matches!(outcome, ProcessOutcome::TimedOut { .. }));
    }

    #[tokio::test]
    async fn cancellation_kills_process() {
        let mut handle = spawn("sleep", vec!["5".into()], None);
        // Give the process a moment to actually start before cancelling.
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.cancel();
        let (_stdout, _stderr, outcome) = drain(handle).await;
        assert!(matches!(outcome, ProcessOutcome::Cancelled { .. }));
    }

    #[tokio::test]
    async fn spawn_failure_is_reported() {
        let handle = spawn("this-binary-does-not-exist-xyz", vec![], None);
        let (_stdout, _stderr, outcome) = drain(handle).await;
        assert!(matches!(outcome, ProcessOutcome::SpawnFailed { .. }));
    }
}
