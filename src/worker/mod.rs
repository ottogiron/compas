//! Worker module — apalis-based trigger execution.
//!
//! The worker polls the Jobs table for trigger-worthy work, spawns CLI backend
//! processes, and processes results through a stepped workflow pipeline.

pub mod pipeline;
pub mod trigger;

pub use trigger::{ParsedReply, TriggerJob, TriggerOutput};
