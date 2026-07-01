//! Result and error types returned by the Ferrix runtime service.

use std::{error::Error, fmt, path::PathBuf};

use ferrix_compiler::CompileReport;
use ferrix_core::Value;

use crate::RuntimeMode;

/// Successful source or bytecode execution result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    /// Process-style exit code suggested for CLI callers.
    pub exit_code: i32,
    /// Final VM return value.
    pub value: Value,
    /// Human-readable display for non-nil final values.
    pub value_display: Option<String>,
    /// Captured native output.
    pub output: String,
    /// Runtime stats collected for this run.
    pub stats: RuntimeStats,
    /// Placeholder for later audit/event phases.
    pub audit_events: Vec<String>,
}

/// Runtime-level execution counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeStats {
    /// Number of bytecode instructions executed by the VM.
    pub executed_instructions: usize,
    /// Number of active call frames left after execution.
    pub call_depth: usize,
    /// Maximum call depth observed during execution.
    pub max_call_depth: usize,
    /// Maximum active register count observed during execution.
    pub max_register_count: usize,
    /// Number of heap objects currently owned by the VM.
    pub heap_objects: usize,
    /// Number of successful heap allocations recorded by the VM.
    pub allocations: u64,
    /// Allocation pressure remaining after the latest collection.
    pub allocation_pressure: usize,
    /// Number of GC collections recorded by the VM.
    pub gc_collections: u64,
    /// Number of incremental GC steps recorded by the VM.
    pub incremental_gc_steps: u64,
    /// Number of native host calls executed by the VM.
    pub native_calls: u64,
    /// Number of throw instructions executed by the VM.
    pub thrown_errors: u64,
    /// Number of thrown values caught by an exception handler.
    pub handled_exceptions: u64,
    /// Wall-clock execution duration measured by the runtime.
    pub execution_time_ms: u128,
}

/// Placeholder compile-only result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileResult {
    pub diagnostics: Vec<String>,
    pub report: Option<CompileReport>,
}

/// Placeholder bytecode inspection result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectResult {
    pub diagnostics: Vec<String>,
}

/// Placeholder debug session result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugSessionResult {
    pub diagnostics: Vec<String>,
}

/// Error returned by runtime orchestration.
#[derive(Debug)]
pub struct RuntimeError {
    /// Suggested process exit code for CLI callers.
    pub exit_code: i32,
    /// Machine-readable category.
    pub kind: RuntimeErrorKind,
}

/// Runtime error category with user-facing text.
#[derive(Debug)]
pub enum RuntimeErrorKind {
    /// A source, package, manifest, or bytecode file could not be read.
    Read { path: PathBuf, message: String },
    /// A package manifest was invalid.
    Manifest { path: PathBuf, message: String },
    /// An imported module could not be read.
    ReadImport {
        importer: PathBuf,
        module: String,
        path: PathBuf,
        message: String,
    },
    /// A package-local import could not be resolved.
    PackageImport {
        importer: PathBuf,
        package: String,
        module: String,
        searched: Vec<PathBuf>,
    },
    /// Source compilation or runtime execution produced a rendered diagnostic.
    Diagnostic(String),
    /// Runtime policy rejected a host-visible operation before execution.
    PolicyDenied { message: String },
    /// Import graph loading found a cycle.
    ImportCycle { path: PathBuf },
    /// Bytecode serialization failed to decode.
    DecodeBytecode(String),
    /// Bytecode declares a feature unsupported by this runtime.
    UnsupportedFeature { feature: String },
    /// Custom extension id was not registered.
    MissingExtension { id: String },
    /// Runtime bytecode execution failed without source diagnostics.
    Execution(String),
    /// The selected runtime mode needs a daemon that is not available yet.
    RuntimeUnavailable { mode: RuntimeMode },
    /// A start request targeted a service that is already running.
    ServiceAlreadyRunning,
    /// A stop request targeted a service that is not running.
    ServiceNotRunning,
    /// Runtime daemon state or process metadata operation failed.
    DaemonState { message: String },
}

impl RuntimeError {
    /// Creates a runtime error with an explicit CLI exit code.
    pub fn new(exit_code: i32, kind: RuntimeErrorKind) -> Self {
        Self { exit_code, kind }
    }

    /// Renders the error exactly once for CLI stderr.
    pub fn render(&self) -> String {
        match &self.kind {
            RuntimeErrorKind::Read { path, message } => {
                format!("error: could not read `{}`: {message}\n", path.display())
            }
            RuntimeErrorKind::Manifest { path, message } => {
                format!(
                    "error: invalid package manifest `{}`: {message}\n",
                    path.display()
                )
            }
            RuntimeErrorKind::ReadImport {
                importer,
                module,
                path,
                message,
            } => {
                format!(
                    "error: could not resolve import `{module}` from `{}` as `{}`: {message}\n",
                    importer.display(),
                    path.display()
                )
            }
            RuntimeErrorKind::PackageImport {
                importer,
                package,
                module,
                searched,
            } => {
                let searched = searched
                    .iter()
                    .map(|path| format!("`{}`", path.display()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "error: could not resolve package import `{module}` from `{}` in package `{package}`; searched {searched}\n",
                    importer.display()
                )
            }
            RuntimeErrorKind::Diagnostic(rendered) => rendered.clone(),
            RuntimeErrorKind::PolicyDenied { message } => {
                format!("error: runtime policy denied request: {message}\n")
            }
            RuntimeErrorKind::ImportCycle { path } => {
                format!("error: import cycle involving `{}`\n", path.display())
            }
            RuntimeErrorKind::DecodeBytecode(message) => {
                format!("error: could not decode bytecode: {message}\n")
            }
            RuntimeErrorKind::UnsupportedFeature { feature } => {
                format!("error: unsupported bytecode feature `{feature}`\n")
            }
            RuntimeErrorKind::MissingExtension { id } => {
                format!("error: missing custom extension handler `{id}`\n")
            }
            RuntimeErrorKind::Execution(message) => format!("error: {message}\n"),
            RuntimeErrorKind::RuntimeUnavailable {
                mode: RuntimeMode::Required,
            } => {
                "Ferrix runtime is not running.\nStart it with: ferrix runtime start\n".to_string()
            }
            RuntimeErrorKind::RuntimeUnavailable {
                mode: RuntimeMode::Managed,
            } => {
                "Ferrix managed runtime mode is not available yet.\nUse --runtime-mode embedded for local execution.\n".to_string()
            }
            RuntimeErrorKind::RuntimeUnavailable {
                mode: RuntimeMode::Embedded,
            } => "Ferrix embedded runtime is not available.\n".to_string(),
            RuntimeErrorKind::ServiceAlreadyRunning => "Service is already running\n".to_string(),
            RuntimeErrorKind::ServiceNotRunning => "Service is not running\n".to_string(),
            RuntimeErrorKind::DaemonState { message } => {
                format!("error: runtime daemon state error: {message}\n")
            }
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

impl Error for RuntimeError {}
