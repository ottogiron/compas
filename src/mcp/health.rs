//! orch_health and orch_diagnose implementations.

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::model::agent::Agent;
use crate::store::ThreadStatus;

impl OrchestratorMcpServer {
    // ── orch_health ──────────────────────────────────────────────────────

    pub async fn health_impl(&self, params: HealthParams) -> Result<CallToolResult, rmcp::Error> {
        #[derive(Serialize)]
        struct AgentHealth {
            alias: String,
            backend: String,
            ping_alive: bool,
            ping_latency_ms: u64,
            ping_detail: Option<String>,
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

        // Filter agents if alias specified
        let agents_to_check: Vec<_> = if let Some(ref alias) = params.alias {
            self.config
                .agents
                .iter()
                .filter(|a| a.alias == *alias)
                .collect()
        } else {
            self.config.agents.iter().collect()
        };

        let ping_timeout = self.config.orchestration.ping_timeout_secs;
        let mut agent_health = Vec::new();

        for agent_cfg in agents_to_check {
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
            };

            let ping = match self.backend_registry.get(agent_cfg) {
                Ok(backend) => backend.ping(&agent, ping_timeout).await,
                Err(e) => crate::backend::PingResult {
                    alive: false,
                    latency_ms: 0,
                    detail: Some(format!("backend not found: {}", e)),
                },
            };

            agent_health.push(AgentHealth {
                alias: agent_cfg.alias.clone(),
                backend: agent_cfg.backend.clone(),
                ping_alive: ping.alive,
                ping_latency_ms: ping.latency_ms,
                ping_detail: ping.detail,
            });
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
    ) -> Result<CallToolResult, rmcp::Error> {
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
            message_count: usize,
            execution_count: usize,
            latest_execution_status: Option<String>,
            blockers: Vec<String>,
            suggestions: Vec<String>,
        }

        Ok(json_text(&Diagnosis {
            thread_id: params.thread_id,
            thread_status: thread.status,
            message_count: messages.len(),
            execution_count: executions.len(),
            latest_execution_status: latest_exec.map(|e| e.status.clone()),
            blockers,
            suggestions,
        }))
    }
}
