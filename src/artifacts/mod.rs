//! Artifact discovery and integrity information for the immutable,
//! read-only payload repository (CLAUDE.md sections 5 and 20).

pub mod checksum;
pub mod discovery;

pub use discovery::{ArtifactInfo, ArtifactKind, ArtifactSlot, GitInfo, PayloadRepository};
