//! Runtime service mode selection and connection policy.
//!
//! The first implementation only executes in embedded mode. The explicit mode
//! layer gives CLI and future tooling a stable place to route daemon-backed
//! execution without rewriting command dispatch.

use std::{error::Error, fmt, path::PathBuf, str::FromStr};

use crate::{
    CompiledProgram, DebugRequest, RunBytecodeRequest, RunResult, RunSourceRequest, RuntimeDaemon,
    RuntimeError, RuntimeService,
};

/// Runtime service mode requested by a caller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeMode {
    /// Create and use an in-process runtime service.
    #[default]
    Embedded,
    /// Require an already-running runtime daemon.
    Required,
    /// Start or connect to a runtime daemon automatically.
    Managed,
}

impl RuntimeMode {
    /// Returns the stable lowercase mode name used by configuration.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Required => "required",
            Self::Managed => "managed",
        }
    }
}

impl fmt::Display for RuntimeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RuntimeMode {
    type Err = RuntimeModeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "embedded" => Ok(Self::Embedded),
            "required" => Ok(Self::Required),
            "managed" => Ok(Self::Managed),
            _ => Err(RuntimeModeParseError {
                value: value.to_string(),
            }),
        }
    }
}

/// Error returned when runtime mode configuration is not recognized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeModeParseError {
    value: String,
}

impl RuntimeModeParseError {
    /// Returns the invalid raw configuration value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for RuntimeModeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid runtime mode `{}`; expected embedded, required, or managed",
            self.value
        )
    }
}

impl Error for RuntimeModeParseError {}

/// Mode-aware runtime gateway used by CLI execution commands.
#[derive(Clone, Debug, Default)]
pub struct RuntimeGateway {
    mode: RuntimeMode,
    home: Option<PathBuf>,
}

impl RuntimeGateway {
    /// Creates a gateway with the selected runtime mode.
    pub fn new(mode: RuntimeMode) -> Self {
        Self { mode, home: None }
    }

    /// Creates a gateway with an explicit daemon home.
    pub fn with_home(mode: RuntimeMode, home: impl Into<PathBuf>) -> Self {
        Self {
            mode,
            home: Some(home.into()),
        }
    }

    /// Creates a gateway using the default embedded runtime mode.
    pub fn embedded() -> Self {
        Self::new(RuntimeMode::Embedded)
    }

    /// Returns the selected runtime mode.
    pub fn mode(&self) -> RuntimeMode {
        self.mode
    }

    /// Runs a source request through the selected runtime connection.
    pub fn run_source(&self, request: RunSourceRequest) -> Result<RunResult, RuntimeError> {
        match self.mode {
            RuntimeMode::Embedded => RuntimeService::new().run_source(request),
            RuntimeMode::Required => self.daemon().run_source(request),
            RuntimeMode::Managed => {
                let mut daemon = self.daemon();
                daemon.ensure_started()?;
                daemon.run_source(request)
            }
        }
    }

    /// Runs a bytecode request through the selected runtime connection.
    pub fn run_bytecode(&self, request: RunBytecodeRequest) -> Result<RunResult, RuntimeError> {
        match self.mode {
            RuntimeMode::Embedded => RuntimeService::new().run_bytecode(request),
            RuntimeMode::Required => self.daemon().run_bytecode(request),
            RuntimeMode::Managed => {
                let mut daemon = self.daemon();
                daemon.ensure_started()?;
                daemon.run_bytecode(request)
            }
        }
    }

    /// Prepares a source program for the interactive debugger.
    pub fn prepare_debug(&self, request: DebugRequest) -> Result<CompiledProgram, RuntimeError> {
        match self.mode {
            RuntimeMode::Embedded => RuntimeService::new().prepare_debug(request),
            RuntimeMode::Required => self.daemon().prepare_debug(request),
            RuntimeMode::Managed => {
                let mut daemon = self.daemon();
                daemon.ensure_started()?;
                daemon.prepare_debug(request)
            }
        }
    }

    fn daemon(&self) -> RuntimeDaemon {
        if let Some(home) = &self.home {
            RuntimeDaemon::with_home(home.clone())
        } else {
            RuntimeDaemon::new()
        }
    }
}
