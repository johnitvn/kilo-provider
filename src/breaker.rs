//! Circuit breaker skeleton.
//!
//! This module owns the state machine (`Closed` → `Open` → `HalfOpen`) and the
//! counters configured via the `circuit_breaker` section of the config file.
//! It is **not wired into the request path** yet — the bootstrap only needs
//! the plumbing and the `/readyz` visibility.

use std::{
    sync::{Mutex, MutexGuard},
    time::Instant,
};

use serde::Serialize;

use crate::config::CircuitBreakerConfig;

/// Discrete circuit states.
///
/// * `Closed`: normal operation; failures are counted.
/// * `Open`: requests are (in the future) short-circuited until the open
///   timeout elapses, then promoted to `HalfOpen`.
/// * `HalfOpen`: a limited number of probe requests decide whether the
///   circuit closes again (enough successes) or re-opens (a failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Counter/state plumbing guarded by a mutex (no `await` happens inside, so a
/// std mutex is sufficient and cheap).
#[derive(Debug)]
struct BreakerCore {
    state: CircuitState,
    consecutive_failures: u32,
    consecutive_successes: u32,
    /// Moment the circuit last opened; `None` outside `Open`.
    opened_at: Option<Instant>,
}

/// Circuit breaker built from config thresholds. Prototype of the component
/// that will later gate upstream calls.
#[derive(Debug)]
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    core: Mutex<BreakerCore>,
}

/// Serializable view used by readiness endpoints.
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            core: Mutex::new(BreakerCore {
                state: CircuitState::Closed,
                consecutive_failures: 0,
                consecutive_successes: 0,
                opened_at: None,
            }),
            config,
        }
    }

    /// The breaker's effective state. `Open` circuits are lazily promoted to
    /// `HalfOpen` once the configured open timeout has elapsed.
    pub fn state(&self) -> CircuitState {
        self.with_core(|mut core, config| {
            if core.state == CircuitState::Open {
                let elapsed = core
                    .opened_at
                    .map(|opened_at| opened_at.elapsed().as_secs())
                    .unwrap_or(u64::MAX);
                if elapsed >= config.open_timeout_secs {
                    core.state = CircuitState::HalfOpen;
                    core.consecutive_successes = 0;
                }
            }
            core.state
        })
    }

    /// Register a success. Behaviour per state:
    /// * `Closed` — resets the failure counter.
    /// * `HalfOpen` — counts toward `success_threshold`; enough successes close
    ///   the circuit.
    /// * `Open` — no-op (probes are expected only in `HalfOpen`).
    pub fn record_success(&self) {
        self.with_core(|mut core, config| match core.state {
            CircuitState::Closed => {
                core.consecutive_failures = 0;
            }
            CircuitState::HalfOpen => {
                core.consecutive_successes = core.consecutive_successes.saturating_add(1);
                if core.consecutive_successes >= config.success_threshold {
                    core.state = CircuitState::Closed;
                    core.consecutive_failures = 0;
                    core.consecutive_successes = 0;
                    core.opened_at = None;
                }
            }
            CircuitState::Open => {}
        });
    }

    /// Register a failure. Behaviour per state:
    /// * `Closed` — counts toward `failure_threshold`; enough failures open the
    ///   circuit.
    /// * `HalfOpen` — a probe failure re-opens the circuit.
    /// * `Open` — no-op (counters are frozen while open).
    pub fn record_failure(&self) {
        self.with_core(|mut core, config| match core.state {
            CircuitState::Closed => {
                core.consecutive_failures = core.consecutive_failures.saturating_add(1);
                if core.consecutive_failures >= config.failure_threshold {
                    core.open(config.failure_threshold);
                }
            }
            CircuitState::HalfOpen => {
                core.open(config.failure_threshold);
            }
            CircuitState::Open => {}
        });
    }

    /// Serializable counters/status for `/readyz`. Not a gatekeeper.
    pub fn snapshot(&self) -> Snapshot {
        let state = self.state();
        self.with_core(|core, _| Snapshot {
            state,
            consecutive_failures: core.consecutive_failures,
            consecutive_successes: core.consecutive_successes,
        })
    }

    fn with_core<T>(
        &self,
        with: impl FnOnce(MutexGuard<'_, BreakerCore>, &CircuitBreakerConfig) -> T,
    ) -> T {
        let guard = self
            .core
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        with(guard, &self.config)
    }
}

impl BreakerCore {
    fn open(&mut self, _failure_threshold: u32) {
        self.state = CircuitState::Open;
        self.consecutive_successes = 0;
        // Keep the failure count visible for observability (reset on close).
        self.opened_at = Some(Instant::now());
    }
}

// The breaker is intentionally NOT used by the request path in this bootstrap.
// A dedicated integration (checking `state()`, wrapping `UpstreamClient`
// calls, feeding `record_success/failure`) lands with the resilience feature
// PR. See https://.../kilo-provider design notes.

#[cfg(test)]
mod tests {
    use super::*;

    fn breaker(failures: u32, successes: u32) -> CircuitBreaker {
        CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: failures,
            success_threshold: successes,
            open_timeout_secs: 30,
        })
    }

    #[test]
    fn opens_after_threshold_failures() {
        let breaker = breaker(2, 3);
        assert_eq!(breaker.state(), CircuitState::Closed);
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Open);
    }

    #[test]
    fn success_resets_failures_in_closed_state() {
        let breaker = breaker(2, 2);
        breaker.record_failure();
        breaker.record_success();
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::Closed);
        assert_eq!(breaker.snapshot().consecutive_failures, 1);
    }

    #[test]
    fn half_open_promotes_after_open_timeout() {
        // 0s open timeout promotes immediately on the next state() call.
        let breaker = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            success_threshold: 2,
            open_timeout_secs: 0,
        });
        breaker.record_failure();
        assert_eq!(breaker.state(), CircuitState::HalfOpen);
    }
}
