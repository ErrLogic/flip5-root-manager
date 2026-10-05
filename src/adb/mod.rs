//! ADB abstraction layer. See [`client::AdbClient`] for the trait every
//! other module depends on, [`mock::MockAdbClient`] for the unit-test
//! double, and [`dryrun::DryRunAdbClient`] for the TUI's Dry Run mode.

pub mod client;
pub mod dryrun;
pub mod mock;

pub use client::{
    AdbClient, AdbError, CapturedOutput, CommandKind, DescribedCommand, DeviceEntry, RealAdbClient,
};
pub use dryrun::{DryRunAdbClient, DryRunScenario};
pub use mock::MockAdbClient;
