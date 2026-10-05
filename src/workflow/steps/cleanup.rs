//! Temporary mount cleanup step (CLAUDE.md section 6.12).
//!
//! Cleanup is an explicit workflow step with its own failure state
//! (`CleanupFailed`); a failure here must never be silently ignored.

/// Reference command (CLAUDE.md 6.12).
pub const CLEANUP_COMMAND: &str = "su -c 'umount /system/bin/logcat'";
