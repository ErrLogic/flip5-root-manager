//! Host-level process execution abstraction.
//!
//! Everything in this module operates on the *host* (WSL/Linux) side. It
//! knows nothing about ADB or Android; [`crate::adb`] builds on top of it.

pub mod cancel;
pub mod process;

pub use cancel::CancelToken;
pub use process::{ProcessEvent, ProcessHandle, ProcessOutcome, spawn};
