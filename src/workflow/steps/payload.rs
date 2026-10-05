//! Payload push/execution step definitions (CLAUDE.md sections 6.3-6.6).
//!
//! The configured attempt/timeout values are reproduced verbatim and must
//! never be silently changed by this application.

use std::time::Duration;

/// Destination paths (CLAUDE.md 6.3/6.4).
pub const REMOTE_LIB_PATH: &str = "/data/local/tmp/b5q.so";
pub const REMOTE_RUNNER_PATH: &str = "/data/local/tmp/cve-2026-43499-root";
pub const LOG_PATH: &str = "/data/local/tmp/b5q-fzg1-mcast.log";

// Exact configured values from CLAUDE.md section 6.6. Do not change.
pub const SLIDE_SOURCE: &str = "tracefs";
pub const EXPLOIT_ATTEMPTS: u32 = 3;
pub const P0_ATTEMPT_TIMEOUT_SEC: u32 = 115;
pub const EXPLOIT_ATTEMPT_TIMEOUT_SEC: u32 = 600;

/// Reference command (CLAUDE.md 6.5): `chmod 755` on the pushed runner.
pub fn chmod_runner_args() -> [&'static str; 3] {
    ["chmod", "755", REMOTE_RUNNER_PATH]
}

/// Reference command (CLAUDE.md 6.6), reproduced verbatim as the literal
/// device-shell string. This is intentionally a single shell string (see
/// CLAUDE.md section 12: "make that shell boundary explicit") rather than
/// a reconstructed argument list, since it is executed through the
/// device's own shell exactly as the user's manual workflow does.
pub fn run_payload_command() -> String {
    format!(
        "SLIDE_SOURCE={SLIDE_SOURCE} EXPLOIT_ATTEMPTS={EXPLOIT_ATTEMPTS} \
P0_ATTEMPT_TIMEOUT_SEC={P0_ATTEMPT_TIMEOUT_SEC} EXPLOIT_ATTEMPT_TIMEOUT_SEC={EXPLOIT_ATTEMPT_TIMEOUT_SEC} \
{REMOTE_RUNNER_PATH} --run-payload {REMOTE_LIB_PATH} {REMOTE_RUNNER_PATH} {LOG_PATH}"
    )
}

/// A generous TUI-level safety-net timeout wrapped around the whole
/// invocation. This is strictly a crash backstop: it is set comfortably
/// above `EXPLOIT_ATTEMPT_TIMEOUT_SEC * EXPLOIT_ATTEMPTS` so the payload's
/// own internal attempt/timeout handling is always given the chance to
/// finish (or fail) on its own terms first.
pub fn overall_timeout() -> Duration {
    Duration::from_secs(u64::from(EXPLOIT_ATTEMPT_TIMEOUT_SEC) * u64::from(EXPLOIT_ATTEMPTS) + 120)
}

/// Best-effort extraction of the current attempt number from a line of
/// payload stdout, for TUI display ("Attempt 1/3"). Returns `None` if the
/// line doesn't look like an attempt marker. This is purely cosmetic and
/// never affects control flow.
pub fn detect_attempt_number(line: &str) -> Option<u32> {
    let lower = line.to_ascii_lowercase();
    let idx = lower.find("attempt")?;
    lower[idx + "attempt".len()..]
        .trim_start()
        .trim_start_matches(['#', ':'])
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_payload_command_preserves_configured_values() {
        let cmd = run_payload_command();
        assert!(cmd.contains("SLIDE_SOURCE=tracefs"));
        assert!(cmd.contains("EXPLOIT_ATTEMPTS=3"));
        assert!(cmd.contains("P0_ATTEMPT_TIMEOUT_SEC=115"));
        assert!(cmd.contains("EXPLOIT_ATTEMPT_TIMEOUT_SEC=600"));
        assert!(cmd.contains("--run-payload /data/local/tmp/b5q.so /data/local/tmp/cve-2026-43499-root /data/local/tmp/b5q-fzg1-mcast.log"));
    }

    #[test]
    fn detects_attempt_number_from_log_line() {
        assert_eq!(detect_attempt_number("Attempt 2/3 starting"), Some(2));
        assert_eq!(detect_attempt_number("[12:41:10] attempt #1"), Some(1));
        assert_eq!(detect_attempt_number("no attempt marker here at all"), None);
        assert_eq!(detect_attempt_number("unrelated output"), None);
    }
}
