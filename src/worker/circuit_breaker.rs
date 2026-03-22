//! Per-backend circuit breaker for failure protection.
//!
//! Tracks consecutive failures per backend and transitions through states:
//! - Closed: normal operation, all dispatches proceed
//! - Open: backend is failing, dispatches are skipped
//! - HalfOpen: cooldown elapsed, one probe execution is allowed
//!
//! Thread-safe: wrapped in `Arc<Mutex<_>>` by the worker runner.

use std::collections::HashMap;
use std::time::Instant;

/// Circuit breaker state for a single backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation — dispatches proceed.
    Closed,
    /// Backend is failing — dispatches are skipped until cooldown expires.
    Open,
    /// Cooldown expired — one probe execution is allowed to test recovery.
    HalfOpen,
}

impl CircuitState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Internal state for a single backend's circuit.
#[derive(Debug)]
struct BackendCircuit {
    consecutive_failures: u32,
    state: CircuitState,
    /// When the circuit transitioned to Open.
    opened_at: Option<Instant>,
    /// Whether a half-open probe execution is currently in flight.
    half_open_probe_active: bool,
}

impl Default for BackendCircuit {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            state: CircuitState::Closed,
            opened_at: None,
            half_open_probe_active: false,
        }
    }
}

/// Registry of per-backend circuit breakers.
#[derive(Debug, Default)]
pub struct CircuitBreakerRegistry {
    circuits: HashMap<String, BackendCircuit>,
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check the current state of a backend's circuit breaker.
    ///
    /// Returns:
    /// - `Closed`: proceed with dispatch
    /// - `Open`: skip dispatch (backend is failing)
    /// - `HalfOpen`: allow one probe execution
    ///
    /// Automatically transitions Open → HalfOpen when cooldown expires.
    pub fn check(&mut self, backend: &str, cooldown_secs: u64) -> CircuitState {
        let circuit = self.circuits.entry(backend.to_string()).or_default();

        match circuit.state {
            CircuitState::Closed => CircuitState::Closed,
            CircuitState::Open => {
                // Check if cooldown has elapsed
                if let Some(opened_at) = circuit.opened_at {
                    if opened_at.elapsed().as_secs() >= cooldown_secs {
                        circuit.state = CircuitState::HalfOpen;
                        circuit.half_open_probe_active = true;
                        return CircuitState::HalfOpen;
                    }
                }
                CircuitState::Open
            }
            CircuitState::HalfOpen => {
                if circuit.half_open_probe_active {
                    // Already have a probe in flight — block additional dispatches
                    CircuitState::Open
                } else {
                    circuit.half_open_probe_active = true;
                    CircuitState::HalfOpen
                }
            }
        }
    }

    /// Record a successful execution for a backend.
    ///
    /// Resets the failure counter and transitions to Closed.
    pub fn record_success(&mut self, backend: &str) {
        let circuit = self.circuits.entry(backend.to_string()).or_default();
        circuit.consecutive_failures = 0;
        circuit.state = CircuitState::Closed;
        circuit.opened_at = None;
        circuit.half_open_probe_active = false;
    }

    /// Record a failed execution for a backend.
    ///
    /// Increments the failure counter. If threshold is reached, transitions
    /// to Open state.
    ///
    /// Returns the new circuit state.
    pub fn record_failure(&mut self, backend: &str, threshold: u32) -> CircuitState {
        let circuit = self.circuits.entry(backend.to_string()).or_default();
        circuit.consecutive_failures += 1;

        if circuit.consecutive_failures >= threshold {
            circuit.state = CircuitState::Open;
            circuit.opened_at = Some(Instant::now());
            circuit.half_open_probe_active = false;
        }

        circuit.state.clone()
    }

    /// Get the state of all tracked backends.
    ///
    /// Returns `(backend_name, state, consecutive_failures)` tuples.
    pub fn states(&self) -> Vec<(String, CircuitState, u32)> {
        self.circuits
            .iter()
            .map(|(name, circuit)| {
                (
                    name.clone(),
                    circuit.state.clone(),
                    circuit.consecutive_failures,
                )
            })
            .collect()
    }

