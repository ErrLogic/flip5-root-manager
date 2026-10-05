//! Root verification step (CLAUDE.md section 6.11).
//!
//! Success is never inferred from the exit code alone: the returned
//! identity must actually demonstrate `uid=0(root)`.

/// Reference command (CLAUDE.md 6.11).
pub const VERIFY_ROOT_COMMAND: &str = "su -c 'id'";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub raw: String,
    pub is_root: bool,
}

/// Parses the output of `su -c 'id'` and determines whether it actually
/// demonstrates root privilege (`uid=0(root)`), rather than trusting the
/// command's exit code alone (CLAUDE.md section 19).
///
/// Only the first non-empty line is treated as the identity: `id`'s real
/// output is always exactly one line, so any further lines (e.g. extra
/// narration a given `AdbClient` backend might include in the same
/// captured stdout) must never bleed into the parsed/displayed identity.
pub fn parse_identity(output: &str) -> Identity {
    let raw = output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    let is_root = raw.contains("uid=0(root)");
    Identity { raw, is_root }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_root_identity() {
        let id = parse_identity("uid=0(root) gid=0(root) groups=0(root)\n");
        assert!(id.is_root);
    }

    #[test]
    fn rejects_non_root_identity() {
        let id = parse_identity("uid=2000(shell) gid=2000(shell)\n");
        assert!(!id.is_root);
    }

    #[test]
    fn rejects_empty_output() {
        let id = parse_identity("");
        assert!(!id.is_root);
    }

    #[test]
    fn ignores_extra_lines_after_the_identity_line() {
        // Regression: a captured stdout blob that contains the `id`
        // output followed by unrelated narration (e.g. Dry Run's
        // "Simulated root verification passed." line, which lives in the
        // same `CapturedOutput.stdout`) must not corrupt the parsed/
        // displayed identity by gluing the two together.
        let id = parse_identity(
            "uid=0(root) gid=0(root) groups=0(root) (DRY RUN)\n[DRY RUN] Simulated root verification passed.\n",
        );
        assert!(id.is_root);
        assert_eq!(id.raw, "uid=0(root) gid=0(root) groups=0(root) (DRY RUN)");
    }
}
