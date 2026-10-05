//! `hybrid_mount` configuration inspection and idempotent update
//! (CLAUDE.md sections 6.13, 6.14, 23).
//!
//! Per CLAUDE.md section 6.14, this intentionally does **not** reproduce
//! the user's original `awk`-based text manipulation. Instead it parses
//! the remote TOML file with a real TOML parser, modifies only the rule
//! that needs to change, and never touches the file if the rule is already
//! correct. If the file does not parse as TOML, the operation stops and
//! reports `ConfigurationInvalid` rather than guessing or overwriting it.

use std::path::Path;

use thiserror::Error;

use crate::adb::{AdbClient, AdbError};

/// Reference location (CLAUDE.md 6.13/6.14).
pub const REMOTE_CONFIG_PATH: &str = "/data/adb/modules/hybrid_mount/config.toml";
/// Host-side staging path used for the atomic replace, mirroring the
/// temp-file-then-`mv` pattern of the user's original manual command.
pub const REMOTE_STAGING_PATH: &str = "/data/local/tmp/config.tmp";

pub const RULE_NAME: &str = "ViPER4Android-RE";
pub const EXPECTED_MODE: &str = "magic";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration is not valid TOML: {0}")]
    Parse(String),
    #[error("'rules' key exists but is not a table")]
    RulesNotATable,
    #[error("'rules.{0}' exists but is not a table")]
    RuleNotATable(String),
    #[error("'rules.{0}.default_mode' exists but is not a string")]
    DefaultModeNotAString(String),
    #[error("adb error: {0}")]
    Adb(#[from] AdbError),
    #[error("failed to verify configuration after write: rule was not applied")]
    VerificationFailed,
    #[error("local staging error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleState {
    Missing,
    Present(String),
}

/// A parsed `hybrid_mount` configuration document, kept as a generic TOML
/// value so unrelated rules/keys are preserved untouched.
#[derive(Debug, Clone)]
pub struct ParsedConfig {
    value: toml::Value,
}

impl ParsedConfig {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let value: toml::Value =
            toml::from_str(raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
        if value.as_table().is_none() {
            return Err(ConfigError::Parse(
                "document root is not a table".to_string(),
            ));
        }
        Ok(Self { value })
    }

    pub fn rule_state(&self, rule_name: &str) -> Result<RuleState, ConfigError> {
        let table = self
            .value
            .as_table()
            .expect("root validated as table in parse()");
        let Some(rules) = table.get("rules") else {
            return Ok(RuleState::Missing);
        };
        let rules_table = rules.as_table().ok_or(ConfigError::RulesNotATable)?;
        let Some(rule) = rules_table.get(rule_name) else {
            return Ok(RuleState::Missing);
        };
        let rule_table = rule
            .as_table()
            .ok_or_else(|| ConfigError::RuleNotATable(rule_name.to_string()))?;
        match rule_table.get("default_mode") {
            None => Ok(RuleState::Missing),
            Some(v) => v
                .as_str()
                .map(|s| RuleState::Present(s.to_string()))
                .ok_or_else(|| ConfigError::DefaultModeNotAString(rule_name.to_string())),
        }
    }

    /// Ensures `rules.<rule_name>.default_mode == expected_value`, creating
    /// the `rules` table and/or the rule sub-table only if they don't
    /// already exist. Returns `Ok(false)` without modifying anything if the
    /// rule already has the expected value (idempotent).
    pub fn ensure_rule(
        &mut self,
        rule_name: &str,
        expected_value: &str,
    ) -> Result<bool, ConfigError> {
        if self.rule_state(rule_name)? == RuleState::Present(expected_value.to_string()) {
            return Ok(false);
        }
        let table = self
            .value
            .as_table_mut()
            .expect("root validated as table in parse()");
        let rules_entry = table
            .entry("rules".to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()));
        let rules_table = rules_entry
            .as_table_mut()
            .ok_or(ConfigError::RulesNotATable)?;
        let rule_entry = rules_table
            .entry(rule_name.to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()));
        let rule_table = rule_entry
            .as_table_mut()
            .ok_or_else(|| ConfigError::RuleNotATable(rule_name.to_string()))?;
        rule_table.insert(
            "default_mode".to_string(),
            toml::Value::String(expected_value.to_string()),
        );
        Ok(true)
    }

    pub fn to_toml_string(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(&self.value).map_err(|e| ConfigError::Parse(e.to_string()))
    }
}

