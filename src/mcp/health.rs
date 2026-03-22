//! orch_health and orch_diagnose implementations.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::backend::PingResult;
use crate::model::agent::Agent;
use crate::store::ThreadStatus;

// ---------------------------------------------------------------------------
// PingCache — per-agent TTL cache for ping results
// ---------------------------------------------------------------------------

/// Cached ping result with timestamp.
struct CachedPing {
    result: PingResult,
    cached_at: Instant,
}

/// Thread-safe cache for backend ping results.
///
/// Each agent alias maps to a `CachedPing`. Entries within the configured TTL
/// are returned immediately; expired or missing entries trigger a fresh ping.
pub struct PingCache {
    entries: Mutex<HashMap<String, CachedPing>>,
}

impl PingCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Return a cached result if it exists and is within TTL.
    fn get(&self, alias: &str, ttl: Duration) -> Option<PingResult> {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(alias).and_then(|entry| {
            if entry.cached_at.elapsed() < ttl {
                Some(entry.result.clone())
            } else {
                None
            }
        })
    }

    /// Insert or overwrite a cache entry.
    fn set(&self, alias: String, result: PingResult) {
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(
            alias,
            CachedPing {
                result,
                cached_at: Instant::now(),
            },
        );
    }
}

impl OrchestratorMcpServer {
    // ── orch_health ──────────────────────────────────────────────────────

    pub async fn health_impl(
        &self,
        params: HealthParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        #[derive(Serialize)]
        struct AgentHealth {
            alias: String,
            backend: String,
            ping_alive: bool,
            ping_latency_ms: u64,
            ping_detail: Option<String>,
            cached: bool,
            circuit_state: String,
            circuit_failures: u32,
        }

        #[derive(Serialize)]
        struct HealthReport {
            worker_heartbeat: Option<HeartbeatInfo>,
            queue_depth: i64,
            agents: Vec<AgentHealth>,
        }

        #[derive(Serialize)]
        struct HeartbeatInfo {
            worker_id: String,
            last_beat_at: i64,
            started_at: i64,
            version: Option<String>,
        }

        // Worker heartbeat
        let heartbeat = match self.store.latest_heartbeat().await {
            Ok(Some((id, beat, start, ver))) => Some(HeartbeatInfo {
                worker_id: id,
                last_beat_at: beat,
                started_at: start,
                version: ver,
            }),
            _ => None,
        };

        let queue_depth = self.store.queue_depth().await.unwrap_or(0);

        // Snapshot live config for this request.
        let config = self.config.load();

        // Filter agents if alias specified
        let agents_to_check: Vec<_> = if let Some(ref alias) = params.alias {
            config.agents.iter().filter(|a| a.alias == *alias).collect()
        } else {
            config.agents.iter().collect()
        };

        let ping_timeout = config.orchestration.ping_timeout_secs;
        let cache_ttl = Duration::from_secs(config.orchestration.ping_cache_ttl_secs);

        // Fetch circuit breaker states for all backends.
        let cb_states: HashMap<String, (String, u32)> = self
            .store
            .get_circuit_breaker_states()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(backend, state, failures)| (backend, (state, failures)))
            .collect();

        // Partition agents into cache-hit vs cache-miss.
        let mut agent_health: Vec<AgentHealth> = Vec::with_capacity(agents_to_check.len());
        let mut need_ping: Vec<usize> = Vec::new(); // indices into agents_to_check

        for (i, agent_cfg) in agents_to_check.iter().enumerate() {
            let (circuit_state, circuit_failures) = cb_states
                .get(&agent_cfg.backend)
                .cloned()
                .unwrap_or_else(|| ("closed".to_string(), 0));

            if let Some(cached) = self.ping_cache.get(&agent_cfg.alias, cache_ttl) {
                agent_health.push(AgentHealth {
                    alias: agent_cfg.alias.clone(),
                    backend: agent_cfg.backend.clone(),
                    ping_alive: cached.alive,
                    ping_latency_ms: cached.latency_ms,
                    ping_detail: cached.detail,
                    cached: true,
                    circuit_state,
                    circuit_failures,
                });
            } else {
                // Placeholder; will be filled after parallel pings.
                agent_health.push(AgentHealth {
                    alias: agent_cfg.alias.clone(),
                    backend: agent_cfg.backend.clone(),
                    ping_alive: false,
                    ping_latency_ms: 0,
                    ping_detail: None,
                    cached: false,
                    circuit_state,
                    circuit_failures,
                });
                need_ping.push(i);
            }
        }

