//! Model/build verification against the known Flip5 target (CLAUDE.md
//! section 4). This must run — and pass — before any device-changing
//! operation. Never bypass it.

use thiserror::Error;

use crate::adb::{AdbClient, AdbError};

/// Reference command (CLAUDE.md 6.1): `adb shell "getprop ro.product.model"`
pub const EXPECTED_MODEL: &str = "SM-F731B";

/// Reference command (CLAUDE.md 6.2): `adb shell "getprop ro.build.display.id"`
pub const EXPECTED_BUILD: &str = "BP4A.251205.006.F731BXXS7GZG1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDevice {
    pub serial: String,
    pub model: String,
    pub build: String,
}

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("device mismatch: model is '{actual}', expected '{expected}'")]
    ModelMismatch { expected: String, actual: String },
    #[error("device mismatch: build is '{actual}', expected '{expected}'")]
    BuildMismatch { expected: String, actual: String },
    #[error("device disconnected during verification")]
    Disconnected,
    #[error("adb error: {0}")]
    Adb(#[from] AdbError),
}

/// Verifies the connected device's model and build against the known
/// compatible Flip5 target. Returns `DeviceError::ModelMismatch` /
/// `BuildMismatch` (CLAUDE.md: `DeviceMismatch`) if either check fails.
///
/// This performs real state verification (reads the live `getprop` value)
/// rather than trusting any cached/persisted assumption, per CLAUDE.md
/// section 18/24.
pub async fn verify_device(
    client: &dyn AdbClient,
    serial: &str,
) -> Result<VerifiedDevice, DeviceError> {
    let model_out = client
        .shell_capture(serial, &["getprop", "ro.product.model"])
        .await
        .map_err(|e| match e {
            AdbError::DeviceDisconnected => DeviceError::Disconnected,
            other => DeviceError::Adb(other),
        })?;
    let model = model_out.stdout_trimmed().to_string();
    if model != EXPECTED_MODEL {
        return Err(DeviceError::ModelMismatch {
            expected: EXPECTED_MODEL.to_string(),
            actual: model,
        });
    }

    let build_out = client
        .shell_capture(serial, &["getprop", "ro.build.display.id"])
        .await
        .map_err(|e| match e {
            AdbError::DeviceDisconnected => DeviceError::Disconnected,
            other => DeviceError::Adb(other),
        })?;
    let build = build_out.stdout_trimmed().to_string();
    if build != EXPECTED_BUILD {
        return Err(DeviceError::BuildMismatch {
            expected: EXPECTED_BUILD.to_string(),
            actual: build,
        });
    }

    Ok(VerifiedDevice {
        serial: serial.to_string(),
        model,
        build,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::mock::{MockAdbClient, MockScenario};

    #[tokio::test]
    async fn verifies_matching_device() {
        let client = MockAdbClient::scenario(MockScenario::DeviceConnected);
        let result = verify_device(&client, "R58N00000XX").await.unwrap();
        assert_eq!(result.model, EXPECTED_MODEL);
        assert_eq!(result.build, EXPECTED_BUILD);
    }

    #[tokio::test]
    async fn rejects_wrong_model() {
        let client = MockAdbClient::scenario(MockScenario::DeviceMismatch);
        let err = verify_device(&client, "R58N00000XX").await.unwrap_err();
        assert!(matches!(err, DeviceError::ModelMismatch { .. }));
    }

    #[tokio::test]
    async fn rejects_wrong_build() {
        let client = MockAdbClient::scenario(MockScenario::BuildMismatch);
        let err = verify_device(&client, "R58N00000XX").await.unwrap_err();
        assert!(matches!(err, DeviceError::BuildMismatch { .. }));
    }

    #[tokio::test]
    async fn reports_disconnect() {
        let client = MockAdbClient::scenario(MockScenario::DeviceDisconnect);
        let err = verify_device(&client, "R58N00000XX").await.unwrap_err();
        assert!(matches!(err, DeviceError::Disconnected));
    }
}
