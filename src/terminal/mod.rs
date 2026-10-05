//! Terminal output panel model (CLAUDE.md section 11).

pub mod buffer;
pub mod scroll;

pub use buffer::{LineKind, LogBuffer, LogLine};
pub use scroll::ScrollState;
