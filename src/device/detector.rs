//! Picks the single device this application should operate on.
//!
//! Detection is deliberately strict: if zero or more than one device is
//! attached, we stop rather than guess.

use thiserror::Error;

use crate::adb::{AdbClient, AdbError, DeviceEntry};

#[derive(Debug, Error)]
pub enum DetectError {
    #[error("no device connected")]
    NoDevice,
    #[error("multiple devices connected: {0:?}")]
    MultipleDevices(Vec<String>),
    #[error("device present but not ready (state: {0})")]
    NotReady(String),
    #[error("adb error: {0}")]
    Adb(#[from] AdbError),
}

/// Returns the single connected, ready device, or an error describing why
/// none could be unambiguously selected.
pub async fn detect_single_device(client: &dyn AdbClient) -> Result<DeviceEntry, DetectError> {
    let devices = client.devices().await?;
    match devices.len() {
        0 => Err(DetectError::NoDevice),
        1 => {
            let device = devices.into_iter().next().unwrap();
            if device.is_ready() {
                Ok(device)
            } else {
                Err(DetectError::NotReady(device.state))
            }
        }
        _ => Err(DetectError::MultipleDevices(
            devices.into_iter().map(|d| d.serial).collect(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adb::mock::{MockAdbClient, MockScenario};

    #[tokio::test]
    async fn detects_single_ready_device() {
        let client = MockAdbClient::scenario(MockScenario::DeviceConnected);
        let device = detect_single_device(&client).await.unwrap();
        assert_eq!(device.state, "device");
    }

    #[tokio::test]
    async fn no_device_is_an_error() {
        let client = MockAdbClient::scenario(MockScenario::DeviceDisconnect);
        let err = detect_single_device(&client).await.unwrap_err();
        assert!(matches!(err, DetectError::NoDevice));
    }
}
