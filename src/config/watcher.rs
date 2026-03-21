//! Config live-reload: watch the config file for changes and
//! atomically swap the active `OrchestratorConfig` via `ArcSwap`.
//!
//! Usage:
//!   let watcher = ConfigWatcher::start(path, initial_config)?;
//!   // Consumers read the latest snapshot:
//!   let cfg = watcher.load();

use arc_swap::ArcSwap;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::load_config;
use super::types::OrchestratorConfig;

/// Debounce window — file editors often trigger multiple events per save.
const DEBOUNCE_DURATION: Duration = Duration::from_millis(500);

/// Shared, atomically swappable config handle.
///
/// Cheap to clone (wraps `Arc<ArcSwap<…>>`). All clones observe the same
/// underlying config and see updates after a successful reload.
#[derive(Clone)]
pub struct ConfigHandle {
    inner: Arc<ArcSwap<OrchestratorConfig>>,
}

impl ConfigHandle {
    /// Create a handle from an already-loaded config (no file watcher).
    pub fn new(config: OrchestratorConfig) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(config)),
        }
    }

    /// Read the current config snapshot.
    ///
    /// Returns a guard that `Deref`s to `Arc<OrchestratorConfig>`.
    /// Cheap and wait-free; callers should take one snapshot per logical
    /// operation for consistency.
    pub fn load(&self) -> arc_swap::Guard<Arc<OrchestratorConfig>> {
        self.inner.load()
    }

    /// Atomically replace the config (used by the watcher and tests).
    fn store(&self, config: OrchestratorConfig) {
        self.inner.store(Arc::new(config));
    }
}

/// Start watching a config file and auto-reload on changes.
///
/// Returns a `ConfigHandle` that always reflects the latest valid config.
/// If the file changes to invalid YAML or fails validation, the previous
/// config is retained and a warning is logged.
///
/// # Errors
/// Returns an error if the initial file watcher cannot be set up.
pub fn start_watching(
    config_path: PathBuf,
    initial_config: OrchestratorConfig,
) -> crate::error::Result<ConfigHandle> {
    let handle = ConfigHandle::new(initial_config);
    let watcher_handle = handle.clone();

    // Canonicalize the path so we can match events reliably.
    let canonical = std::fs::canonicalize(&config_path).unwrap_or(config_path.clone());

    // Watch the parent directory — some editors replace the file (write-tmp + rename)
    // and watching the file directly can lose the watch after rename.
    let watch_dir = canonical
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let mut debouncer = new_debouncer(
        DEBOUNCE_DURATION,
        move |events: Result<Vec<notify_debouncer_mini::DebouncedEvent>, _>| {
            let events = match events {
                Ok(evts) => evts,
                Err(e) => {
                    tracing::warn!(error = %e, "config file watcher error");
                    return;
                }
            };

            // Check if any event targets our config file.
            let config_changed = events.iter().any(|e| {
                e.kind == DebouncedEventKind::Any && {
                    let event_canon =
                        std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone());
                    event_canon == canonical
                }
            });

            if !config_changed {
                return;
            }

            // Reload: parse + validate. On failure, keep old config.
            match load_config(&config_path) {
                Ok(new_config) => {
                    let old = watcher_handle.load();
                    warn_restart_only_changes(&old, &new_config);
                    watcher_handle.store(new_config);
                    tracing::info!("config reloaded from {}", config_path.display());
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %config_path.display(),
                        "config reload failed; keeping previous config"
                    );
                }
            }
        },
    )
    .map_err(|e| {
        crate::error::OrchestratorError::Other(format!("failed to start config watcher: {e}"))
    })?;

    debouncer
        .watcher()
        .watch(
            &watch_dir,
            notify_debouncer_mini::notify::RecursiveMode::NonRecursive,
        )
        .map_err(|e| {
            crate::error::OrchestratorError::Other(format!(
                "failed to watch config directory {}: {e}",
                watch_dir.display()
            ))
        })?;

    // Leak the debouncer intentionally — it must live for the process lifetime.
    // The background thread it owns drives the watcher; dropping it would stop
    // file monitoring. This is the standard pattern for process-scoped watchers.
    std::mem::forget(debouncer);

    tracing::info!(
        path = %watch_dir.display(),
        "config file watcher started"
    );

    Ok(handle)
}

