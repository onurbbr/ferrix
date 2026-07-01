//! In-memory runtime event bus.
//!
//! The daemon uses this as its first internal notification backbone. It is
//! intentionally synchronous and bounded so later socket streaming can be built
//! on top without changing the event vocabulary.

use std::{collections::VecDeque, time::SystemTime};

use ferrix_core::diagnostics::SourceSpan;

use crate::{RuntimeProcessId, RuntimeSessionId};

/// Runtime event severity used by audit, status, and compact CLI output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeEventSeverity {
    /// Informational lifecycle or progress event.
    #[default]
    Info,
    /// Potentially surprising behavior that did not fail the request.
    Warn,
    /// Failed or denied runtime behavior.
    Error,
}

impl RuntimeEventSeverity {
    /// Returns the stable lowercase severity name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Runtime event category recorded by the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeEventKind {
    /// Runtime service entered serving state.
    RuntimeStarted,
    /// Runtime service entered stopped state.
    RuntimeStopped,
    /// A runtime process was allocated.
    ProcessStarted,
    /// A runtime process completed successfully.
    ProcessCompleted,
    /// A runtime process failed.
    ProcessFailed,
    /// A runtime process was marked killed.
    ProcessKilled,
    /// Debugger preparation or attachment was requested.
    DebuggerAttached,
    /// Runtime profile was selected for a process.
    ProfileSelected(String),
    /// A lightweight checkpoint was recorded.
    CheckpointRecorded,
    /// Runtime audit entry emitted by policy, VM, or service layers.
    AuditEvent(String),
    /// A source or bytecode program started execution.
    ProgramStarted,
    /// A source or bytecode program completed execution.
    ProgramCompleted,
    /// A source or bytecode program failed execution.
    ProgramFailed,
    /// A native host function was called.
    NativeFunctionCalled(String),
    /// Runtime policy or VM capability enforcement denied an operation.
    CapabilityDenied(String),
    /// An exception was thrown by bytecode.
    ExceptionThrown,
    /// A bytecode exception handler caught a thrown value.
    ExceptionHandled,
    /// A static module was loaded by the compiler/runtime pipeline.
    ModuleLoaded(String),
    /// A garbage collection pass started.
    GcStarted,
    /// A garbage collection pass completed.
    GcCompleted,
    /// Debugger stopped at a breakpoint.
    DebuggerBreakpointHit,
    /// A custom host extension was called.
    CustomExtensionCalled(String),
    /// Instruction budget was exceeded.
    InstructionBudgetExceeded,
}

/// Optional metadata attached to one runtime event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeEventMetadata {
    /// Event severity.
    pub severity: RuntimeEventSeverity,
    /// Optional human-readable event message.
    pub message: Option<String>,
    /// Optional source span related to the event.
    pub source_span: Option<SourceSpan>,
    /// Optional function name related to the event.
    pub function_name: Option<String>,
    /// Optional module name related to the event.
    pub module_name: Option<String>,
}

impl RuntimeEventMetadata {
    /// Creates metadata with a severity and no optional fields.
    pub fn new(severity: RuntimeEventSeverity) -> Self {
        Self {
            severity,
            ..Self::default()
        }
    }

    /// Attaches a compact human-readable message.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

/// Event item stored in the bounded event queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeEvent {
    /// Monotonic event id inside one bus instance.
    pub id: u64,
    /// Wall-clock event timestamp in milliseconds since the Unix epoch.
    pub timestamp_ms: u128,
    /// Process related to this event, when applicable.
    pub process_id: Option<RuntimeProcessId>,
    /// Session related to this event, when applicable.
    pub session_id: Option<RuntimeSessionId>,
    /// Typed event category.
    pub kind: RuntimeEventKind,
    /// Severity and optional context for this event.
    pub metadata: RuntimeEventMetadata,
}

/// Snapshot of bounded event queue usage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeEventBusStats {
    /// Number of retained events.
    pub len: usize,
    /// Maximum retained events before oldest-event dropping begins.
    pub capacity: usize,
    /// Number of events discarded because the queue reached capacity.
    pub dropped_events: u64,
}

/// Bounded synchronous event queue.
#[derive(Clone, Debug)]
pub struct RuntimeEventBus {
    capacity: usize,
    next_id: u64,
    dropped_events: u64,
    events: VecDeque<RuntimeEvent>,
}

impl RuntimeEventBus {
    /// Creates an event bus with a fixed capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            next_id: 1,
            dropped_events: 0,
            events: VecDeque::new(),
        }
    }

    /// Records a new event and drops the oldest one when the queue is full.
    pub fn publish(
        &mut self,
        kind: RuntimeEventKind,
        process_id: Option<RuntimeProcessId>,
        session_id: Option<RuntimeSessionId>,
    ) -> RuntimeEvent {
        self.publish_event(
            kind,
            process_id,
            session_id,
            RuntimeEventMetadata::default(),
        )
    }

    /// Records a new event with metadata and drops the oldest one when full.
    pub fn publish_event(
        &mut self,
        kind: RuntimeEventKind,
        process_id: Option<RuntimeProcessId>,
        session_id: Option<RuntimeSessionId>,
        metadata: RuntimeEventMetadata,
    ) -> RuntimeEvent {
        if self.events.len() == self.capacity {
            self.events.pop_front();
            self.dropped_events += 1;
        }

        let event = RuntimeEvent {
            id: self.next_id,
            timestamp_ms: timestamp_ms(),
            process_id,
            session_id,
            kind,
            metadata,
        };
        self.next_id += 1;
        self.events.push_back(event.clone());
        event
    }

    /// Returns all retained events in publish order.
    pub fn events(&self) -> Vec<RuntimeEvent> {
        self.events.iter().cloned().collect()
    }

    /// Returns retained events for one process.
    pub fn events_for_process(&self, process_id: RuntimeProcessId) -> Vec<RuntimeEvent> {
        self.events
            .iter()
            .filter(|event| event.process_id == Some(process_id))
            .cloned()
            .collect()
    }

    /// Returns retained events for one session.
    pub fn events_for_session(&self, session_id: RuntimeSessionId) -> Vec<RuntimeEvent> {
        self.events
            .iter()
            .filter(|event| event.session_id == Some(session_id))
            .cloned()
            .collect()
    }

    /// Returns bounded queue usage counters.
    pub fn stats(&self) -> RuntimeEventBusStats {
        RuntimeEventBusStats {
            len: self.events.len(),
            capacity: self.capacity,
            dropped_events: self.dropped_events,
        }
    }

    /// Number of events discarded because the queue reached capacity.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }
}

impl Default for RuntimeEventBus {
    fn default() -> Self {
        Self::with_capacity(256)
    }
}

pub(crate) fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}
