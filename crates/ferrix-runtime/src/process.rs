//! Runtime process and session metadata.

use std::{fmt, path::PathBuf};

use crate::{
    RuntimeProfile, RuntimeStats,
    event::timestamp_ms,
    middleware::{RuntimeCorrelationId, RuntimeRequestId},
};

/// Runtime-owned process id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeProcessId(pub u64);

impl fmt::Display for RuntimeProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Runtime-owned session id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeSessionId(pub u64);

impl fmt::Display for RuntimeSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Kind of execution represented by a process record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeProcessKind {
    /// Source file or package execution via `ferrix run`.
    Run,
    /// Source validation via `ferrix check`.
    Check,
    /// Source-to-bytecode compilation via `ferrix compile`.
    Compile,
    /// Serialized bytecode execution via `ferrix run-bytecode`.
    RunBytecode,
    /// Interactive source debugging via `ferrix debug`.
    Debug,
}

impl RuntimeProcessKind {
    /// Returns the stable lowercase name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Check => "check",
            Self::Compile => "compile",
            Self::RunBytecode => "run-bytecode",
            Self::Debug => "debug",
        }
    }
}

/// Lifecycle state for a runtime process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeProcessStatus {
    /// Process record exists but execution has not entered the VM yet.
    Starting,
    /// Process is running.
    Running,
    /// Process is paused by debugger or future scheduler support.
    Paused,
    /// Process completed successfully.
    Completed,
    /// Process failed with diagnostics or runtime error.
    Failed,
    /// Process was explicitly killed.
    Killed,
}

impl RuntimeProcessStatus {
    /// Returns the stable lowercase name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
        }
    }

    /// Parses a status name written by the daemon state store.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "starting" => Some(Self::Starting),
            "running" => Some(Self::Running),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "killed" => Some(Self::Killed),
            _ => None,
        }
    }

    /// Returns true for processes that are currently live.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Paused)
    }
}

/// Runtime process metadata stored by the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeProcessRecord {
    /// Runtime process id.
    pub id: RuntimeProcessId,
    /// Middleware request id that created this process.
    pub request_id: RuntimeRequestId,
    /// Correlation id shared by logs, audit events, and metrics.
    pub correlation_id: RuntimeCorrelationId,
    /// Optional parent process id.
    pub parent_id: Option<RuntimeProcessId>,
    /// Runtime session id.
    pub session_id: RuntimeSessionId,
    /// Current process lifecycle status.
    pub status: RuntimeProcessStatus,
    /// Source or bytecode execution kind.
    pub kind: RuntimeProcessKind,
    /// Runtime profile selected for this process.
    pub profile: RuntimeProfile,
    /// Input path used to create the process.
    pub path: PathBuf,
    /// Command arguments reserved for future language args support.
    pub args: Vec<String>,
    /// Start timestamp in milliseconds since Unix epoch.
    pub started_at_ms: u128,
    /// End timestamp in milliseconds since Unix epoch.
    pub ended_at_ms: Option<u128>,
    /// Process exit code when known.
    pub exit_code: Option<i32>,
    /// Latest captured runtime stats.
    pub stats: RuntimeStats,
    /// Last rendered error, if the process failed.
    pub last_error: Option<String>,
}

impl RuntimeProcessRecord {
    /// Creates a starting process record.
    pub fn starting(
        id: RuntimeProcessId,
        session_id: RuntimeSessionId,
        kind: RuntimeProcessKind,
        profile: RuntimeProfile,
        path: PathBuf,
        args: Vec<String>,
    ) -> Self {
        Self {
            id,
            request_id: RuntimeRequestId(id.0),
            correlation_id: RuntimeCorrelationId(session_id.0),
            parent_id: None,
            session_id,
            status: RuntimeProcessStatus::Starting,
            kind,
            profile,
            path,
            args,
            started_at_ms: timestamp_ms(),
            ended_at_ms: None,
            exit_code: None,
            stats: RuntimeStats::default(),
            last_error: None,
        }
    }

    /// Marks this process as running.
    pub fn mark_running(&mut self) {
        self.status = RuntimeProcessStatus::Running;
    }

    /// Marks this process as completed.
    pub fn mark_completed(&mut self, exit_code: i32, stats: RuntimeStats) {
        self.status = RuntimeProcessStatus::Completed;
        self.ended_at_ms = Some(timestamp_ms());
        self.exit_code = Some(exit_code);
        self.stats = stats;
        self.last_error = None;
    }

    /// Marks this process as failed.
    pub fn mark_failed(&mut self, exit_code: i32, error: String) {
        self.status = RuntimeProcessStatus::Failed;
        self.ended_at_ms = Some(timestamp_ms());
        self.exit_code = Some(exit_code);
        self.last_error = Some(error);
    }

    /// Marks this process as killed.
    pub fn mark_killed(&mut self) {
        self.status = RuntimeProcessStatus::Killed;
        self.ended_at_ms = Some(timestamp_ms());
        self.exit_code = Some(137);
    }
}

/// In-memory process table.
#[derive(Clone, Debug, Default)]
pub struct RuntimeProcessTable {
    next_process_id: u64,
    next_session_id: u64,
    records: Vec<RuntimeProcessRecord>,
}

impl RuntimeProcessTable {
    /// Creates an empty process table.
    pub fn new() -> Self {
        Self {
            next_process_id: 1,
            next_session_id: 1,
            records: Vec::new(),
        }
    }

    /// Allocates and inserts a starting process.
    pub fn start_process(
        &mut self,
        kind: RuntimeProcessKind,
        profile: RuntimeProfile,
        path: PathBuf,
        args: Vec<String>,
    ) -> RuntimeProcessRecord {
        let id = RuntimeProcessId(self.next_process_id);
        let session_id = RuntimeSessionId(self.next_session_id);
        self.next_process_id += 1;
        self.next_session_id += 1;
        let record = RuntimeProcessRecord::starting(id, session_id, kind, profile, path, args);
        self.records.push(record.clone());
        record
    }

    /// Replaces an existing process record.
    pub fn update(&mut self, record: RuntimeProcessRecord) {
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|existing| existing.id == record.id)
        {
            *existing = record;
        }
    }

    /// Returns all process records in insertion order.
    pub fn list(&self) -> Vec<RuntimeProcessRecord> {
        self.records.clone()
    }

    /// Returns one process record.
    pub fn get(&self, id: RuntimeProcessId) -> Option<RuntimeProcessRecord> {
        self.records.iter().find(|record| record.id == id).cloned()
    }
}
