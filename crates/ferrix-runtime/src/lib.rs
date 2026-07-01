//! Runtime orchestration layer for Ferrix.
//!
//! This crate sits above `ferrix-vm`: it loads source or bytecode, invokes the
//! compiler/verifier path, installs the standard library, configures VM output,
//! runs the program, and returns structured request results for tools such as
//! `ferrix-cli`.

pub mod config;
pub mod daemon;
pub mod event;
pub mod extension;
pub mod middleware;
pub mod mode;
pub mod output;
pub mod policy;
pub mod process;
pub mod profile;
pub mod protocol;
pub mod request;
pub mod result;
pub mod service;

pub use config::{RuntimeConfig, RuntimeLogLevel};
pub use daemon::{
    RuntimeCheckpoint, RuntimeDaemon, RuntimeHealth, RuntimeMetricsReport, RuntimeStateStore,
    RuntimeStatusReport, default_config_path, default_ferrix_home, default_runtime_home,
    ensure_default_layout,
};
pub use event::{
    RuntimeEvent, RuntimeEventBus, RuntimeEventBusStats, RuntimeEventKind, RuntimeEventMetadata,
    RuntimeEventSeverity,
};
pub use extension::{
    CustomExtension, CustomExtensionCallResult, CustomExtensionHandler, CustomExtensionMetadata,
    ExtensionCostClass, RuntimeExtensionRegistry,
};
pub use ferrix_vm::{HostCapability, HostCapabilityParseError};
pub use middleware::{
    RuntimeCorrelationId, RuntimeMiddlewareChain, RuntimeRateLimiter, RuntimeRequestContext,
    RuntimeRequestId, RuntimeRequestLogEntry,
};
pub use mode::{
    RuntimeConnection, RuntimeController, RuntimeGateway, RuntimeMode, RuntimeModeParseError,
};
pub use policy::{PolicyFailure, PolicyRule, RuntimePolicy};
pub use process::{
    RuntimeProcessId, RuntimeProcessKind, RuntimeProcessRecord, RuntimeProcessStatus,
    RuntimeProcessTable, RuntimeSessionId,
};
pub use profile::{RuntimeProfile, RuntimeProfileParseError};
pub use protocol::{
    CURRENT_PROTOCOL_VERSION, MAX_SUPPORTED_PROTOCOL_VERSION, MIN_SUPPORTED_PROTOCOL_VERSION,
    RuntimeProtocolInfo, RuntimeProtocolVersion,
};
pub use request::{
    CompileRequest, DebugRequest, InspectBytecodeRequest, OutputMode, RecordProcessRequest,
    RunBytecodeRequest, RunSourceRequest,
};
pub use result::{
    CompileResult, DebugSessionResult, InspectResult, RunResult, RuntimeError, RuntimeErrorKind,
    RuntimeStats,
};
pub use service::{CompiledProgram, RuntimeService};
