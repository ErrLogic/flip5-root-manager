//! KernelSU push/stage/load/verify step definitions (CLAUDE.md sections
//! 6.7-6.10).

use crate::workflow::steps::payload::REMOTE_RUNNER_PATH;

/// Destination path (CLAUDE.md 6.7). Reproduced exactly as specified,
/// including its "-s25u-" naming even though the artifact itself is the
/// b5q/F731B build — this is the user's existing known-good reference
/// command and this application preserves it verbatim rather than
/// "fixing" what might look like a typo.
pub const REMOTE_KSUD_PATH: &str = "/data/local/tmp/ksud-s25u-kdp";
pub const STAGE_PATH: &str = "/data/local/tmp/.ksud-stage";
pub const BIND_TARGET: &str = "/system/bin/logcat";

/// Reference command (CLAUDE.md 6.8), reproduced verbatim as a single
/// device-shell string (the `-c '...; ...'` argument must stay one shell
/// word so the semicolon is interpreted by the runner's own `-c`, not by
/// the outer device shell).
pub fn stage_command() -> String {
    format!("{REMOTE_RUNNER_PATH} -c 'cp {REMOTE_KSUD_PATH} {STAGE_PATH}; chmod 755 {STAGE_PATH}'")
}

/// Reference command (CLAUDE.md 6.9).
pub fn load_command() -> String {
    format!(
        "{REMOTE_RUNNER_PATH} -c 'mount -o bind {REMOTE_KSUD_PATH} {BIND_TARGET}; \
RUST_LOG=info {BIND_TARGET} late-load --allow-shell --package-name me.weishu.kernelsu'"
    )
}

/// Reference command (CLAUDE.md 6.10).
pub const VERIFY_COMMAND: &str = "cat /proc/modules | grep kernelsu";

/// This step must verify actual device state, not just the exit code of
/// the load step (CLAUDE.md section 19): a `grep` with no match exits
/// non-zero and/or produces empty output, which this treats as "not
/// loaded" rather than inferring anything from the load command's own
/// exit status.
pub fn kernelsu_present(modules_output: &str) -> bool {
    modules_output.lines().any(|line| line.contains("kernelsu"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_command_keeps_semicolon_inside_single_c_argument() {
        let cmd = stage_command();
        assert_eq!(
            cmd,
            "/data/local/tmp/cve-2026-43499-root -c 'cp /data/local/tmp/ksud-s25u-kdp /data/local/tmp/.ksud-stage; chmod 755 /data/local/tmp/.ksud-stage'"
        );
    }

    #[test]
    fn detects_kernelsu_present() {
        assert!(kernelsu_present(
            "kernelsu 16384 0 - Live 0x0000000000000000\n"
        ));
    }

    #[test]
    fn detects_kernelsu_absent_on_empty_output() {
        assert!(!kernelsu_present(""));
    }

    #[test]
    fn detects_kernelsu_absent_when_grep_finds_nothing() {
        assert!(!kernelsu_present("some_other_module 4096 0 - Live 0x0\n"));
    }
}
