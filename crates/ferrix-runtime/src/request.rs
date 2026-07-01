//! Request types accepted by the Ferrix runtime service.

use std::path::PathBuf;

use crate::{RuntimeProcessKind, RuntimeProfile};

/// How VM/native output should be handled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// Capture output and return it in the runtime result.
    #[default]
    Capture,
    /// Discard output.
    Null,
}

/// Request to compile and run a Ferrix source file or package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSourceRequest {
    /// Source file, package directory, or package manifest path.
    pub path: PathBuf,
    /// Runtime profile used to configure VM execution.
    pub profile: RuntimeProfile,
    /// Arguments reserved for future source-level `args` support.
    pub args: Vec<String>,
    /// Whether runtime stats should be populated.
    pub collect_stats: bool,
    /// Whether audit collection is requested. Placeholder for later phases.
    pub collect_audit: bool,
    /// Whether tracing is requested. Placeholder for later phases.
    pub trace: bool,
    /// Output behavior for native functions.
    pub output: OutputMode,
}

impl RunSourceRequest {
    /// Creates a source run request with CLI-friendly defaults.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            profile: RuntimeProfile::Cli,
            args: Vec::new(),
            collect_stats: false,
            collect_audit: false,
            trace: false,
            output: OutputMode::Capture,
        }
    }
}

/// Request to run a serialized Ferrix bytecode file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunBytecodeRequest {
    /// Bytecode file path.
    pub path: PathBuf,
    /// Runtime profile used to configure VM execution.
    pub profile: RuntimeProfile,
    /// Whether runtime stats should be populated.
    pub collect_stats: bool,
    /// Output behavior for native functions.
    pub output: OutputMode,
}

impl RunBytecodeRequest {
    /// Creates a bytecode run request with CLI-friendly defaults.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            profile: RuntimeProfile::Cli,
            collect_stats: false,
            output: OutputMode::Capture,
        }
    }
}

/// Request to compile source without executing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileRequest {
    pub path: PathBuf,
    pub profile: RuntimeProfile,
}

/// Request to inspect a bytecode file without executing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectBytecodeRequest {
    pub path: PathBuf,
}

/// Request to append one CLI command to runtime-owned history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordProcessRequest {
    /// CLI command kind.
    pub kind: RuntimeProcessKind,
    /// File/package/bytecode path associated with the command.
    pub path: PathBuf,
    /// Process-style exit code.
    pub exit_code: i32,
    /// Captured output snapshot, if any.
    pub output: String,
    /// Last rendered error, if the command failed.
    pub last_error: Option<String>,
}

impl RecordProcessRequest {
    /// Creates a history record request.
    pub fn new(kind: RuntimeProcessKind, path: impl Into<PathBuf>, exit_code: i32) -> Self {
        Self {
            kind,
            path: path.into(),
            exit_code,
            output: String::new(),
            last_error: None,
        }
    }

    /// Adds an output snapshot.
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = output.into();
        self
    }

    /// Adds a rendered error snapshot.
    pub fn with_last_error(mut self, error: impl Into<String>) -> Self {
        self.last_error = Some(error.into());
        self
    }
}

/// Request to run a program under a debugger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugRequest {
    pub path: PathBuf,
    pub profile: RuntimeProfile,
}

impl DebugRequest {
    /// Creates a debugger request with CLI-friendly defaults.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            profile: RuntimeProfile::Cli,
        }
    }
}