/// Outcome of inspecting/applying the hybrid_mount rule against a live
/// device, distinguishing "already correct" from "changed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    AlreadyCorrect,
    Updated,
}

/// Read-only presence check: is the `hybrid_mount` module actually
/// installed/enabled on this device? `hybrid_mount`/ViPER4Android-RE is a
/// personal, environment-specific integration, not something every user
/// has — this must always be checked explicitly rather than assumed, so
/// the optional integration can be skipped cleanly (not treated as an
/// error) when it isn't present.
pub async fn is_installed(client: &dyn AdbClient, serial: &str) -> Result<bool, ConfigError> {
    let raw_command =
        format!("su -c \"test -f {REMOTE_CONFIG_PATH} && echo FOUND || echo MISSING\"");
    let out = client.shell_capture_raw(serial, &raw_command).await?;
    Ok(out.stdout.trim() == "FOUND")
}

/// CLAUDE.md 6.13 — a quick, literal read-only preview of the tail of the
/// remote config, exactly as the user's manual workflow inspects it before
/// touching anything.
pub async fn peek_tail(client: &dyn AdbClient, serial: &str) -> Result<String, ConfigError> {
    let raw_command = format!("su -c \"tail -n 6 {REMOTE_CONFIG_PATH}\"");
    let out = client.shell_capture_raw(serial, &raw_command).await?;
    Ok(out.stdout)
}

/// Reads the full remote configuration file (required to parse and modify
/// it safely; the 6.13 tail preview is for display only).
async fn read_full_config(client: &dyn AdbClient, serial: &str) -> Result<String, ConfigError> {
    let raw_command = format!("su -c \"cat {REMOTE_CONFIG_PATH}\"");
    let out = client.shell_capture_raw(serial, &raw_command).await?;
    Ok(out.stdout)
}

