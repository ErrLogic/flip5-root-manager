//! Reference-command definitions for each workflow step (CLAUDE.md
//! section 6), kept separate from `runner.rs`'s orchestration logic so the
//! exact command text/semantics being preserved is easy to audit in one
//! place per domain.

pub mod cleanup;
pub mod kernelsu;
pub mod payload;
pub mod root;
