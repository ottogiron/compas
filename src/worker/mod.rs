//! Worker module — custom poll-loop trigger execution.
//!
//! The worker polls the `executions` table for queued work, enforces per-agent
//! concurrency, and runs backend triggers via `tokio::task::spawn_blocking`.

mod executor;
mod loop_runner;

pub use executor::execute_trigger;
pub use loop_runner::WorkerRunner;