/// Log warnings for config fields that cannot be live-reloaded.
///
/// These fields are consumed at startup (DB pool, backend workdir, semaphore)
/// and require a process restart to take effect.
fn warn_restart_only_changes(old: &OrchestratorConfig, new: &OrchestratorConfig) {
    if old.default_workdir != new.default_workdir {
        tracing::warn!(
            old = %old.default_workdir.display(),
            new = %new.default_workdir.display(),
            "default_workdir changed — requires restart to take effect"
        );
    }
    if old.state_dir != new.state_dir {
        tracing::warn!(
            old = %old.state_dir.display(),
            new = %new.state_dir.display(),
            "state_dir changed — requires restart to take effect"
        );
    }
    if old.database.max_connections != new.database.max_connections
        || old.database.min_connections != new.database.min_connections
        || old.database.acquire_timeout_ms != new.database.acquire_timeout_ms
    {
        tracing::warn!("database pool settings changed — requires restart to take effect");
    }
    if old.effective_max_concurrent_triggers() != new.effective_max_concurrent_triggers() {
        tracing::warn!(
            old = old.effective_max_concurrent_triggers(),
            new = new.effective_max_concurrent_triggers(),
            "max_concurrent_triggers changed — requires restart to take effect"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config(default_workdir: &Path) -> OrchestratorConfig {
        use crate::config::load_config_from_str;
        load_config_from_str(&format!(
            r#"
default_workdir: {}
state_dir: /tmp/compas-test-state
agents:
  - alias: test-agent
    backend: stub
"#,
            default_workdir.display()
        ))
        .unwrap()
    }

    #[test]
    fn test_config_handle_returns_initial_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = minimal_config(dir.path());
        let handle = ConfigHandle::new(config);
        let loaded = handle.load();
        assert_eq!(loaded.agents.len(), 1);
        assert_eq!(loaded.agents[0].alias, "test-agent");
    }

    #[test]
    fn test_config_handle_store_swaps_config() {
        let dir = tempfile::tempdir().unwrap();
        let config1 = minimal_config(dir.path());
        let handle = ConfigHandle::new(config1);

        // Verify initial
        assert_eq!(handle.load().agents[0].alias, "test-agent");

        // Swap in a new config with a different agent alias
        let mut config2 = minimal_config(dir.path());
        config2.agents[0].alias = "new-agent".to_string();
        handle.store(config2);

        // Verify swap
        assert_eq!(handle.load().agents[0].alias, "new-agent");
    }

    #[test]
    fn test_config_handle_clone_shares_state() {
        let dir = tempfile::tempdir().unwrap();
        let config = minimal_config(dir.path());
        let handle1 = ConfigHandle::new(config);
        let handle2 = handle1.clone();

        // Mutate through handle1
        let mut new_config = minimal_config(dir.path());
        new_config.poll_interval_secs = 42;
        handle1.store(new_config);

        // Observe through handle2
        assert_eq!(handle2.load().poll_interval_secs, 42);
    }

    #[test]
    fn test_warn_restart_only_changes_no_warnings_when_equal() {
        // Just verify it doesn't panic — warnings go to tracing (not captured here).
        let dir = tempfile::tempdir().unwrap();
        let config = minimal_config(dir.path());
        warn_restart_only_changes(&config, &config);
    }

    #[test]
    fn test_start_watching_with_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let default_workdir = dir.path().join("repo");
        std::fs::create_dir_all(&default_workdir).unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            format!(
                r#"
default_workdir: {}
state_dir: /tmp/compas-watcher-test
agents:
  - alias: watched
    backend: stub
"#,
                default_workdir.display()
            ),
        )
        .unwrap();

        let config = load_config(&config_path).unwrap();
        let handle = start_watching(config_path, config).unwrap();
        assert_eq!(handle.load().agents[0].alias, "watched");
    }
}
