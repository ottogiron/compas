//! Worker module — custom poll-loop trigger execution.
//!
//! The worker polls the `executions` table for queued work, enforces per-agent
//! concurrency, and runs backend triggers via `tokio::task::spawn_blocking`.

mod executor;
pub mod guard;
mod loop_runner;

pub use executor::execute_trigger;
pub use guard::{
    acquire_worker_lock, is_worker_alive, WorkerLockGuard, WORKER_HEARTBEAT_MAX_AGE_SECS,
};
pub use loop_runner::WorkerRunner;
