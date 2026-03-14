//! Event broadcast channel for orchestrator state changes.
//!
//! The `EventBus` wraps a `tokio::sync::broadcast` channel. The worker emits
//! events at every significant state transition; consumers (dashboard, future
//! HTTP/SSE layer, notification system) subscribe independently.
//!
//! The channel is **optional** — sending to a bus with zero subscribers is a
//! cheap no-op (`send` returns `Err` which we discard).
//!
//! Events are ephemeral: not persisted. A slow subscriber that falls more than
//! `BROADCAST_CAPACITY` events behind will receive `RecvError::Lagged` and must
//! recover (typically: refresh from SQLite).

use tokio::sync::broadcast;

/// Capacity of the internal broadcast channel.
const BROADCAST_CAPACITY: usize = 256;

/// All significant orchestrator state-change events.
#[derive(Debug, Clone)]
pub enum OrchestratorEvent {
    /// A claimed execution has been handed off to the backend trigger.
    ExecutionStarted {
        execution_id: String,
        thread_id: String,
        agent_alias: String,
    },
    /// Incremental progress from a running execution (placeholder for ORCH-EVO-1).
    ExecutionProgress {
        execution_id: String,
        thread_id: String,
        agent_alias: String,
        /// Short human-readable summary of recent activity.
        summary: String,
    },
    /// An execution has reached a terminal state.
    ExecutionCompleted {
        execution_id: String,
        thread_id: String,
        agent_alias: String,
        /// `true` if the backend exited cleanly.
        success: bool,
        duration_ms: i64,
    },
    /// A thread's status has changed.
    ThreadStatusChanged {
        thread_id: String,
        /// New status string (mirrors `ThreadStatus::as_str()`).
        new_status: String,
    },
    /// Progress update for a batch of related threads.
    BatchProgress {
        batch_id: String,
        completed: u32,
        total: u32,
    },
    /// An agent's health/liveness status has changed.
    AgentHealthChanged { agent_alias: String, healthy: bool },
    /// A new message has been inserted into a thread.
    MessageReceived {
        thread_id: String,
        message_id: i64,
        from_alias: String,
        intent: String,
    },
}

/// Shared event bus — thin wrapper around `tokio::broadcast`.
///
/// Clone to share across threads; each clone shares the same underlying channel.
#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<OrchestratorEvent>,
}

impl EventBus {
    /// Create a new event bus with the default broadcast capacity.
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self { sender }
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<OrchestratorEvent> {
        self.sender.subscribe()
    }

    /// Emit an event. Silently discards the error when there are no subscribers.
    pub fn emit(&self, event: OrchestratorEvent) {
        let _ = self.sender.send(event);
    }

    /// Number of active subscribers.
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_bus_emit_and_receive() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        bus.emit(OrchestratorEvent::ExecutionStarted {
            execution_id: "exec-1".to_string(),
            thread_id: "thread-1".to_string(),
            agent_alias: "worker-a".to_string(),
        });

        let event = rx.recv().await.expect("should receive event");
        match event {
            OrchestratorEvent::ExecutionStarted {
                execution_id,
                thread_id,
                agent_alias,
            } => {
                assert_eq!(execution_id, "exec-1");
                assert_eq!(thread_id, "thread-1");
                assert_eq!(agent_alias, "worker-a");
            }
            _ => panic!("unexpected event variant"),
        }
    }

    #[tokio::test]
    async fn test_event_bus_no_subscriber_is_noop() {
        let bus = EventBus::new();
        // Emit without any subscriber — must not panic or block.
        bus.emit(OrchestratorEvent::ExecutionCompleted {
            execution_id: "exec-2".to_string(),
            thread_id: "thread-2".to_string(),
            agent_alias: "worker-b".to_string(),
            success: true,
            duration_ms: 1000,
        });
    }

    #[tokio::test]
    async fn test_event_bus_multiple_subscribers() {
        let bus = EventBus::new();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.emit(OrchestratorEvent::ThreadStatusChanged {
            thread_id: "t-42".to_string(),
            new_status: "Completed".to_string(),
        });

        let e1 = rx1.recv().await.expect("rx1 should receive");
        let e2 = rx2.recv().await.expect("rx2 should receive");

        match (e1, e2) {
            (
                OrchestratorEvent::ThreadStatusChanged { thread_id: t1, .. },
                OrchestratorEvent::ThreadStatusChanged { thread_id: t2, .. },
            ) => {
                assert_eq!(t1, "t-42");
                assert_eq!(t2, "t-42");
            }
            _ => panic!("unexpected event variants"),
        }
    }

    #[test]
    fn test_event_bus_try_recv_empty_when_no_events() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn test_event_bus_receiver_count() {
        let bus = EventBus::new();
        assert_eq!(bus.receiver_count(), 0);
        let _rx1 = bus.subscribe();
        assert_eq!(bus.receiver_count(), 1);
        let _rx2 = bus.subscribe();
        assert_eq!(bus.receiver_count(), 2);
    }
}
