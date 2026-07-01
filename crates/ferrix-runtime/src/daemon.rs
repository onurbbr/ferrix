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
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
};

use crate::{
    DebugRequest, RunBytecodeRequest, RunResult, RunSourceRequest, RuntimeError, RuntimeErrorKind,
    RuntimeEventBus, RuntimeEventKind, RuntimeMode, RuntimeProcessId, RuntimeProcessKind,
    RuntimeProcessRecord, RuntimeProcessStatus, RuntimeProcessTable, RuntimeProfile,
    RuntimeSessionId, RuntimeStats,
    event::{RuntimeEventMetadata, RuntimeEventSeverity, timestamp_ms},
    request::RecordProcessRequest,
    service::CompiledProgram,
};

const DAEMON_STATE_FILE: &str = "daemon.state";
const NEXT_PROCESS_FILE: &str = "next-process-id";
const PID_FILE: &str = "daemon.pid";
const SOCKET_FILE: &str = "runtime.sock";
const PROCESS_DIR: &str = "processes";
const LOG_DIR: &str = "logs";
const CHECKPOINT_FILE: &str = "checkpoints.log";
const LOCAL_HOME_DIR: &str = "ferrix";
const CONFIG_DIR: &str = "configs";
const SERVICE_DIR: &str = "services";
const RUNTIME_SERVICE_DIR: &str = "runtime";
const CONFIG_FILE: &str = "config.toml";

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
    /// Retained runtime event count.
    pub event_queue_len: usize,
    /// Runtime events dropped because the queue reached capacity.
    pub dropped_event_count: u64,
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

    /// Returns the Unix socket path used by the daemon process.
    pub fn socket_path(&self) -> PathBuf {
        self.home.join(SOCKET_FILE)
    }

    /// Returns the pid file path used by the daemon process.
    pub fn pid_path(&self) -> PathBuf {
        self.home.join(PID_FILE)
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
                ("pid", std::process::id().to_string()),
                ("socket", self.socket_path().display().to_string()),
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
                ("pid", String::new()),
                ("socket", self.socket_path().display().to_string()),
                ("last_runtime_error", String::new()),
            ],
        )?;
        let _ = fs::remove_file(self.pid_path());
        let _ = fs::remove_file(self.socket_path());
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

    /// Returns status after checking whether a serving daemon is actually alive.
    pub fn checked_status(&mut self) -> Result<RuntimeStatusReport, RuntimeError> {
        let status = self.status()?;
        if !status.is_serving() || self.ping()? {
            return Ok(status);
        }
        self.stop()
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
        let history = self.list_history()?;
        let active_process_count = history
            .iter()
            .filter(|process| process.status.is_active())
            .count();
        let completed_process_count = history
            .iter()
            .filter(|process| process.status == RuntimeProcessStatus::Completed)
            .count();
        let failed_process_count = history
            .iter()
            .filter(|process| process.status == RuntimeProcessStatus::Failed)
            .count();
        let uptime_ms = if health == RuntimeHealth::Serving {
            started_at_ms.map(|started| timestamp_ms().saturating_sub(started))
        } else {
            None
        };

        let event_stats = self.events.stats();

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
            process_count: history.len(),
            default_mode: RuntimeMode::Required,
            bytecode_cache_size: 0,
            module_cache_size: 0,
            event_queue_len: event_stats.len,
            dropped_event_count: event_stats.dropped_events,
            last_runtime_error: state
                .get("last_runtime_error")
                .filter(|value| !value.is_empty())
                .cloned(),
        })
    }

    /// Serves local Unix socket requests until a stop request is received.
    pub fn serve_forever(&mut self) -> Result<(), RuntimeError> {
        self.ensure_layout()?;
        let _ = fs::remove_file(self.socket_path());
        let listener = UnixListener::bind(self.socket_path()).map_err(runtime_io_error)?;
        fs::write(self.pid_path(), std::process::id().to_string()).map_err(runtime_io_error)?;
        self.start()?;

        for stream in listener.incoming() {
            let stream = stream.map_err(runtime_io_error)?;
            if self.handle_stream(stream)? {
                break;
            }
        }

        self.stop()?;
        Ok(())
    }

    /// Returns true when a daemon process is answering socket requests.
    pub fn ping(&self) -> Result<bool, RuntimeError> {
        match self.request("PING") {
            Ok(response) => Ok(response == "OK\tPONG"),
            Err(error) if matches!(error.kind, RuntimeErrorKind::RuntimeUnavailable { .. }) => {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Sends a stop request to the running daemon process.
    pub fn stop_process(&self) -> Result<(), RuntimeError> {
        let response = self.request("STOP")?;
        if response == "OK\tSTOPPED" {
            Ok(())
        } else {
            Err(RuntimeError::new(
                70,
                RuntimeErrorKind::DaemonState { message: response },
            ))
        }
    }

    /// Runs source through the daemon socket.
    pub fn request_run_source(&self, request: RunSourceRequest) -> Result<RunResult, RuntimeError> {
        self.decode_run_response(&self.request(&format!(
            "RUN_SOURCE\t{}\t{}\t{}\t{}",
            escape(&request.path.display().to_string()),
            escape(request.profile.as_str()),
            request.collect_stats,
            request.collect_audit
        ))?)
    }

    /// Runs bytecode through the daemon socket.
    pub fn request_run_bytecode(
        &self,
        request: RunBytecodeRequest,
    ) -> Result<RunResult, RuntimeError> {
        self.decode_run_response(&self.request(&format!(
            "RUN_BYTECODE\t{}\t{}\t{}\t{}",
            escape(&request.path.display().to_string()),
            escape(request.profile.as_str()),
            request.collect_stats,
            request.collect_audit
        ))?)
    }

    /// Records a CLI command through the daemon socket.
    pub fn request_record_process(
        &self,
        request: RecordProcessRequest,
    ) -> Result<RuntimeProcessRecord, RuntimeError> {
        decode_record_response(&self.request(&format!(
            "RECORD_PROCESS\t{}\t{}\t{}\t{}\t{}",
            escape(request.kind.as_str()),
            escape(&request.path.display().to_string()),
            request.exit_code,
            escape(&request.output),
            request.last_error.as_deref().map_or(String::new(), escape)
        ))?)
    }

    /// Lists active process records through the daemon socket.
    pub fn request_list_processes(&self) -> Result<Vec<RuntimeProcessRecord>, RuntimeError> {
        decode_records_response(&self.request("LIST_PROCESSES")?)
    }

    /// Lists process history records through the daemon socket.
    pub fn request_list_history(&self) -> Result<Vec<RuntimeProcessRecord>, RuntimeError> {
        decode_records_response(&self.request("LIST_HISTORY")?)
    }

    /// Returns one process record through the daemon socket.
    pub fn request_process_info(
        &self,
        process_id: RuntimeProcessId,
    ) -> Result<Option<RuntimeProcessRecord>, RuntimeError> {
        decode_optional_record_response(&self.request(&format!("PROCESS_INFO\t{process_id}"))?)
    }

    /// Returns a process output snapshot through the daemon socket.
    pub fn request_logs(&self, process_id: RuntimeProcessId) -> Result<String, RuntimeError> {
        decode_text_response(&self.request(&format!("PROCESS_LOG\t{process_id}"))?)
    }

    /// Kills an active process through the daemon socket.
    pub fn request_kill_process(
        &self,
        process_id: RuntimeProcessId,
    ) -> Result<RuntimeProcessRecord, RuntimeError> {
        decode_record_response(&self.request(&format!("KILL_PROCESS\t{process_id}"))?)
    }

    /// Runs source through a serving daemon and records process metadata.
    pub fn run_source(&mut self, mut request: RunSourceRequest) -> Result<RunResult, RuntimeError> {
        self.require_serving()?;
        request.collect_stats = true;
        let record = self.start_process(
            RuntimeProcessKind::Run,
            request.profile,
            request.path.clone(),
            request.args.clone(),
        )?;
        self.events.publish_event(
            RuntimeEventKind::ProgramStarted,
            Some(record.id),
            Some(record.session_id),
            RuntimeEventMetadata::new(RuntimeEventSeverity::Info)
                .with_message(format!("run {}", record.path.display())),
        );
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
            RuntimeProcessKind::RunBytecode,
            request.profile,
            request.path.clone(),
            Vec::new(),
        )?;
        self.events.publish_event(
            RuntimeEventKind::ProgramStarted,
            Some(record.id),
            Some(record.session_id),
            RuntimeEventMetadata::new(RuntimeEventSeverity::Info)
                .with_message(format!("run-bytecode {}", record.path.display())),
        );
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

    /// Records a CLI-level command in the runtime process history.
    pub fn record_cli_process(
        &mut self,
        kind: RuntimeProcessKind,
        path: impl Into<PathBuf>,
        exit_code: i32,
        output: &str,
        last_error: Option<&str>,
    ) -> Result<RuntimeProcessRecord, RuntimeError> {
        let mut record = self.start_process(kind, RuntimeProfile::Cli, path.into(), Vec::new())?;
        if exit_code == 0 {
            record.mark_completed(exit_code, RuntimeStats::default());
        } else {
            record.mark_failed(
                exit_code,
                last_error
                    .unwrap_or("command failed")
                    .trim_end()
                    .to_string(),
            );
        }
        write_process_record(&self.process_path(record.id), &record)?;
        self.write_process_log(&record, output, None)?;
        self.memory_table.update(record.clone());
        Ok(record)
    }

    /// Lists active process records.
    pub fn list_processes(&self) -> Result<Vec<RuntimeProcessRecord>, RuntimeError> {
        Ok(self
            .list_history()?
            .into_iter()
            .filter(|record| record.status.is_active())
            .collect())
    }

    /// Lists persisted process history records.
    pub fn list_history(&self) -> Result<Vec<RuntimeProcessRecord>, RuntimeError> {
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

    /// Returns one persisted process history record.
    pub fn process_info(
        &self,
        process_id: RuntimeProcessId,
    ) -> Result<Option<RuntimeProcessRecord>, RuntimeError> {
        read_process_record(&self.process_path(process_id))
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
        if !record.status.is_active() {
            return Err(RuntimeError::new(
                66,
                RuntimeErrorKind::DaemonState {
                    message: format!("runtime process {process_id} is not active"),
                },
            ));
        }
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

    fn handle_stream(&mut self, mut stream: UnixStream) -> Result<bool, RuntimeError> {
        let mut line = String::new();
        {
            let mut reader = BufReader::new(&mut stream);
            reader.read_line(&mut line).map_err(runtime_io_error)?;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        let mut parts = line.split('\t');
        let command = parts.next().unwrap_or_default();
        let mut should_stop = false;
        let response = match command {
            "PING" => "OK\tPONG".to_string(),
            "STOP" => {
                should_stop = true;
                "OK\tSTOPPED".to_string()
            }
            "RUN_SOURCE" => {
                let path = parts.next().map(unescape).unwrap_or_default();
                let profile = parts
                    .next()
                    .map(unescape)
                    .and_then(|value| value.parse::<RuntimeProfile>().ok())
                    .unwrap_or(RuntimeProfile::Cli);
                let mut request = RunSourceRequest::new(path);
                request.profile = profile;
                request.collect_stats = parts.next().and_then(parse_bool).unwrap_or(false);
                request.collect_audit = parts.next().and_then(parse_bool).unwrap_or(false);
                encode_run_response(self.run_source(request))
            }
            "RUN_BYTECODE" => {
                let path = parts.next().map(unescape).unwrap_or_default();
                let profile = parts
                    .next()
                    .map(unescape)
                    .and_then(|value| value.parse::<RuntimeProfile>().ok())
                    .unwrap_or(RuntimeProfile::Cli);
                let mut request = RunBytecodeRequest::new(path);
                request.profile = profile;
                request.collect_stats = parts.next().and_then(parse_bool).unwrap_or(false);
                request.collect_audit = parts.next().and_then(parse_bool).unwrap_or(false);
                encode_run_response(self.run_bytecode(request))
            }
            "RECORD_PROCESS" => {
                let kind = parts
                    .next()
                    .map(unescape)
                    .and_then(|value| parse_process_kind(&value))
                    .unwrap_or(RuntimeProcessKind::Run);
                let path = parts.next().map(unescape).unwrap_or_default();
                let exit_code = parts
                    .next()
                    .and_then(|value| value.parse::<i32>().ok())
                    .unwrap_or(70);
                let output = parts.next().map(unescape).unwrap_or_default();
                let last_error = parts.next().map(unescape).filter(|value| !value.is_empty());
                encode_record_response(self.record_cli_process(
                    kind,
                    path,
                    exit_code,
                    &output,
                    last_error.as_deref(),
                ))
            }
            "LIST_PROCESSES" => encode_records_response(self.list_processes()),
            "LIST_HISTORY" => encode_records_response(self.list_history()),
            "PROCESS_INFO" => {
                let process_id = parts
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(RuntimeProcessId);
                match process_id {
                    Some(process_id) => {
                        encode_optional_record_response(self.process_info(process_id))
                    }
                    None => format!("ERR\t64\t{}", escape("invalid runtime process id")),
                }
            }
            "PROCESS_LOG" => {
                let process_id = parts
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(RuntimeProcessId);
                match process_id {
                    Some(process_id) => encode_text_response(self.logs(process_id)),
                    None => format!("ERR\t64\t{}", escape("invalid runtime process id")),
                }
            }
            "KILL_PROCESS" => {
                let process_id = parts
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(RuntimeProcessId);
                match process_id {
                    Some(process_id) => encode_record_response(self.kill_process(process_id)),
                    None => format!("ERR\t64\t{}", escape("invalid runtime process id")),
                }
            }
            _ => format!("ERR\t64\t{}", escape("unknown daemon command")),
        };
        writeln!(stream, "{response}").map_err(runtime_io_error)?;
        Ok(should_stop)
    }

    fn request(&self, request: &str) -> Result<String, RuntimeError> {
        let mut stream = UnixStream::connect(self.socket_path()).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound
                || error.kind() == io::ErrorKind::ConnectionRefused
            {
                RuntimeError::new(
                    69,
                    RuntimeErrorKind::RuntimeUnavailable {
                        mode: RuntimeMode::Required,
                    },
                )
            } else {
                runtime_io_error(error)
            }
        })?;
        writeln!(stream, "{request}").map_err(runtime_io_error)?;
        let mut response = String::new();
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut response).map_err(runtime_io_error)?;
        Ok(response.trim_end_matches(['\r', '\n']).to_string())
    }

    fn decode_run_response(&self, response: &str) -> Result<RunResult, RuntimeError> {
        let parts = response.split('\t').collect::<Vec<_>>();
        match parts.as_slice() {
            ["OK", exit_code, output, value_display] => Ok(RunResult {
                exit_code: exit_code.parse().unwrap_or(0),
                value: ferrix_core::Value::Nil,
                value_display: optional_unescape(value_display),
                output: unescape(output),
                stats: RuntimeStats::default(),
                audit_events: Vec::new(),
            }),
            ["OK", exit_code, output, value_display, stats, audit_events] => Ok(RunResult {
                exit_code: exit_code.parse().unwrap_or(0),
                value: ferrix_core::Value::Nil,
                value_display: optional_unescape(value_display),
                output: unescape(output),
                stats: parse_runtime_stats(&unescape(stats)),
                audit_events: parse_audit_events(&unescape(audit_events)),
            }),
            ["ERR", exit_code, message] => Err(RuntimeError::new(
                exit_code.parse().unwrap_or(70),
                RuntimeErrorKind::Execution(unescape(message)),
            )),
            _ => Err(RuntimeError::new(
                70,
                RuntimeErrorKind::DaemonState {
                    message: format!("invalid daemon response `{response}`"),
                },
            )),
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
                self.events.publish_event(
                    RuntimeEventKind::ProgramCompleted,
                    Some(record.id),
                    Some(record.session_id),
                    RuntimeEventMetadata::new(RuntimeEventSeverity::Info)
                        .with_message(format!("exit_code={}", result.exit_code)),
                );
                self.publish_audit_events(&record, &result.audit_events);
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
                self.events.publish_event(
                    RuntimeEventKind::ProgramFailed,
                    Some(record.id),
                    Some(record.session_id),
                    RuntimeEventMetadata::new(RuntimeEventSeverity::Error)
                        .with_message(format!("exit_code={}", error.exit_code)),
                );
                Err(error)
            }
        }
    }

    fn publish_audit_events(&mut self, record: &RuntimeProcessRecord, audit_events: &[String]) {
        for event in audit_events {
            let severity = if event.contains("denied") || event.contains("failed") {
                RuntimeEventSeverity::Error
            } else {
                RuntimeEventSeverity::Info
            };
            let kind = if event.contains("capability_denied") {
                RuntimeEventKind::CapabilityDenied(event.clone())
            } else if event.contains("program_completed") {
                RuntimeEventKind::ProgramCompleted
            } else if event.contains("program_failed") {
                RuntimeEventKind::ProgramFailed
            } else {
                RuntimeEventKind::AuditEvent(event.clone())
            };
            self.events.publish_event(
                kind,
                Some(record.id),
                Some(record.session_id),
                RuntimeEventMetadata::new(severity).with_message(event.clone()),
            );
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
    default_ferrix_home()
        .join(SERVICE_DIR)
        .join(RUNTIME_SERVICE_DIR)
}

/// Returns the default local Ferrix home next to the running binary.
pub fn default_ferrix_home() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(LOCAL_HOME_DIR)))
        .unwrap_or_else(|| PathBuf::from("/tmp").join(LOCAL_HOME_DIR))
}