    /// Get the state of a single backend.
    pub fn state_of(&self, backend: &str) -> CircuitState {
        self.circuits
            .get(backend)
            .map(|c| c.state.clone())
            .unwrap_or(CircuitState::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_closed_by_default() {
        let mut registry = CircuitBreakerRegistry::new();
        assert_eq!(registry.check("claude", 60), CircuitState::Closed);
        assert_eq!(registry.state_of("claude"), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_opens_after_threshold() {
        let mut registry = CircuitBreakerRegistry::new();
        // 2 failures — still closed
        registry.record_failure("claude", 3);
        registry.record_failure("claude", 3);
        assert_eq!(registry.check("claude", 60), CircuitState::Closed);

        // 3rd failure — opens
        let state = registry.record_failure("claude", 3);
        assert_eq!(state, CircuitState::Open);
        assert_eq!(registry.check("claude", 60), CircuitState::Open);
    }

    #[test]
    fn test_circuit_half_open_after_cooldown() {
        let mut registry = CircuitBreakerRegistry::new();
        // Open the circuit
        for _ in 0..3 {
            registry.record_failure("claude", 3);
        }
        assert_eq!(registry.check("claude", 60), CircuitState::Open);

        // Simulate cooldown elapsed by setting opened_at to the past
        let circuit = registry.circuits.get_mut("claude").unwrap();
        circuit.opened_at = Some(Instant::now() - std::time::Duration::from_secs(61));

        // Should transition to HalfOpen
        assert_eq!(registry.check("claude", 60), CircuitState::HalfOpen);
    }

    #[test]
    fn test_half_open_success_resets_to_closed() {
        let mut registry = CircuitBreakerRegistry::new();
        // Open the circuit
        for _ in 0..3 {
            registry.record_failure("claude", 3);
        }
        // Fast-forward cooldown
        let circuit = registry.circuits.get_mut("claude").unwrap();
        circuit.opened_at = Some(Instant::now() - std::time::Duration::from_secs(61));

        // Transition to HalfOpen
        assert_eq!(registry.check("claude", 60), CircuitState::HalfOpen);

        // Record success — should reset to Closed
        registry.record_success("claude");
        assert_eq!(registry.state_of("claude"), CircuitState::Closed);
        assert_eq!(registry.check("claude", 60), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_failure_reopens() {
        let mut registry = CircuitBreakerRegistry::new();
        // Open the circuit
        for _ in 0..3 {
            registry.record_failure("claude", 3);
        }
        // Fast-forward cooldown
        let circuit = registry.circuits.get_mut("claude").unwrap();
        circuit.opened_at = Some(Instant::now() - std::time::Duration::from_secs(61));

        // Transition to HalfOpen
        assert_eq!(registry.check("claude", 60), CircuitState::HalfOpen);

        // Record failure — should re-open (1 failure is enough since
        // consecutive_failures is still >= threshold from before)
        let state = registry.record_failure("claude", 3);
        assert_eq!(state, CircuitState::Open);
    }

    #[test]
    fn test_success_resets_counter() {
        let mut registry = CircuitBreakerRegistry::new();
        registry.record_failure("claude", 3);
        registry.record_failure("claude", 3);
        // 2 failures, then success — counter resets
        registry.record_success("claude");
        assert_eq!(registry.check("claude", 60), CircuitState::Closed);

        // Need 3 more failures to open again
        registry.record_failure("claude", 3);
        registry.record_failure("claude", 3);
        assert_eq!(registry.check("claude", 60), CircuitState::Closed);
        registry.record_failure("claude", 3);
        assert_eq!(registry.check("claude", 60), CircuitState::Open);
    }

    #[test]
    fn test_independent_backends() {
        let mut registry = CircuitBreakerRegistry::new();
        // Open circuit for backend A
        for _ in 0..3 {
            registry.record_failure("claude", 3);
        }
        assert_eq!(registry.check("claude", 60), CircuitState::Open);

        // Backend B should still be closed
        assert_eq!(registry.check("codex", 60), CircuitState::Closed);
        assert_eq!(registry.state_of("codex"), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_blocks_second_probe() {
        let mut registry = CircuitBreakerRegistry::new();
        // Open the circuit
        for _ in 0..3 {
            registry.record_failure("claude", 3);
        }
        // Fast-forward cooldown
        let circuit = registry.circuits.get_mut("claude").unwrap();
        circuit.opened_at = Some(Instant::now() - std::time::Duration::from_secs(61));

        // First check: HalfOpen (probe allowed)
        assert_eq!(registry.check("claude", 60), CircuitState::HalfOpen);
        // Second check: should block (probe already in flight)
        assert_eq!(registry.check("claude", 60), CircuitState::Open);
    }

    #[test]
    fn test_states_returns_all_backends() {
        let mut registry = CircuitBreakerRegistry::new();
        registry.record_failure("claude", 3);
        registry.record_success("codex");

        let states = registry.states();
        assert_eq!(states.len(), 2);

        let claude_state = states.iter().find(|(n, _, _)| n == "claude").unwrap();
        assert_eq!(claude_state.1, CircuitState::Closed);
        assert_eq!(claude_state.2, 1);

        let codex_state = states.iter().find(|(n, _, _)| n == "codex").unwrap();
        assert_eq!(codex_state.1, CircuitState::Closed);
        assert_eq!(codex_state.2, 0);
    }
}
