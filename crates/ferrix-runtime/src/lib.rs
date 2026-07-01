//! Runtime orchestration layer for Ferrix.
//!
//! This crate sits above `ferrix-vm`: it loads source or bytecode, invokes the
//! compiler/verifier path, installs the standard library, configures VM output,
//! runs the program, and returns structured request results for tools such as
//! `ferrix-cli`.

pub mod output;
pub mod profile;
pub mod request;
pub mod result;
pub mod service;

pub use profile::RuntimeProfile;
pub use request::{
    CompileRequest, DebugRequest, InspectBytecodeRequest, OutputMode, RunBytecodeRequest,
    RunSourceRequest,
};
pub use result::{
    CompileResult, DebugSessionResult, InspectResult, RunResult, RuntimeError, RuntimeErrorKind,
    RuntimeStats,
};
pub use service::{CompiledProgram, RuntimeService};