        // Ping all cache-miss agents in parallel using JoinSet.
        if !need_ping.is_empty() {
            let mut join_set = tokio::task::JoinSet::new();

            for &idx in &need_ping {
                let agent_cfg = agents_to_check[idx];
                let agent = Agent {
                    alias: agent_cfg.alias.clone(),
                    backend: agent_cfg.backend.clone(),
                    model: agent_cfg.model.clone(),
                    prompt: agent_cfg.prompt.clone(),
                    prompt_file: agent_cfg.prompt_file.clone(),
                    timeout_secs: agent_cfg.timeout_secs,
                    backend_args: agent_cfg.backend_args.clone(),
                    env: agent_cfg.env.clone(),
                    log_path: None,
                    execution_workdir: agent_cfg.workdir.clone(),
                };

                let backend_result = self.backend_registry.get(agent_cfg);
                let ping_timeout_secs = ping_timeout;

                join_set.spawn(async move {
                    let ping = match backend_result {
                        Ok(backend) => backend.ping(&agent, ping_timeout_secs).await,
                        Err(e) => PingResult {
                            alive: false,
                            latency_ms: 0,
                            detail: Some(format!("backend not found: {}", e)),
                        },
                    };
                    (idx, ping)
                });
            }

            // Collect all results.
            while let Some(result) = join_set.join_next().await {
                match result {
                    Ok((idx, ping)) => {
                        let alias = agents_to_check[idx].alias.clone();

                        // Update the placeholder entry.
                        agent_health[idx].ping_alive = ping.alive;
                        agent_health[idx].ping_latency_ms = ping.latency_ms;
                        agent_health[idx].ping_detail = ping.detail.clone();

                        // Populate cache.
                        self.ping_cache.set(alias, ping);
                    }
                    Err(join_err) => {
                        tracing::warn!("ping task panicked: {}", join_err);
                    }
                }
            }
        }

