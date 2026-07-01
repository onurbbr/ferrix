//! Runtime daemon and session manager.
//!
//! This is the first daemon-shaped runtime layer. It persists a small local
//! state store so separate CLI invocations can observe daemon status, completed
//! process records, logs, and lightweight checkpoints. A socket protocol can
//! later replace the direct file-backed client boundary without changing the
//! public daemon concepts.

use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::{
    DebugRequest, RunBytecodeRequest, RunResult, RunSourceRequest, RuntimeError, RuntimeErrorKind,
    RuntimeEventBus, RuntimeEventKind, RuntimeMode, RuntimeProcessId, RuntimeProcessKind,
    RuntimeProcessRecord, RuntimeProcessStatus, RuntimeProcessTable, RuntimeProfile,
    RuntimeSessionId, RuntimeStats, event::timestamp_ms, service::CompiledProgram,
};

const RUNTIME_HOME_ENV: &str = "FERRIX_RUNTIME_HOME";
const DAEMON_STATE_FILE: &str = "daemon.state";
const NEXT_PROCESS_FILE: &str = "next-process-id";
const PROCESS_DIR: &str = "processes";
const LOG_DIR: &str = "logs";
const CHECKPOINT_FILE: &str = "checkpoints.log";

/// Runtime daemon health state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeHealth {
    /// Daemon is starting.
    Starting,
    /// Daemon is serving requests.
    Serving,
    /// Daemon is serving with reduced capability.
    Degraded,
    /// Daemon is stopping.
    Stopping,
    /// Daemon is stopped.
    Stopped,
}

impl RuntimeHealth {
    /// Returns the stable lowercase state name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Serving => "serving",
            Self::Degraded => "degraded",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "starting" => Self::Starting,
            "serving" => Self::Serving,
            "degraded" => Self::Degraded,
            "stopping" => Self::Stopping,
            "stopped" => Self::Stopped,
            _ => Self::Stopped,
        }
    }
}

/// Daemon status snapshot returned to CLI and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeStatusReport {
    /// Health state.
    pub health: RuntimeHealth,
    /// Runtime crate version.
    pub version: String,
    /// Milliseconds since the daemon was started.
    pub uptime_ms: Option<u128>,
    /// Number of active process records.
    pub active_process_count: usize,
    /// Number of completed process records.
    pub completed_process_count: usize,
    /// Number of failed process records.
    pub failed_process_count: usize,
    /// Total process records retained in the state store.
    pub process_count: usize,
    /// Default runtime mode used by this daemon-shaped layer.
    pub default_mode: RuntimeMode,
    /// Placeholder bytecode cache size.
    pub bytecode_cache_size: usize,
    /// Placeholder module cache size.
    pub module_cache_size: usize,
    /// Last runtime error, if one was recorded.
    pub last_runtime_error: Option<String>,
}

impl RuntimeStatusReport {
    /// Returns true when the daemon can accept execution requests.
    pub fn is_serving(&self) -> bool {
        self.health == RuntimeHealth::Serving
    }
}

/// Lightweight checkpoint metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeCheckpoint {
    /// Monotonic checkpoint id.
    pub id: u64,
    /// Process that produced the checkpoint.
    pub process_id: RuntimeProcessId,
    /// Session that produced the checkpoint.
    pub session_id: RuntimeSessionId,
    /// Human-readable checkpoint label.
    pub label: String,
    /// Timestamp in milliseconds since Unix epoch.
    pub timestamp_ms: u128,
    /// Exit code recorded with the checkpoint.
    pub exit_code: i32,
    /// Output snapshot retained for `logs` and later debugger tooling.
    pub output_snapshot: String,
}

/// Small in-memory state store for daemon metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeStateStore {
    entries: BTreeMap<String, String>,
}

impl RuntimeStateStore {
    /// Inserts or replaces one metadata value.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.entries.insert(key.into(), value.into());
    }

    /// Reads one metadata value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }

    /// Number of stored metadata entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when no metadata entries exist.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// File-backed runtime daemon facade.
