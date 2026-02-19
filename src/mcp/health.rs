//! orch_health and orch_tasks implementations.

use chrono::Utc;
use rmcp::model::CallToolResult;

use super::params::{HealthParams, TasksParams};
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::health::{AgentHealth, HealthReport, HealthStatus};
use crate::model::agent::Agent;

impl OrchestratorMcpServer {
    pub(crate) async fn health_impl(
        &self,
        params: HealthParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let ping_timeout = self.config.orchestration.ping_timeout_secs;

        if let Some(alias) = &params.alias {
            // Single agent health check
            let health = self.check_agent_health(alias, ping_timeout).await;
            Ok(json_text(&health))
        } else {
            // All agents health check
            let mut agents = Vec::new();
            for agent_config in &self.config.agents {
                let health = self
                    .check_agent_health(&agent_config.alias, ping_timeout)
                    .await;
                agents.push(health);
            }
            let report = HealthReport {
                agents,
                checked_at: Utc::now(),
            };
            Ok(json_text(&report))
        }
    }

    async fn check_agent_health(&self, alias: &str, ping_timeout: u64) -> AgentHealth {
        let agent_config = match self.config.agents.iter().find(|a| a.alias == alias) {
            Some(a) => a,
            None => {
                return AgentHealth {
                    alias: alias.to_string(),
                    backend: "unknown".to_string(),
                    status: HealthStatus::Unhealthy,
                    latency_ms: None,
                    state: "not-found".to_string(),
                    detail: Some(format!("agent '{}' not in config", alias)),
                };
            }
        };

        let backend = match self.backend_registry.get_by_name(&agent_config.backend) {
            Ok(b) => b,
            Err(_) => {
                return AgentHealth {
                    alias: alias.to_string(),
                    backend: agent_config.backend.clone(),
                    status: HealthStatus::Unhealthy,
                    latency_ms: None,
                    state: "idle".to_string(),
                    detail: Some(format!(
                        "backend '{}' not registered",
                        agent_config.backend
                    )),
                };
            }
        };

        // Build an Agent model for the ping call
        let agent = Agent {
            alias: agent_config.alias.clone(),
            identity: agent_config.identity.clone(),
            backend: agent_config.backend.clone(),
            model: agent_config.model.clone(),
            prompt: agent_config.prompt.clone(),
            prompt_file: agent_config.prompt_file.clone(),
            timeout_secs: agent_config.timeout_secs,
            backend_args: agent_config.backend_args.clone(),
            env: agent_config.env.clone(),
        };

        let ping_result = backend.ping(&agent, ping_timeout).await;

        AgentHealth {
            alias: alias.to_string(),
            backend: agent_config.backend.clone(),
            status: if ping_result.alive {
                HealthStatus::Healthy
            } else {
                HealthStatus::Unhealthy
            },
            latency_ms: Some(ping_result.latency_ms),
            state: "idle".to_string(),
            detail: ping_result.detail,
        }
    }

    pub(crate) async fn tasks_impl(
        &self,
        params: TasksParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let limit = params.limit.unwrap_or(20) as i64;

        // Query the apalis Jobs table directly for trigger execution records
        let rows = match self.query_trigger_tasks(
            params.alias.as_deref(),
            params.batch_id.as_deref(),
            limit,
        ).await {
            Ok(r) => r,
            Err(e) => return Ok(err_text(e)),
        };

        Ok(json_text(&rows))
    }

    async fn query_trigger_tasks(
        &self,
        alias: Option<&str>,
        batch_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, sqlx::Error> {
        // Build query with optional filters
        // Jobs table: job (BLOB), id, job_type, status, attempts, max_attempts, run_at, last_result, lock_at, lock_by, done_at, priority, metadata
        // The `job` column contains JSON-encoded TriggerJob bytes
        let mut sql = String::from(
            "SELECT id, job_type, status, attempts, run_at, done_at, last_result, job
             FROM Jobs WHERE job_type = ?",
        );
        // We'll filter by alias/batch_id via JSON extraction from the job column
        sql.push_str(" ORDER BY run_at DESC LIMIT ?");

        let rows: Vec<(String, String, String, i32, i64, Option<i64>, Option<String>, Vec<u8>)> =
            sqlx::query_as(&sql)
                .bind(crate::worker::trigger::TRIGGER_QUEUE)
                .bind(limit)
                .fetch_all(self.store.pool())
                .await?;

        let mut results = Vec::new();
        for (id, job_type, status, attempts, run_at, done_at, last_result, job_bytes) in rows {
            // Try to parse the TriggerJob from bytes
            let job: Option<crate::worker::TriggerJob> =
                serde_json::from_slice(&job_bytes).ok();

            // Apply alias/batch_id filters on the parsed job
            if let Some(filter_alias) = alias {
                if let Some(ref j) = job {
                    if j.agent_alias != filter_alias {
                        continue;
                    }
                } else {
                    continue;
                }
            }
            if let Some(filter_batch) = batch_id {
                if let Some(ref j) = job {
                    if j.batch_id.as_deref() != Some(filter_batch) {
                        continue;
                    }
                } else {
                    continue;
                }
            }

            let mut val = serde_json::json!({
                "job_id": id,
                "queue": job_type,
                "status": status,
                "attempts": attempts,
                "run_at": run_at,
            });
            if let Some(da) = done_at {
                val["done_at"] = serde_json::Value::Number(da.into());
            }
            if let Some(ref err) = last_result {
                val["last_result"] = serde_json::Value::String(err.clone());
            }
            if let Some(ref j) = job {
                val["thread_id"] = serde_json::Value::String(j.thread_id.clone());
                val["agent_alias"] = serde_json::Value::String(j.agent_alias.clone());
                val["from_alias"] = serde_json::Value::String(j.from_alias.clone());
                val["intent"] = serde_json::Value::String(j.intent.clone());
                if let Some(ref b) = j.batch_id {
                    val["batch_id"] = serde_json::Value::String(b.clone());
                }
            }
            results.push(val);
        }

        Ok(results)
    }
}