        Ok(json_text(&HealthReport {
            worker_heartbeat: heartbeat,
            queue_depth,
            agents: agent_health,
        }))
    }

    // ── orch_diagnose ────────────────────────────────────────────────────

    pub async fn diagnose_impl(
        &self,
        params: DiagnoseParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let thread = match self.store.get_thread(&params.thread_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                return Ok(err_text(format!("thread not found: {}", params.thread_id)));
            }
            Err(e) => return Ok(err_text(format!("lookup failed: {}", e))),
        };

        let messages = self
            .store
            .get_thread_messages(&params.thread_id)
            .await
            .unwrap_or_default();

        let executions = self
            .store
            .get_thread_executions(&params.thread_id)
            .await
            .unwrap_or_default();

        let latest_exec = executions.last();

        // Determine blockers and suggestions
        let mut blockers = Vec::new();
        let mut suggestions = Vec::new();

        let status: Result<ThreadStatus, _> = thread.status.parse();

        if let Ok(ref s) = status {
            match s {
                ThreadStatus::Active => {
                    if let Some(exec) = latest_exec {
                        match exec.status.as_str() {
                            "queued" => {
                                // Check worker heartbeat
                                let heartbeat = self.store.latest_heartbeat().await.ok().flatten();
                                if heartbeat.is_none() {
                                    blockers.push(
                                        "execution is queued but no worker heartbeat found"
                                            .to_string(),
                                    );
                                    suggestions.push("start the worker process".to_string());
                                }
                            }
                            "executing" | "picked_up" => {
                                suggestions.push(
                                    "execution in progress — wait for completion".to_string(),
                                );
                            }
                            "failed" | "crashed" | "timed_out" => {
                                blockers.push(format!(
                                    "last execution {}: {}",
                                    exec.status,
                                    exec.error_detail.as_deref().unwrap_or("(no detail)")
                                ));
                                suggestions
                                    .push("inspect error and re-dispatch or abandon".to_string());
                            }
                            _ => {}
                        }
                    } else if messages.is_empty() {
                        blockers
                            .push("thread is Active but has no messages or executions".to_string());
                        suggestions.push("dispatch a message to start work".to_string());
                    }
                }
                ThreadStatus::Completed => {
                    suggestions.push("thread is completed — no action needed".to_string());
                }
                ThreadStatus::Abandoned => {
                    suggestions.push("thread was abandoned — reopen if needed".to_string());
                }
                ThreadStatus::Failed => {
                    blockers.push("thread is in failed state".to_string());
                    suggestions.push(
                        "inspect last execution error, then reopen and re-dispatch".to_string(),
                    );
                }
            }
        }

        #[derive(Serialize)]
        struct Diagnosis {
            thread_id: String,
            thread_status: String,
            summary: Option<String>,
            message_count: usize,
            execution_count: usize,
            latest_execution_id: Option<String>,
            latest_execution_status: Option<String>,
            latest_execution_agent: Option<String>,
            latest_execution_error: Option<String>,
            latest_execution_duration_ms: Option<i64>,
            blockers: Vec<String>,
            suggestions: Vec<String>,
        }

        Ok(json_text(&Diagnosis {
            thread_id: params.thread_id,
            thread_status: thread.status,
            summary: thread.summary,
            message_count: messages.len(),
            execution_count: executions.len(),
            latest_execution_id: latest_exec.map(|e| e.id.clone()),
            latest_execution_status: latest_exec.map(|e| e.status.clone()),
            latest_execution_agent: latest_exec.map(|e| e.agent_alias.clone()),
            latest_execution_error: latest_exec.and_then(|e| e.error_detail.clone()),
            latest_execution_duration_ms: latest_exec.and_then(|e| e.duration_ms),
            blockers,
            suggestions,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ping_cache_miss_returns_none() {
        let cache = PingCache::new();
        let ttl = Duration::from_secs(60);
        assert!(cache.get("agent-a", ttl).is_none());
    }

    #[test]
    fn test_ping_cache_hit_within_ttl() {
        let cache = PingCache::new();
        let ttl = Duration::from_secs(60);
        cache.set(
            "agent-a".to_string(),
            PingResult {
                alive: true,
                latency_ms: 42,
                detail: None,
            },
        );
        let result = cache.get("agent-a", ttl).expect("should hit cache");
        assert!(result.alive);
        assert_eq!(result.latency_ms, 42);
    }

    #[test]
    fn test_ping_cache_expired_returns_none() {
        let cache = PingCache::new();
        // Use a zero TTL so the entry is immediately expired.
        let ttl = Duration::from_secs(0);
        cache.set(
            "agent-a".to_string(),
            PingResult {
                alive: true,
                latency_ms: 10,
                detail: None,
            },
        );
        assert!(cache.get("agent-a", ttl).is_none());
    }

    #[test]
    fn test_ping_cache_overwrite() {
        let cache = PingCache::new();
        let ttl = Duration::from_secs(60);
        cache.set(
            "agent-a".to_string(),
            PingResult {
                alive: false,
                latency_ms: 100,
                detail: Some("down".to_string()),
            },
        );
        cache.set(
            "agent-a".to_string(),
            PingResult {
                alive: true,
                latency_ms: 5,
                detail: None,
            },
        );
        let result = cache.get("agent-a", ttl).expect("should hit cache");
        assert!(result.alive);
        assert_eq!(result.latency_ms, 5);
    }

    #[test]
    fn test_ping_cache_independent_agents() {
        let cache = PingCache::new();
        let ttl = Duration::from_secs(60);
        cache.set(
            "agent-a".to_string(),
            PingResult {
                alive: true,
                latency_ms: 10,
                detail: None,
            },
        );
        cache.set(
            "agent-b".to_string(),
            PingResult {
                alive: false,
                latency_ms: 0,
                detail: Some("unreachable".to_string()),
            },
        );
        let a = cache.get("agent-a", ttl).unwrap();
        let b = cache.get("agent-b", ttl).unwrap();
        assert!(a.alive);
        assert!(!b.alive);
        assert!(cache.get("agent-c", ttl).is_none());
    }
}