#[derive(Clone, Debug)]
pub struct RuntimeDaemon {
    home: PathBuf,
    events: RuntimeEventBus,
    memory_table: RuntimeProcessTable,
    state: RuntimeStateStore,
}

impl RuntimeDaemon {
    /// Creates a daemon facade rooted at the default runtime home.
    pub fn new() -> Self {
        Self::with_home(default_runtime_home())
    }

    /// Creates a daemon facade rooted at a specific runtime home.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        let mut state = RuntimeStateStore::default();
        state.set("version", env!("CARGO_PKG_VERSION"));
        Self {
            home: home.into(),
            events: RuntimeEventBus::default(),
            memory_table: RuntimeProcessTable::new(),
            state,
        }
    }

    /// Returns the daemon home directory.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Starts the daemon-shaped runtime service.
    pub fn start(&mut self) -> Result<RuntimeStatusReport, RuntimeError> {
        self.ensure_layout()?;
        write_key_values(
            &self.state_path(),
            &[
                ("health", RuntimeHealth::Serving.as_str().to_string()),
                ("version", env!("CARGO_PKG_VERSION").to_string()),
                ("started_at_ms", timestamp_ms().to_string()),
                ("last_runtime_error", String::new()),
            ],
        )?;
        self.state.set("health", RuntimeHealth::Serving.as_str());
        self.events
            .publish(RuntimeEventKind::RuntimeStarted, None, None);
        self.status()
    }

    /// Stops the daemon-shaped runtime service.
    pub fn stop(&mut self) -> Result<RuntimeStatusReport, RuntimeError> {
        self.ensure_layout()?;
        let started_at = read_key_values(&self.state_path())?
            .remove("started_at_ms")
            .unwrap_or_else(|| timestamp_ms().to_string());
        write_key_values(
            &self.state_path(),
            &[
                ("health", RuntimeHealth::Stopped.as_str().to_string()),
                ("version", env!("CARGO_PKG_VERSION").to_string()),
                ("started_at_ms", started_at),
                ("last_runtime_error", String::new()),
            ],
        )?;
        self.state.set("health", RuntimeHealth::Stopped.as_str());
        self.events
            .publish(RuntimeEventKind::RuntimeStopped, None, None);
        self.status()
    }

    /// Restarts the daemon-shaped runtime service.
    pub fn restart(&mut self) -> Result<RuntimeStatusReport, RuntimeError> {
        let _ = self.stop();
        self.start()
    }

    /// Starts the daemon only when it is not already serving.
    pub fn ensure_started(&mut self) -> Result<RuntimeStatusReport, RuntimeError> {
        let status = self.status()?;
        if status.is_serving() {
            Ok(status)
        } else {
            self.start()
        }
    }

    /// Returns the current daemon status.
    pub fn status(&self) -> Result<RuntimeStatusReport, RuntimeError> {
        let state = read_key_values(&self.state_path())?;
        let health = state
            .get("health")
            .map_or(RuntimeHealth::Stopped, |value| RuntimeHealth::parse(value));
        let started_at_ms = state
            .get("started_at_ms")
            .and_then(|value| value.parse::<u128>().ok());
        let processes = self.list_processes()?;
        let active_process_count = processes
            .iter()
            .filter(|process| {
                matches!(
                    process.status,
                    RuntimeProcessStatus::Starting
                        | RuntimeProcessStatus::Running
                        | RuntimeProcessStatus::Paused
                )
            })
            .count();
        let completed_process_count = processes
            .iter()
            .filter(|process| process.status == RuntimeProcessStatus::Completed)
            .count();
        let failed_process_count = processes
            .iter()
            .filter(|process| process.status == RuntimeProcessStatus::Failed)
            .count();
        let uptime_ms = if health == RuntimeHealth::Serving {
            started_at_ms.map(|started| timestamp_ms().saturating_sub(started))
        } else {
            None
        };

        Ok(RuntimeStatusReport {
            health,
            version: state
                .get("version")
                .cloned()
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
            uptime_ms,
            active_process_count,
            completed_process_count,
            failed_process_count,
            process_count: processes.len(),
            default_mode: RuntimeMode::Required,
            bytecode_cache_size: 0,
            module_cache_size: 0,
            last_runtime_error: state
                .get("last_runtime_error")
                .filter(|value| !value.is_empty())
                .cloned(),
        })
    }

    /// Runs source through a serving daemon and records process metadata.
    pub fn run_source(&mut self, mut request: RunSourceRequest) -> Result<RunResult, RuntimeError> {
        self.require_serving()?;
        request.collect_stats = true;
        let record = self.start_process(
            RuntimeProcessKind::Source,
            request.profile,
            request.path.clone(),
            request.args.clone(),
        )?;
        let result = crate::RuntimeService::new().run_source(request);
        self.finish_process(record, result)
    }

    /// Runs bytecode through a serving daemon and records process metadata.
    pub fn run_bytecode(
        &mut self,
        mut request: RunBytecodeRequest,
    ) -> Result<RunResult, RuntimeError> {
        self.require_serving()?;
        request.collect_stats = true;
        let record = self.start_process(
            RuntimeProcessKind::Bytecode,
            request.profile,
            request.path.clone(),
            Vec::new(),
        )?;
        let result = crate::RuntimeService::new().run_bytecode(request);
        self.finish_process(record, result)
    }

    /// Prepares a debugger program through a serving daemon.
    pub fn prepare_debug(
        &mut self,
        request: DebugRequest,
    ) -> Result<CompiledProgram, RuntimeError> {
        self.require_serving()?;
        self.events
            .publish(RuntimeEventKind::DebuggerAttached, None, None);
        crate::RuntimeService::new().prepare_debug(request)
    }

    /// Lists persisted process records.
    pub fn list_processes(&self) -> Result<Vec<RuntimeProcessRecord>, RuntimeError> {
        let dir = self.process_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(&dir).map_err(runtime_io_error)? {
            let path = entry.map_err(runtime_io_error)?.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("state")
                && let Some(record) = read_process_record(&path)?
            {
                records.push(record);
            }
        }
        records.sort_by_key(|record| record.id);
        Ok(records)
    }

    /// Returns logs captured for one process.
    pub fn logs(&self, process_id: RuntimeProcessId) -> Result<String, RuntimeError> {
        fs::read_to_string(self.log_path(process_id)).map_err(|error| {
            RuntimeError::new(
                66,
                RuntimeErrorKind::DaemonState {
                    message: format!("could not read logs for process {process_id}: {error}"),
                },
            )
        })
    }

    /// Marks a process as killed.
    pub fn kill_process(
        &mut self,
        process_id: RuntimeProcessId,
    ) -> Result<RuntimeProcessRecord, RuntimeError> {
        self.ensure_layout()?;
        let path = self.process_path(process_id);
        let Some(mut record) = read_process_record(&path)? else {
            return Err(RuntimeError::new(
                66,
                RuntimeErrorKind::DaemonState {
                    message: format!("unknown runtime process {process_id}"),
                },
            ));
        };
        record.mark_killed();
        write_process_record(&path, &record)?;
        self.events.publish(
            RuntimeEventKind::ProcessKilled,
            Some(record.id),
            Some(record.session_id),
        );
        Ok(record)
    }

    /// Returns retained in-memory events for this daemon instance.
    pub fn events(&self) -> Vec<crate::RuntimeEvent> {
        self.events.events()
    }

    /// Returns the daemon state store.
    pub fn state(&self) -> &RuntimeStateStore {
        &self.state
    }

    /// Returns persisted checkpoints.
    pub fn checkpoints(&self) -> Result<Vec<RuntimeCheckpoint>, RuntimeError> {
        let path = self.checkpoint_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let source = fs::read_to_string(path).map_err(runtime_io_error)?;
        Ok(source.lines().filter_map(parse_checkpoint).collect())
    }

    fn require_serving(&self) -> Result<(), RuntimeError> {
        if self.status()?.is_serving() {
            Ok(())
        } else {
            Err(RuntimeError::new(
                69,
                RuntimeErrorKind::RuntimeUnavailable {
                    mode: RuntimeMode::Required,
                },
            ))
        }
    }

    fn start_process(
        &mut self,
        kind: RuntimeProcessKind,
        profile: RuntimeProfile,
        path: PathBuf,
        args: Vec<String>,
    ) -> Result<RuntimeProcessRecord, RuntimeError> {
        self.ensure_layout()?;
        let id = self.allocate_process_id()?;
        let session_id = RuntimeSessionId(id.0);
        let mut record = RuntimeProcessRecord::starting(id, session_id, kind, profile, path, args);
        record.mark_running();
        write_process_record(&self.process_path(record.id), &record)?;
        self.memory_table.update(record.clone());
        self.events.publish(
            RuntimeEventKind::ProcessStarted,
            Some(record.id),
            Some(record.session_id),
        );
        self.events.publish(
            RuntimeEventKind::ProfileSelected(record.profile.as_str().to_string()),
            Some(record.id),
            Some(record.session_id),
        );
        Ok(record)
    }

    fn finish_process(
        &mut self,
        mut record: RuntimeProcessRecord,
        result: Result<RunResult, RuntimeError>,
    ) -> Result<RunResult, RuntimeError> {
        match result {
            Ok(result) => {
                record.mark_completed(result.exit_code, result.stats);
                write_process_record(&self.process_path(record.id), &record)?;
                self.write_process_log(&record, &result.output, result.value_display.as_deref())?;
                self.record_checkpoint(&record, &result)?;
                self.events.publish(
                    RuntimeEventKind::ProcessCompleted,
                    Some(record.id),
                    Some(record.session_id),
                );
                Ok(result)
            }
            Err(error) => {
                let rendered = error.render();
                record.mark_failed(error.exit_code, rendered.clone());
                write_process_record(&self.process_path(record.id), &record)?;
                self.write_process_log(&record, "", Some(rendered.trim_end()))?;
                self.write_last_error(&rendered)?;
                self.events.publish(
                    RuntimeEventKind::ProcessFailed,
                    Some(record.id),
                    Some(record.session_id),
                );
                Err(error)
            }
        }
    }

    fn record_checkpoint(
        &mut self,
        record: &RuntimeProcessRecord,
        result: &RunResult,
    ) -> Result<(), RuntimeError> {
        let checkpoint = RuntimeCheckpoint {
            id: self.next_checkpoint_id()?,
            process_id: record.id,
            session_id: record.session_id,
            label: "latest-completed-run".to_string(),
            timestamp_ms: timestamp_ms(),
            exit_code: result.exit_code,
            output_snapshot: process_output_snapshot(
                &result.output,
                result.value_display.as_deref(),
            ),
        };
        append_line(&self.checkpoint_path(), &format_checkpoint(&checkpoint))?;
        self.events.publish(
            RuntimeEventKind::CheckpointRecorded,
            Some(record.id),
            Some(record.session_id),
        );
        Ok(())
    }

    fn write_process_log(
        &self,
        record: &RuntimeProcessRecord,
        output: &str,
        value_display: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let snapshot = process_output_snapshot(output, value_display);
        fs::write(self.log_path(record.id), snapshot).map_err(runtime_io_error)
    }

    fn write_last_error(&self, error: &str) -> Result<(), RuntimeError> {
        let mut state = read_key_values(&self.state_path())?;
        state.insert("last_runtime_error".to_string(), error.to_string());
        write_key_values_from_map(&self.state_path(), &state)
    }

    fn allocate_process_id(&self) -> Result<RuntimeProcessId, RuntimeError> {
        let path = self.next_process_path();
        let next = fs::read_to_string(&path)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(1);
        fs::write(path, (next + 1).to_string()).map_err(runtime_io_error)?;
        Ok(RuntimeProcessId(next))
    }

    fn next_checkpoint_id(&self) -> Result<u64, RuntimeError> {
        Ok(self.checkpoints()?.len() as u64 + 1)
    }

    fn ensure_layout(&self) -> Result<(), RuntimeError> {
        fs::create_dir_all(self.process_dir()).map_err(runtime_io_error)?;
        fs::create_dir_all(self.log_dir()).map_err(runtime_io_error)?;
        Ok(())
    }

    fn state_path(&self) -> PathBuf {
        self.home.join(DAEMON_STATE_FILE)
    }

    fn next_process_path(&self) -> PathBuf {
        self.home.join(NEXT_PROCESS_FILE)
    }

    fn process_dir(&self) -> PathBuf {
        self.home.join(PROCESS_DIR)
    }

    fn log_dir(&self) -> PathBuf {
        self.home.join(LOG_DIR)
    }

    fn process_path(&self, process_id: RuntimeProcessId) -> PathBuf {
        self.process_dir().join(format!("{}.state", process_id.0))
    }

    fn log_path(&self, process_id: RuntimeProcessId) -> PathBuf {
        self.log_dir().join(format!("{}.log", process_id.0))
    }

    fn checkpoint_path(&self) -> PathBuf {
        self.home.join(CHECKPOINT_FILE)
    }
}

