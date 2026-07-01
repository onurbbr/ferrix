//! Runtime request middleware.
//!
//! The middleware chain is deliberately small and synchronous. It gives daemon
//! requests identity, validates protocol compatibility, applies a global token
//! bucket, measures timeout violations, and retains lightweight request logs.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    RuntimeError, RuntimeErrorKind,
    event::timestamp_ms,
    protocol::{CURRENT_PROTOCOL_VERSION, RuntimeProtocolVersion},
};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Runtime request id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeRequestId(pub u64);

impl std::fmt::Display for RuntimeRequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Correlation id shared by logs, process records, and audit events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeCorrelationId(pub u64);

impl std::fmt::Display for RuntimeCorrelationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Per-request middleware context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRequestContext {
    /// Unique request id.
    pub request_id: RuntimeRequestId,
    /// Correlation id used across related records.
    pub correlation_id: RuntimeCorrelationId,
    /// Daemon command name.
    pub command: String,
    /// Client protocol version.
    pub protocol_version: RuntimeProtocolVersion,
    /// Request start timestamp.
    pub started_at_ms: u128,
    /// Timeout budget in milliseconds.
    pub timeout_ms: u64,
}

/// Retained middleware log entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRequestLogEntry {
    /// Request id.
    pub request_id: RuntimeRequestId,
    /// Correlation id.
    pub correlation_id: RuntimeCorrelationId,
    /// Command name.
    pub command: String,
    /// Outcome label.
    pub outcome: String,
    /// Elapsed request time.
    pub elapsed_ms: u128,
}

/// Simple fixed-window token bucket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRateLimiter {
    limit_per_second: u32,
    window_started_at_ms: u128,
    used_in_window: u32,
}

impl RuntimeRateLimiter {
    /// Creates a rate limiter with a global per-second limit.
    pub fn new(limit_per_second: u32) -> Self {
        Self {
            limit_per_second,
            window_started_at_ms: 0,
            used_in_window: 0,
        }
    }

    /// Returns true when the request can proceed.
    pub fn allow(&mut self, now_ms: u128) -> bool {
        if self.limit_per_second == 0 {
            return false;
        }
        if now_ms.saturating_sub(self.window_started_at_ms) >= 1_000 {
            self.window_started_at_ms = now_ms;
            self.used_in_window = 0;
        }
        if self.used_in_window >= self.limit_per_second {
            return false;
        }
        self.used_in_window += 1;
        true
    }

    /// Returns the configured request limit.
    pub fn limit_per_second(&self) -> u32 {
        self.limit_per_second
    }
}

/// Runtime middleware chain state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeMiddlewareChain {
    timeout_ms: u64,
    rate_limiter: RuntimeRateLimiter,
    logs: Vec<RuntimeRequestLogEntry>,
}

impl RuntimeMiddlewareChain {
    /// Creates middleware with timeout and rate-limit settings.
    pub fn new(timeout_ms: u64, rate_limit_per_second: u32) -> Self {
        Self {
            timeout_ms,
            rate_limiter: RuntimeRateLimiter::new(rate_limit_per_second),
            logs: Vec::new(),
        }
    }

    /// Starts middleware handling and returns request context.
    pub fn begin(
        &mut self,
        command: impl Into<String>,
        protocol_version: RuntimeProtocolVersion,
    ) -> Result<RuntimeRequestContext, RuntimeError> {
        validate_protocol(protocol_version)?;
        let now = timestamp_ms();
        if !self.rate_limiter.allow(now) {
            return Err(RuntimeError::new(
                70,
                RuntimeErrorKind::RateLimited {
                    limit_per_second: self.rate_limiter.limit_per_second(),
                },
            ));
        }
        let request_id = RuntimeRequestId(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed));
        Ok(RuntimeRequestContext {
            request_id,
            correlation_id: RuntimeCorrelationId(request_id.0),
            command: command.into(),
            protocol_version,
            started_at_ms: now,
            timeout_ms: self.timeout_ms,
        })
    }

    /// Finishes a request and records a middleware log entry.
    pub fn finish(
        &mut self,
        context: &RuntimeRequestContext,
        outcome: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let elapsed_ms = timestamp_ms().saturating_sub(context.started_at_ms);
        let outcome = outcome.into();
        self.logs.push(RuntimeRequestLogEntry {
            request_id: context.request_id,
            correlation_id: context.correlation_id,
            command: context.command.clone(),
            outcome,
            elapsed_ms,
        });
        if elapsed_ms > u128::from(context.timeout_ms) {
            return Err(RuntimeError::new(
                70,
                RuntimeErrorKind::RequestTimeout {
                    timeout_ms: context.timeout_ms,
                },
            ));
        }
        Ok(())
    }

    /// Returns retained request logs.
    pub fn logs(&self) -> &[RuntimeRequestLogEntry] {
        &self.logs
    }
}

impl Default for RuntimeMiddlewareChain {
    fn default() -> Self {
        Self::new(30_000, 64)
    }
}

fn validate_protocol(protocol_version: RuntimeProtocolVersion) -> Result<(), RuntimeError> {
    if protocol_version.major == CURRENT_PROTOCOL_VERSION.major {
        return Ok(());
    }
    Err(RuntimeError::new(
        70,
        RuntimeErrorKind::ProtocolMismatch {
            cli_supported: CURRENT_PROTOCOL_VERSION.to_string(),
            daemon_protocol: protocol_version.to_string(),
        },
    ))
}
