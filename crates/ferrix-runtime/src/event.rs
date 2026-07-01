//! In-memory runtime event bus.
//!
//! The daemon uses this as its first internal notification backbone. It is
//! intentionally synchronous and bounded so later socket streaming can be built
//! on top without changing the event vocabulary.

use std::{collections::VecDeque, time::SystemTime};

use crate::{RuntimeProcessId, RuntimeSessionId};

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