impl Default for RuntimeDaemon {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the default daemon home directory.
pub fn default_runtime_home() -> PathBuf {
    env::var_os(RUNTIME_HOME_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("ferrix-runtime"))
}

fn write_process_record(path: &Path, record: &RuntimeProcessRecord) -> Result<(), RuntimeError> {
    write_key_values(
        path,
        &[
            ("id", record.id.0.to_string()),
            (
                "parent_id",
                record
                    .parent_id
                    .map_or_else(String::new, |parent| parent.0.to_string()),
            ),
            ("session_id", record.session_id.0.to_string()),
            ("status", record.status.as_str().to_string()),
            ("kind", record.kind.as_str().to_string()),
            ("profile", record.profile.as_str().to_string()),
            ("path", record.path.display().to_string()),
            ("args", record.args.join(",")),
            ("started_at_ms", record.started_at_ms.to_string()),
            (
                "ended_at_ms",
                record
                    .ended_at_ms
                    .map_or_else(String::new, |value| value.to_string()),
            ),
            (
                "exit_code",
                record
                    .exit_code
                    .map_or_else(String::new, |value| value.to_string()),
            ),
            (
                "executed_instructions",
                record.stats.executed_instructions.to_string(),
            ),
            ("call_depth", record.stats.call_depth.to_string()),
            ("heap_objects", record.stats.heap_objects.to_string()),
            ("gc_collections", record.stats.gc_collections.to_string()),
            (
                "incremental_gc_steps",
                record.stats.incremental_gc_steps.to_string(),
            ),
            ("last_error", record.last_error.clone().unwrap_or_default()),
        ],
    )
}

fn read_process_record(path: &Path) -> Result<Option<RuntimeProcessRecord>, RuntimeError> {
    let values = read_key_values(path)?;
    if values.is_empty() {
        return Ok(None);
    }
    let Some(id) = values.get("id").and_then(|value| value.parse::<u64>().ok()) else {
        return Ok(None);
    };
    let session_id = values
        .get("session_id")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(id);
    let status = values
        .get("status")
        .and_then(|value| RuntimeProcessStatus::parse(value))
        .unwrap_or(RuntimeProcessStatus::Failed);
    let kind = match values.get("kind").map(String::as_str) {
        Some("bytecode") => RuntimeProcessKind::Bytecode,
        _ => RuntimeProcessKind::Source,
    };
    let profile = values
        .get("profile")
        .and_then(|value| value.parse::<RuntimeProfile>().ok())
        .unwrap_or(RuntimeProfile::Cli);
    let stats = RuntimeStats {
        executed_instructions: parse_value(&values, "executed_instructions"),
        call_depth: parse_value(&values, "call_depth"),
        heap_objects: parse_value(&values, "heap_objects"),
        gc_collections: parse_value(&values, "gc_collections"),
        incremental_gc_steps: parse_value(&values, "incremental_gc_steps"),
    };

    Ok(Some(RuntimeProcessRecord {
        id: RuntimeProcessId(id),
        parent_id: values
            .get("parent_id")
            .and_then(|value| value.parse::<u64>().ok())
            .map(RuntimeProcessId),
        session_id: RuntimeSessionId(session_id),
        status,
        kind,
        profile,
        path: values
            .get("path")
            .map(PathBuf::from)
            .unwrap_or_else(PathBuf::new),
        args: values
            .get("args")
            .filter(|value| !value.is_empty())
            .map(|value| value.split(',').map(str::to_string).collect())
            .unwrap_or_default(),
        started_at_ms: parse_value(&values, "started_at_ms"),
        ended_at_ms: values
            .get("ended_at_ms")
            .and_then(|value| value.parse::<u128>().ok()),
        exit_code: values
            .get("exit_code")
            .and_then(|value| value.parse::<i32>().ok()),
        stats,
        last_error: values
            .get("last_error")
            .filter(|value| !value.is_empty())
            .cloned(),
    }))
}

fn parse_value<T>(values: &BTreeMap<String, String>, key: &str) -> T
where
    T: Default + std::str::FromStr,
{
    values
        .get(key)
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or_default()
}

fn process_output_snapshot(output: &str, value_display: Option<&str>) -> String {
    let mut snapshot = String::from(output);
    if let Some(value) = value_display {
        snapshot.push_str(value);
        snapshot.push('\n');
    }
    snapshot
}

fn format_checkpoint(checkpoint: &RuntimeCheckpoint) -> String {
    [
        checkpoint.id.to_string(),
        checkpoint.process_id.0.to_string(),
        checkpoint.session_id.0.to_string(),
        checkpoint.timestamp_ms.to_string(),
        checkpoint.exit_code.to_string(),
        escape(&checkpoint.label),
        escape(&checkpoint.output_snapshot),
    ]
    .join("\t")
}

fn parse_checkpoint(line: &str) -> Option<RuntimeCheckpoint> {
    let parts = line.split('\t').collect::<Vec<_>>();
    if parts.len() != 7 {
        return None;
    }
    Some(RuntimeCheckpoint {
        id: parts[0].parse().ok()?,
        process_id: RuntimeProcessId(parts[1].parse().ok()?),
        session_id: RuntimeSessionId(parts[2].parse().ok()?),
        timestamp_ms: parts[3].parse().ok()?,
        exit_code: parts[4].parse().ok()?,
        label: unescape(parts[5]),
        output_snapshot: unescape(parts[6]),
    })
}

fn write_key_values(path: &Path, values: &[(&str, String)]) -> Result<(), RuntimeError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(runtime_io_error)?;
    }
    let source = values
        .iter()
        .map(|(key, value)| format!("{key}={}", escape(value)))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, format!("{source}\n")).map_err(runtime_io_error)
}

fn write_key_values_from_map(
    path: &Path,
    values: &BTreeMap<String, String>,
) -> Result<(), RuntimeError> {
    let pairs = values
        .iter()
        .map(|(key, value)| (key.as_str(), value.clone()))
        .collect::<Vec<_>>();
    write_key_values(path, &pairs)
}

fn read_key_values(path: &Path) -> Result<BTreeMap<String, String>, RuntimeError> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(runtime_io_error(error)),
    };
    Ok(source
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.to_string(), unescape(value)))
        })
        .collect())
}

fn append_line(path: &Path, line: &str) -> Result<(), RuntimeError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(runtime_io_error)?;
    }
    let mut source = fs::read_to_string(path).unwrap_or_default();
    source.push_str(line);
    source.push('\n');
    fs::write(path, source).map_err(runtime_io_error)
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => output.push('\n'),
                Some('\\') => output.push('\\'),
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => output.push('\\'),
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn runtime_io_error(error: io::Error) -> RuntimeError {
    RuntimeError::new(
        66,
        RuntimeErrorKind::DaemonState {
            message: error.to_string(),
        },
    )
}