/// Returns the default local Ferrix config file path.
pub fn default_config_path() -> PathBuf {
    default_ferrix_home().join(CONFIG_DIR).join(CONFIG_FILE)
}

/// Creates the default local Ferrix directory layout and config file.
pub fn ensure_default_layout() -> Result<(), RuntimeError> {
    let home = default_ferrix_home();
    let config_dir = home.join(CONFIG_DIR);
    let runtime_home = default_runtime_home();

    fs::create_dir_all(&config_dir).map_err(runtime_io_error)?;
    fs::create_dir_all(&runtime_home).map_err(runtime_io_error)?;

    let config_path = config_dir.join(CONFIG_FILE);
    if !config_path.exists() {
        fs::write(&config_path, default_config_source()).map_err(runtime_io_error)?;
    }

    Ok(())
}

fn default_config_source() -> String {
    [
        "# Ferrix local configuration.",
        "# This file is created next to the ferrix binary for local runtime services.",
        "",
        "[runtime]",
        "mode = \"embedded\"",
        "home = \"services/runtime\"",
        "",
        "[services]",
        "runtime = \"services/runtime\"",
        "",
    ]
    .join("\n")
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
            ("max_call_depth", record.stats.max_call_depth.to_string()),
            (
                "max_register_count",
                record.stats.max_register_count.to_string(),
            ),
            ("heap_objects", record.stats.heap_objects.to_string()),
            ("allocations", record.stats.allocations.to_string()),
            (
                "allocation_pressure",
                record.stats.allocation_pressure.to_string(),
            ),
            ("gc_collections", record.stats.gc_collections.to_string()),
            (
                "incremental_gc_steps",
                record.stats.incremental_gc_steps.to_string(),
            ),
            ("native_calls", record.stats.native_calls.to_string()),
            ("thrown_errors", record.stats.thrown_errors.to_string()),
            (
                "handled_exceptions",
                record.stats.handled_exceptions.to_string(),
            ),
            (
                "execution_time_ms",
                record.stats.execution_time_ms.to_string(),
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
        Some("check") => RuntimeProcessKind::Check,
        Some("compile") => RuntimeProcessKind::Compile,
        Some("debug") => RuntimeProcessKind::Debug,
        Some("run-bytecode" | "bytecode") => RuntimeProcessKind::RunBytecode,
        Some("run" | "source") => RuntimeProcessKind::Run,
        _ => RuntimeProcessKind::Run,
    };
    let profile = values
        .get("profile")
        .and_then(|value| value.parse::<RuntimeProfile>().ok())
        .unwrap_or(RuntimeProfile::Cli);
    let stats = RuntimeStats {
        executed_instructions: parse_value(&values, "executed_instructions"),
        call_depth: parse_value(&values, "call_depth"),
        max_call_depth: parse_value(&values, "max_call_depth"),
        max_register_count: parse_value(&values, "max_register_count"),
        heap_objects: parse_value(&values, "heap_objects"),
        allocations: parse_value(&values, "allocations"),
        allocation_pressure: parse_value(&values, "allocation_pressure"),
        gc_collections: parse_value(&values, "gc_collections"),
        incremental_gc_steps: parse_value(&values, "incremental_gc_steps"),
        native_calls: parse_value(&values, "native_calls"),
        thrown_errors: parse_value(&values, "thrown_errors"),
        handled_exceptions: parse_value(&values, "handled_exceptions"),
        execution_time_ms: parse_value(&values, "execution_time_ms"),
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

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn process_output_snapshot(output: &str, value_display: Option<&str>) -> String {
    let mut snapshot = String::from(output);
    if let Some(value) = value_display {
        snapshot.push_str(value);
        snapshot.push('\n');
    }
    snapshot
}

fn parse_process_kind(value: &str) -> Option<RuntimeProcessKind> {
    match value {
        "run" | "source" => Some(RuntimeProcessKind::Run),
        "check" => Some(RuntimeProcessKind::Check),
        "compile" => Some(RuntimeProcessKind::Compile),
        "run-bytecode" | "bytecode" => Some(RuntimeProcessKind::RunBytecode),
        "debug" => Some(RuntimeProcessKind::Debug),
        _ => None,
    }
}

fn encode_run_response(result: Result<RunResult, RuntimeError>) -> String {
    match result {
        Ok(result) => format!(
            "OK\t{}\t{}\t{}\t{}\t{}",
            result.exit_code,
            escape(&result.output),
            result
                .value_display
                .as_deref()
                .map_or(String::new(), escape),
            escape(&format_runtime_stats(&result.stats)),
            escape(&result.audit_events.join("\n"))
        ),
        Err(error) => format!("ERR\t{}\t{}", error.exit_code, escape(&error.render())),
    }
}

fn format_runtime_stats(stats: &RuntimeStats) -> String {
    [
        stats.executed_instructions.to_string(),
        stats.call_depth.to_string(),
        stats.max_call_depth.to_string(),
        stats.max_register_count.to_string(),
        stats.heap_objects.to_string(),
        stats.allocations.to_string(),
        stats.allocation_pressure.to_string(),
        stats.gc_collections.to_string(),
        stats.incremental_gc_steps.to_string(),
        stats.native_calls.to_string(),
        stats.thrown_errors.to_string(),
        stats.handled_exceptions.to_string(),
        stats.execution_time_ms.to_string(),
    ]
    .join(",")
}

fn parse_runtime_stats(source: &str) -> RuntimeStats {
    let fields = source.split(',').collect::<Vec<_>>();
    RuntimeStats {
        executed_instructions: parse_stat_field(&fields, 0),
        call_depth: parse_stat_field(&fields, 1),
        max_call_depth: parse_stat_field(&fields, 2),
        max_register_count: parse_stat_field(&fields, 3),
        heap_objects: parse_stat_field(&fields, 4),
        allocations: parse_stat_field(&fields, 5),
        allocation_pressure: parse_stat_field(&fields, 6),
        gc_collections: parse_stat_field(&fields, 7),
        incremental_gc_steps: parse_stat_field(&fields, 8),
        native_calls: parse_stat_field(&fields, 9),
        thrown_errors: parse_stat_field(&fields, 10),
        handled_exceptions: parse_stat_field(&fields, 11),
        execution_time_ms: parse_stat_field(&fields, 12),
    }
}

fn parse_stat_field<T>(fields: &[&str], index: usize) -> T
where
    T: Default + std::str::FromStr,
{
    fields
        .get(index)
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or_default()
}

fn parse_audit_events(source: &str) -> Vec<String> {
    source
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn encode_record_response(result: Result<RuntimeProcessRecord, RuntimeError>) -> String {
    match result {
        Ok(record) => format!("OK\t{}", escape(&format_process_record(&record))),
        Err(error) => format!("ERR\t{}\t{}", error.exit_code, escape(&error.render())),
    }
}

fn encode_optional_record_response(
    result: Result<Option<RuntimeProcessRecord>, RuntimeError>,
) -> String {
    match result {
        Ok(Some(record)) => format!("OK\t{}", escape(&format_process_record(&record))),
        Ok(None) => "OK\t".to_string(),
        Err(error) => format!("ERR\t{}\t{}", error.exit_code, escape(&error.render())),
    }
}

fn encode_records_response(result: Result<Vec<RuntimeProcessRecord>, RuntimeError>) -> String {
    match result {
        Ok(records) => {
            let payload = records
                .iter()
                .map(format_process_record)
                .collect::<Vec<_>>()
                .join("\n");
            format!("OK\t{}", escape(&payload))
        }
        Err(error) => format!("ERR\t{}\t{}", error.exit_code, escape(&error.render())),
    }
}

fn encode_text_response(result: Result<String, RuntimeError>) -> String {
    match result {
        Ok(output) => format!("OK\t{}", escape(&output)),
        Err(error) => format!("ERR\t{}\t{}", error.exit_code, escape(&error.render())),
    }
}

fn decode_record_response(response: &str) -> Result<RuntimeProcessRecord, RuntimeError> {
    let parts = response.split('\t').collect::<Vec<_>>();
    match parts.as_slice() {
        ["OK", payload] => parse_process_record_line(&unescape(payload)).ok_or_else(|| {
            RuntimeError::new(
                70,
                RuntimeErrorKind::DaemonState {
                    message: "invalid process record response".to_string(),
                },
            )
        }),
        ["ERR", exit_code, message] => Err(RuntimeError::new(
            exit_code.parse().unwrap_or(70),
            RuntimeErrorKind::DaemonState {
                message: unescape(message).trim_end().to_string(),
            },
        )),
        _ => invalid_daemon_response(response),
    }
}

fn decode_optional_record_response(
    response: &str,
) -> Result<Option<RuntimeProcessRecord>, RuntimeError> {
    let parts = response.split('\t').collect::<Vec<_>>();
    match parts.as_slice() {
        ["OK", ""] => Ok(None),
        ["OK", payload] => parse_process_record_line(&unescape(payload))
            .map(Some)
            .ok_or_else(|| {
                RuntimeError::new(
                    70,
                    RuntimeErrorKind::DaemonState {
                        message: "invalid process record response".to_string(),
                    },
                )
            }),
        ["ERR", exit_code, message] => Err(RuntimeError::new(
            exit_code.parse().unwrap_or(70),
            RuntimeErrorKind::DaemonState {
                message: unescape(message).trim_end().to_string(),
            },
        )),
        _ => invalid_daemon_response(response),
    }
}

fn decode_records_response(response: &str) -> Result<Vec<RuntimeProcessRecord>, RuntimeError> {
    let parts = response.split('\t').collect::<Vec<_>>();
    match parts.as_slice() {
        ["OK", ""] => Ok(Vec::new()),
        ["OK", payload] => Ok(unescape(payload)
            .lines()
            .filter_map(parse_process_record_line)
            .collect()),
        ["ERR", exit_code, message] => Err(RuntimeError::new(
            exit_code.parse().unwrap_or(70),
            RuntimeErrorKind::DaemonState {
                message: unescape(message).trim_end().to_string(),
            },
        )),
        _ => invalid_daemon_response(response),
    }
}

fn decode_text_response(response: &str) -> Result<String, RuntimeError> {
    let parts = response.split('\t').collect::<Vec<_>>();
    match parts.as_slice() {
        ["OK", payload] => Ok(unescape(payload)),
        ["ERR", exit_code, message] => Err(RuntimeError::new(
            exit_code.parse().unwrap_or(70),
            RuntimeErrorKind::DaemonState {
                message: unescape(message).trim_end().to_string(),
            },
        )),
        _ => invalid_daemon_response(response),
    }
}

fn invalid_daemon_response<T>(response: &str) -> Result<T, RuntimeError> {
    Err(RuntimeError::new(
        70,
        RuntimeErrorKind::DaemonState {
            message: format!("invalid daemon response `{response}`"),
        },
    ))
}

fn format_process_record(record: &RuntimeProcessRecord) -> String {
    [
        record.id.0.to_string(),
        record
            .parent_id
            .map_or_else(String::new, |parent| parent.0.to_string()),
        record.session_id.0.to_string(),
        record.status.as_str().to_string(),
        record.kind.as_str().to_string(),
        record.profile.as_str().to_string(),
        escape(&record.path.display().to_string()),
        escape(&record.args.join(",")),
        record.started_at_ms.to_string(),
        record
            .ended_at_ms
            .map_or_else(String::new, |value| value.to_string()),
        record
            .exit_code
            .map_or_else(String::new, |value| value.to_string()),
        record.stats.executed_instructions.to_string(),
        record.stats.call_depth.to_string(),
        record.stats.max_call_depth.to_string(),
        record.stats.max_register_count.to_string(),
        record.stats.heap_objects.to_string(),
        record.stats.allocations.to_string(),
        record.stats.allocation_pressure.to_string(),
        record.stats.gc_collections.to_string(),
        record.stats.incremental_gc_steps.to_string(),
        record.stats.native_calls.to_string(),
        record.stats.thrown_errors.to_string(),
        record.stats.handled_exceptions.to_string(),
        record.stats.execution_time_ms.to_string(),
        record.last_error.as_deref().map_or(String::new(), escape),
    ]
    .join("\t")
}

fn parse_process_record_line(line: &str) -> Option<RuntimeProcessRecord> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 17 && fields.len() != 25 {
        return None;
    }
    let id = RuntimeProcessId(fields[0].parse().ok()?);
    let parent_id = if fields[1].is_empty() {
        None
    } else {
        Some(RuntimeProcessId(fields[1].parse().ok()?))
    };
    let status = RuntimeProcessStatus::parse(fields[3])?;
    let kind = parse_process_kind(fields[4])?;
    let profile = fields[5].parse::<RuntimeProfile>().ok()?;
    let ended_at_ms = if fields[9].is_empty() {
        None
    } else {
        Some(fields[9].parse().ok()?)
    };
    let exit_code = if fields[10].is_empty() {
        None
    } else {
        Some(fields[10].parse().ok()?)
    };
    let last_error_index = fields.len() - 1;
    let last_error = if fields[last_error_index].is_empty() {
        None
    } else {
        Some(unescape(fields[last_error_index]))
    };
    let new_stats = fields.len() == 25;

    Some(RuntimeProcessRecord {
        id,
        parent_id,
        session_id: RuntimeSessionId(fields[2].parse().ok()?),
        status,
        kind,
        profile,
        path: PathBuf::from(unescape(fields[6])),
        args: unescape(fields[7])
            .split(',')
            .filter(|arg| !arg.is_empty())
            .map(str::to_string)
            .collect(),
        started_at_ms: fields[8].parse().ok()?,
        ended_at_ms,
        exit_code,
        stats: RuntimeStats {
            executed_instructions: fields[11].parse().ok()?,
            call_depth: fields[12].parse().ok()?,
            max_call_depth: if new_stats {
                fields[13].parse().ok()?
            } else {
                0
            },
            max_register_count: if new_stats {
                fields[14].parse().ok()?
            } else {
                0
            },
            heap_objects: fields[if new_stats { 15 } else { 13 }].parse().ok()?,
            allocations: if new_stats {
                fields[16].parse().ok()?
            } else {
                0
            },
            allocation_pressure: if new_stats {
                fields[17].parse().ok()?
            } else {
                0
            },
            gc_collections: fields[if new_stats { 18 } else { 14 }].parse().ok()?,
            incremental_gc_steps: fields[if new_stats { 19 } else { 15 }].parse().ok()?,
            native_calls: if new_stats {
                fields[20].parse().ok()?
            } else {
                0
            },
            thrown_errors: if new_stats {
                fields[21].parse().ok()?
            } else {
                0
            },
            handled_exceptions: if new_stats {
                fields[22].parse().ok()?
            } else {
                0
            },
            execution_time_ms: if new_stats {
                fields[23].parse().ok()?
            } else {
                0
            },
        },
        last_error,
    })
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
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn unescape(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => output.push('\n'),
                Some('t') => output.push('\t'),
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

fn optional_unescape(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(unescape(value))
    }
}

fn runtime_io_error(error: io::Error) -> RuntimeError {
    RuntimeError::new(
        66,
        RuntimeErrorKind::DaemonState {
            message: error.to_string(),
        },
    )
}
