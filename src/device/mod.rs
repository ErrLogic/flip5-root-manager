//! Device detection and compatibility verification for the known Flip5
//! target (CLAUDE.md section 4).

pub mod detector;
pub mod verifier;

pub use detector::detect_single_device;
pub use verifier::{DeviceError, EXPECTED_BUILD, EXPECTED_MODEL, VerifiedDevice, verify_device};
