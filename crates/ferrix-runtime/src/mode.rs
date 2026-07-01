//! Runtime service mode selection and connection policy.
//!
//! The first implementation only executes in embedded mode. The explicit mode
//! layer gives CLI and future tooling a stable place to route daemon-backed
//! execution without rewriting command dispatch.

use std::{error::Error, fmt, path::PathBuf, str::FromStr};

use crate::{
    CompiledProgram, DebugRequest, RecordProcessRequest, RunBytecodeRequest, RunResult,
    RunSourceRequest, RuntimeDaemon, RuntimeError, RuntimeErrorKind, RuntimeProcessId,
    RuntimeProcessKind, RuntimeProcessRecord, RuntimeService,
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

/// Runtime availability controller used before gateway requests are handled.
#[derive(Clone, Debug)]
pub struct RuntimeController {
    mode: RuntimeMode,
    daemon: RuntimeDaemon,
}

/// Runtime connection selected after availability checks.
#[derive(Clone, Debug)]
pub enum RuntimeConnection {
    /// Use a runtime daemon through its socket protocol.
    Socket(RuntimeDaemon),
    /// Use the file-backed daemon facade in the current process.
    Local(RuntimeDaemon),
}

impl RuntimeController {
    /// Creates a controller for one runtime mode and daemon home.
    pub fn new(mode: RuntimeMode, daemon: RuntimeDaemon) -> Self {
        Self { mode, daemon }
    }

    /// Checks whether the selected runtime can handle a request.
    pub fn ensure_available(&mut self) -> Result<(), RuntimeError> {
        self.connect().map(|_| ())
    }

    /// Checks availability and returns the connection that should handle work.
    pub fn connect(&mut self) -> Result<RuntimeConnection, RuntimeError> {
        match self.mode {
            RuntimeMode::Embedded => Ok(RuntimeConnection::Local(self.daemon.clone())),
            RuntimeMode::Required => {
                if self.daemon.ping()? {
                    Ok(RuntimeConnection::Socket(self.daemon.clone()))
                } else {
                    Err(RuntimeError::new(
                        69,
                        RuntimeErrorKind::RuntimeUnavailable {
                            mode: RuntimeMode::Required,
                        },
                    ))
                }
            }
            RuntimeMode::Managed => {
                if self.daemon.ping()? {
                    Ok(RuntimeConnection::Socket(self.daemon.clone()))
                } else {
                    self.daemon.ensure_started()?;
                    Ok(RuntimeConnection::Local(self.daemon.clone()))
                }
            }
        }
    }

    /// Consumes the controller and returns its daemon facade.
    pub fn into_daemon(self) -> RuntimeDaemon {
        self.daemon
    }
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
            RuntimeMode::Embedded => {
                let path = request.path.clone();
                self.record_run_result(
                    RuntimeProcessKind::Run,
                    path,
                    RuntimeService::new().run_source(request),
                )
            }
            RuntimeMode::Required => {
                let mut controller = self.controller();
                match controller.connect()? {
                    RuntimeConnection::Socket(daemon) => daemon.request_run_source(request),
                    RuntimeConnection::Local(mut daemon) => daemon.run_source(request),
                }
            }
            RuntimeMode::Managed => {
                let mut controller = self.controller();
                match controller.connect()? {
                    RuntimeConnection::Socket(daemon) => daemon.request_run_source(request),
                    RuntimeConnection::Local(mut daemon) => daemon.run_source(request),
                }
            }
        }
    }

    /// Runs a bytecode request through the selected runtime connection.
    pub fn run_bytecode(&self, request: RunBytecodeRequest) -> Result<RunResult, RuntimeError> {
        match self.mode {
            RuntimeMode::Embedded => {
                let path = request.path.clone();
                self.record_run_result(
                    RuntimeProcessKind::RunBytecode,
                    path,
                    RuntimeService::new().run_bytecode(request),
                )
            }
            RuntimeMode::Required => {
                let mut controller = self.controller();
                match controller.connect()? {
                    RuntimeConnection::Socket(daemon) => daemon.request_run_bytecode(request),
                    RuntimeConnection::Local(mut daemon) => daemon.run_bytecode(request),
                }
            }
            RuntimeMode::Managed => {
                let mut controller = self.controller();
                match controller.connect()? {
                    RuntimeConnection::Socket(daemon) => daemon.request_run_bytecode(request),
                    RuntimeConnection::Local(mut daemon) => daemon.run_bytecode(request),
                }
            }
        }
    }

    /// Appends a command history record through the runtime layer.
    pub fn record_process(
        &self,
        request: RecordProcessRequest,
    ) -> Result<RuntimeProcessRecord, RuntimeError> {
        let mut controller = self.controller();
        match controller.connect()? {
            RuntimeConnection::Socket(daemon) => daemon.request_record_process(request),
            RuntimeConnection::Local(mut daemon) => daemon.record_cli_process(
                request.kind,
                request.path,
                request.exit_code,
                &request.output,
                request.last_error.as_deref(),
            ),
        }
    }

    /// Lists active process records through the runtime layer.
    pub fn list_processes(&self) -> Result<Vec<RuntimeProcessRecord>, RuntimeError> {
        let mut controller = self.controller();
        match controller.connect()? {
            RuntimeConnection::Socket(daemon) => daemon.request_list_processes(),
            RuntimeConnection::Local(daemon) => daemon.list_processes(),
        }
    }

    /// Lists command history records through the runtime layer.
    pub fn list_logs(&self) -> Result<Vec<RuntimeProcessRecord>, RuntimeError> {
        let mut controller = self.controller();
        match controller.connect()? {
            RuntimeConnection::Socket(daemon) => daemon.request_list_history(),
            RuntimeConnection::Local(daemon) => daemon.list_history(),
        }
    }

    /// Returns one command history record through the runtime layer.
    pub fn process_info(
        &self,
        process_id: RuntimeProcessId,
    ) -> Result<Option<RuntimeProcessRecord>, RuntimeError> {
        let mut controller = self.controller();
        match controller.connect()? {
            RuntimeConnection::Socket(daemon) => daemon.request_process_info(process_id),
            RuntimeConnection::Local(daemon) => daemon.process_info(process_id),
        }
    }

    /// Returns a process output snapshot through the runtime layer.
    pub fn process_output(&self, process_id: RuntimeProcessId) -> Result<String, RuntimeError> {
        let mut controller = self.controller();
        match controller.connect()? {
            RuntimeConnection::Socket(daemon) => daemon.request_logs(process_id),
            RuntimeConnection::Local(daemon) => daemon.logs(process_id),
        }
    }

    /// Kills an active runtime process through the runtime layer.
    pub fn kill_process(
        &self,
        process_id: RuntimeProcessId,
    ) -> Result<RuntimeProcessRecord, RuntimeError> {
        let mut controller = self.controller();
        match controller.connect()? {
            RuntimeConnection::Socket(daemon) => daemon.request_kill_process(process_id),
            RuntimeConnection::Local(mut daemon) => daemon.kill_process(process_id),
        }
    }

    /// Prepares a source program for the interactive debugger.
    pub fn prepare_debug(&self, request: DebugRequest) -> Result<CompiledProgram, RuntimeError> {
        match self.mode {
            RuntimeMode::Embedded => RuntimeService::new().prepare_debug(request),
            RuntimeMode::Required => {
                let mut controller = self.controller();
                controller.ensure_available()?;
                controller.into_daemon().prepare_debug(request)
            }
            RuntimeMode::Managed => {
                let mut controller = self.controller();
                controller.ensure_available()?;
                controller.into_daemon().prepare_debug(request)
            }
        }
    }

    fn record_run_result(
        &self,
        kind: RuntimeProcessKind,
        path: PathBuf,
        result: Result<RunResult, RuntimeError>,
    ) -> Result<RunResult, RuntimeError> {
        match result {
            Ok(result) => {
                let _ = self.record_process(
                    RecordProcessRequest::new(kind, path, result.exit_code)
                        .with_output(run_result_snapshot(&result)),
                );
                Ok(result)
            }
            Err(error) => {
                let rendered = error.render();
                let _ = self.record_process(
                    RecordProcessRequest::new(kind, path, error.exit_code)
                        .with_last_error(rendered),
                );
                Err(error)
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

    fn controller(&self) -> RuntimeController {
        RuntimeController::new(self.mode, self.daemon())
    }
}

fn run_result_snapshot(result: &RunResult) -> String {
    let mut snapshot = result.output.clone();
    if let Some(value) = &result.value_display {
        snapshot.push_str(value);
        snapshot.push('\n');
    }
    snapshot
}
