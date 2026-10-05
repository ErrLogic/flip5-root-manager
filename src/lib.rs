//! Flip5 Root Manager — a local Rust TUI that orchestrates a known,
//! user-provided ADB workflow for a specific Samsung Galaxy Z Flip5
//! device. See `CLAUDE.md` for the full specification this implementation
//! follows.
//!
//! This crate is organized as:
//! `device` + `artifacts` (read-only inputs) -> `workflow` (the state
//! machine and step orchestration, the core of the application) ->
//! `adb`/`executor` (the transport layer) with `tui` as the presentation
//! layer on top. See CLAUDE.md section 32.

pub mod adb;
pub mod app;
pub mod artifacts;
pub mod config;
pub mod device;
pub mod executor;
pub mod terminal;
pub mod tui;
pub mod workflow;
