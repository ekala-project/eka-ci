use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Circuit-breaker states for a remote builder.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BreakerState {
    /// Builder is accepting builds normally.
    Closed,
    /// Builder has been disabled after repeated failures. Includes the
    /// instant at which a recovery probe should be attempted.
    Open { recover_at: Instant },
    /// A single probe build has been allowed through to test recovery.
    HalfOpen,
}

/// Per-builder failure tracking.
struct BuilderBreaker {
    state: BreakerState,
    /// Timestamps of recent connection-related failures.
    recent_failures: Vec<Instant>,
    /// Current backoff duration for the Open→HalfOpen transition.
    backoff: Duration,
}

/// Failure threshold: trip the breaker after this many failures…
pub const FAILURE_THRESHOLD: usize = 3;
/// …within this time window.
const FAILURE_WINDOW: Duration = Duration::from_secs(120);
/// Initial backoff before the first recovery probe.
const INITIAL_BACKOFF: Duration = Duration::from_secs(30);
/// Maximum backoff between recovery probes.
const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// Timeout for `nix store ping` recovery probes.
const PING_TIMEOUT: Duration = Duration::from_secs(30);

impl BuilderBreaker {
    fn new() -> Self {
        Self {
            state: BreakerState::Closed,
            recent_failures: Vec::new(),
            backoff: INITIAL_BACKOFF,
        }
    }

    /// Record a connection failure. Returns true if the breaker just tripped.
    fn record_failure(&mut self) -> bool {
        let now = Instant::now();
        self.recent_failures
            .retain(|t| now.duration_since(*t) < FAILURE_WINDOW);
        self.recent_failures.push(now);

        if self.recent_failures.len() >= FAILURE_THRESHOLD && self.state == BreakerState::Closed {
            self.state = BreakerState::Open {
                recover_at: now + self.backoff,
            };
            true
        } else if self.state == BreakerState::HalfOpen {
            // Probe failed — go back to Open with increased backoff.
            self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
            self.state = BreakerState::Open {
                recover_at: now + self.backoff,
            };
            true
        } else {
            false
        }
    }

    /// Record a successful build, resetting the breaker.
    fn record_success(&mut self) {
        if self.state != BreakerState::Closed {
            info!("circuit-breaker: builder recovered, resetting to Closed");
        }
        self.state = BreakerState::Closed;
        self.recent_failures.clear();
        self.backoff = INITIAL_BACKOFF;
    }

    /// Whether this builder should currently accept new builds.
    fn is_available(&self) -> bool {
        match &self.state {
            BreakerState::Closed => true,
            BreakerState::HalfOpen => false,
            BreakerState::Open { recover_at } => Instant::now() >= *recover_at,
        }
    }

    /// Transition from Open (with elapsed cooldown) to HalfOpen.
    fn try_half_open(&mut self) -> bool {
        if let BreakerState::Open { recover_at } = &self.state {
            if Instant::now() >= *recover_at {
                self.state = BreakerState::HalfOpen;
                return true;
            }
        }
        false
    }
}

/// Shared circuit-breaker registry for all remote builders.
///
/// Keyed by builder name (the `builder_name` field on `Builder`).
/// Local builders are never tracked — they cannot have connection failures.
#[derive(Clone)]
pub struct CircuitBreakerRegistry {
    inner: Arc<Mutex<HashMap<String, BuilderBreaker>>>,
}

impl CircuitBreakerRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a builder so the breaker is initialised before any builds run.
    pub async fn register(&self, builder_name: &str) {
        let mut map = self.inner.lock().await;
        map.entry(builder_name.to_string())
            .or_insert_with(BuilderBreaker::new);
    }

    /// Record a connection-related failure for `builder_name`.
    /// Returns `true` if the breaker just tripped (newly disabled).
    pub async fn record_failure(&self, builder_name: &str) -> bool {
        let mut map = self.inner.lock().await;
        if let Some(breaker) = map.get_mut(builder_name) {
            let tripped = breaker.record_failure();
            if tripped {
                warn!(
                    "circuit-breaker: builder '{}' disabled after {} failures in {}s (backoff: \
                     {}s)",
                    builder_name,
                    FAILURE_THRESHOLD,
                    FAILURE_WINDOW.as_secs(),
                    breaker.backoff.as_secs(),
                );
            }
            tripped
        } else {
            false
        }
    }

    /// Record a successful build for `builder_name`, resetting the breaker.
    pub async fn record_success(&self, builder_name: &str) {
        let mut map = self.inner.lock().await;
        if let Some(breaker) = map.get_mut(builder_name) {
            breaker.record_success();
        }
    }

    /// Check whether `builder_name` is currently accepting builds.
    pub async fn is_available(&self, builder_name: &str) -> bool {
        let map = self.inner.lock().await;
        match map.get(builder_name) {
            Some(breaker) => breaker.is_available(),
            None => true, // Unknown builder → allow (defensive)
        }
    }

    /// Attempt a recovery probe for a builder whose cooldown has elapsed.
    /// Returns `true` if the builder is now available (probe succeeded or
    /// the builder was already available).
    pub async fn try_recover(&self, builder_name: &str, remote_uri: &str) -> bool {
        // Check and transition to HalfOpen under the lock, then release
        // before running the probe (which is slow).
        {
            let mut map = self.inner.lock().await;
            if let Some(breaker) = map.get_mut(builder_name) {
                if breaker.state == BreakerState::Closed {
                    return true;
                }
                if !breaker.try_half_open() {
                    return false; // cooldown hasn't elapsed yet
                }
            } else {
                return true;
            }
        }

        // Run the probe outside the lock.
        let probe_ok = probe_builder(remote_uri).await;

        let mut map = self.inner.lock().await;
        if let Some(breaker) = map.get_mut(builder_name) {
            if probe_ok {
                breaker.record_success();
                info!(
                    "circuit-breaker: probe succeeded for '{}', re-enabled",
                    builder_name
                );
                true
            } else {
                breaker.record_failure();
                warn!(
                    "circuit-breaker: probe failed for '{}', extending backoff to {}s",
                    builder_name,
                    breaker.backoff.as_secs(),
                );
                false
            }
        } else {
            probe_ok
        }
    }
}

