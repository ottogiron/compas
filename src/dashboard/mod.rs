//! TUI dashboard for aster-orch.
//!
//! Reads SQLite directly (no MCP, no network). Intended to be launched as:
//!   `aster_orch dashboard --config .aster-orch/config.yaml`

pub mod app;
pub mod views;