/// Implements CLAUDE.md section 23 end-to-end:
/// read -> parse -> check -> modify only if necessary -> write safely ->
/// re-read -> verify. Stops and returns `ConfigError::Parse` without
/// writing anything if the remote file is not valid TOML.
pub async fn inspect_and_apply(
    client: &dyn AdbClient,
    serial: &str,
    staging_dir: &Path,
) -> Result<ApplyOutcome, ConfigError> {
    let raw = read_full_config(client, serial).await?;
    let mut parsed = ParsedConfig::parse(&raw)?;

    if !parsed.ensure_rule(RULE_NAME, EXPECTED_MODE)? {
        return Ok(ApplyOutcome::AlreadyCorrect);
    }

    let new_contents = parsed.to_toml_string()?;

    std::fs::create_dir_all(staging_dir)?;
    let local_tmp = staging_dir.join("hybrid_mount_config.toml");
    std::fs::write(&local_tmp, &new_contents)?;

    client.push(serial, &local_tmp, REMOTE_STAGING_PATH).await?;
    let mv_command = format!("su -c \"mv {REMOTE_STAGING_PATH} {REMOTE_CONFIG_PATH}\"");
    client.shell_capture_raw(serial, &mv_command).await?;

    let _ = std::fs::remove_file(&local_tmp);

    // Re-read and verify per section 23 steps 7-8.
    let reread = read_full_config(client, serial).await?;
    let verified = ParsedConfig::parse(&reread)?;
    if verified.rule_state(RULE_NAME)? != RuleState::Present(EXPECTED_MODE.to_string()) {
        return Err(ConfigError::VerificationFailed);
    }

    Ok(ApplyOutcome::Updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::mock::MockAdbClient;

    #[test]
    fn rule_missing_when_rules_table_absent() {
        let cfg = ParsedConfig::parse("").unwrap();
        assert_eq!(cfg.rule_state(RULE_NAME).unwrap(), RuleState::Missing);
    }

    #[test]
    fn rule_missing_when_other_rules_present() {
        let cfg = ParsedConfig::parse("[rules.other]\ndefault_mode = \"normal\"\n").unwrap();
        assert_eq!(cfg.rule_state(RULE_NAME).unwrap(), RuleState::Missing);
    }

    #[test]
    fn rule_detected_when_present() {
        let cfg =
            ParsedConfig::parse("[rules.ViPER4Android-RE]\ndefault_mode = \"magic\"\n").unwrap();
        assert_eq!(
            cfg.rule_state(RULE_NAME).unwrap(),
            RuleState::Present("magic".to_string())
        );
    }

    #[test]
    fn ensure_rule_is_idempotent_when_already_correct() {
        let mut cfg =
            ParsedConfig::parse("[rules.ViPER4Android-RE]\ndefault_mode = \"magic\"\n").unwrap();
        let changed = cfg.ensure_rule(RULE_NAME, EXPECTED_MODE).unwrap();
        assert!(!changed);
    }

    #[test]
    fn ensure_rule_adds_missing_rule_without_duplicating() {
        let mut cfg = ParsedConfig::parse("[rules.other]\ndefault_mode = \"normal\"\n").unwrap();
        let changed = cfg.ensure_rule(RULE_NAME, EXPECTED_MODE).unwrap();
        assert!(changed);
        assert_eq!(
            cfg.rule_state(RULE_NAME).unwrap(),
            RuleState::Present("magic".to_string())
        );
        assert_eq!(
            cfg.rule_state("other").unwrap(),
            RuleState::Present("normal".to_string())
        );

        // Applying again must not duplicate or change anything further.
        let changed_again = cfg.ensure_rule(RULE_NAME, EXPECTED_MODE).unwrap();
        assert!(!changed_again);
    }

    #[test]
    fn malformed_toml_is_reported_and_never_parsed_as_partial() {
        let err = ParsedConfig::parse("not [ valid toml =").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[tokio::test]
    async fn apply_is_noop_against_already_correct_device_config() {
        let client = MockAdbClient::new(crate::adb::mock::MockState {
            hybrid_mount_config: "[rules.ViPER4Android-RE]\ndefault_mode = \"magic\"\n".to_string(),
            ..Default::default()
        });
        let staging = std::env::temp_dir().join(format!("flip5-hm-test-{}-a", std::process::id()));
        let outcome = inspect_and_apply(&client, "R58N00000XX", &staging)
            .await
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::AlreadyCorrect);
        let _ = std::fs::remove_dir_all(&staging);
    }

    #[tokio::test]
    async fn apply_updates_and_verifies_when_rule_missing() {
        let client = MockAdbClient::new(crate::adb::mock::MockState {
            hybrid_mount_config: "[rules.other]\ndefault_mode = \"normal\"\n".to_string(),
            ..Default::default()
        });
        let staging = std::env::temp_dir().join(format!("flip5-hm-test-{}-b", std::process::id()));
        let outcome = inspect_and_apply(&client, "R58N00000XX", &staging)
            .await
            .unwrap();
        assert_eq!(outcome, ApplyOutcome::Updated);
        let _ = std::fs::remove_dir_all(&staging);
    }

    #[tokio::test]
    async fn apply_stops_on_invalid_remote_config() {
        let client = MockAdbClient::new(crate::adb::mock::MockState {
            hybrid_mount_config: "not [ valid toml =".to_string(),
            ..Default::default()
        });
        let staging = std::env::temp_dir().join(format!("flip5-hm-test-{}-c", std::process::id()));
        let err = inspect_and_apply(&client, "R58N00000XX", &staging)
            .await
            .unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
        let _ = std::fs::remove_dir_all(&staging);
    }
}