/// Run `nix store ping --store <uri>` as a recovery probe.
async fn probe_builder(remote_uri: &str) -> bool {
    tokio::time::timeout(
        PING_TIMEOUT,
        Command::new("nix")
            .args(["store", "ping", "--store", remote_uri])
            .output(),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .map(|x| x.status.success())
    .unwrap_or(false)
}

/// Classify a nix-build exit status as a connection-related failure.
///
/// Connection failures include SSH errors (exit code 255), connection
/// refused, and similar transport-level issues. Regular build failures
/// (derivation evaluation errors, build script failures) return false.
pub fn is_connection_failure(exit_code: Option<i32>, stderr: &str) -> bool {
    // SSH connection errors use exit code 255
    if exit_code == Some(255) {
        return true;
    }

    let lower = stderr.to_ascii_lowercase();
    // Connection-related error patterns in nix-build stderr
    lower.contains("connection refused")
        || lower.contains("connection timed out")
        || lower.contains("connection reset")
        || lower.contains("no route to host")
        || lower.contains("host is unreachable")
        || lower.contains("name or service not known")
        || lower.contains("ssh_exchange_identification")
        || lower.contains("permission denied (publickey")
        || lower.contains("broken pipe")
        || lower.contains("failed to connect to")
        || lower.contains("cannot connect to")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaker_stays_closed_below_threshold() {
        let mut b = BuilderBreaker::new();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            assert!(!b.record_failure());
        }
        assert!(b.is_available());
    }

    #[test]
    fn breaker_trips_at_threshold() {
        let mut b = BuilderBreaker::new();
        for i in 0..FAILURE_THRESHOLD {
            let tripped = b.record_failure();
            if i < FAILURE_THRESHOLD - 1 {
                assert!(!tripped);
            } else {
                assert!(tripped);
            }
        }
        assert!(!b.is_available());
    }

    #[test]
    fn success_resets_breaker() {
        let mut b = BuilderBreaker::new();
        for _ in 0..FAILURE_THRESHOLD {
            b.record_failure();
        }
        assert!(!b.is_available());
        b.record_success();
        assert!(b.is_available());
        assert_eq!(b.state, BreakerState::Closed);
    }

    #[test]
    fn half_open_failure_increases_backoff() {
        let mut b = BuilderBreaker::new();
        for _ in 0..FAILURE_THRESHOLD {
            b.record_failure();
        }
        let initial = b.backoff;
        // Simulate cooldown elapsed
        b.state = BreakerState::HalfOpen;
        b.record_failure();
        assert_eq!(b.backoff, (initial * 2).min(MAX_BACKOFF));
    }

    #[test]
    fn backoff_caps_at_max() {
        let mut b = BuilderBreaker::new();
        b.backoff = MAX_BACKOFF;
        b.state = BreakerState::HalfOpen;
        b.record_failure();
        assert_eq!(b.backoff, MAX_BACKOFF);
    }

    #[test]
    fn old_failures_expire() {
        let mut b = BuilderBreaker::new();
        // Push failures that are "old" by manipulating the vec directly
        let old = Instant::now() - FAILURE_WINDOW - Duration::from_secs(1);
        for _ in 0..FAILURE_THRESHOLD {
            b.recent_failures.push(old);
        }
        // Adding one new failure shouldn't trip because the old ones expired
        assert!(!b.record_failure());
        assert!(b.is_available());
    }

    #[test]
    fn is_connection_failure_ssh() {
        assert!(is_connection_failure(Some(255), ""));
        assert!(is_connection_failure(Some(255), "some output"));
    }

    #[test]
    fn is_connection_failure_patterns() {
        assert!(is_connection_failure(Some(1), "Connection refused"));
        assert!(is_connection_failure(Some(1), "connection timed out"));
        assert!(is_connection_failure(Some(1), "No route to host"));
        assert!(is_connection_failure(
            Some(1),
            "Permission denied (publickey"
        ));
    }

    #[test]
    fn normal_build_failure_not_connection() {
        assert!(!is_connection_failure(Some(1), "build failed"));
        assert!(!is_connection_failure(
            Some(100),
            "error: builder for 'foo' failed"
        ));
        assert!(!is_connection_failure(Some(0), ""));
    }

    #[tokio::test]
    async fn registry_basic_flow() {
        let reg = CircuitBreakerRegistry::new();
        reg.register("test-builder").await;
        assert!(reg.is_available("test-builder").await);

        for _ in 0..FAILURE_THRESHOLD {
            reg.record_failure("test-builder").await;
        }
        assert!(!reg.is_available("test-builder").await);

        reg.record_success("test-builder").await;
        assert!(reg.is_available("test-builder").await);
    }
}
