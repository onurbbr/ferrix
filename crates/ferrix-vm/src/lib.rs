//! Ferrix bytecode runtime.
//!
//! This crate executes verified bytecode programs, owns heap/GC state,
//! exposes native-function registration, and provides tracing/debugger hooks.

pub mod debug;
pub mod error;
pub mod heap;
pub mod limits;
pub mod native;
pub mod runtime;
pub mod trace;

pub use debug::{DebugAction, DebugEvent, DebugOutcome, Debugger};
pub use error::{VmError, VmErrorKind, VmStackFrame};
pub use heap::{GcStats, Heap, IncrementalGcPhase, RootSet};
pub use limits::RuntimeLimits;
pub use native::{NativeContext, NullOutput, OutputWriter};
pub use runtime::{CallFrame, Vm, VmGcStats};
pub use trace::TraceWriter;
