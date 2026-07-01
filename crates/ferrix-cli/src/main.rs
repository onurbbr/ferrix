//! Command-line entry point for Ferrix.
//!
//! Provides source execution, bytecode compile/run commands, static import
//! loading, and an instruction-level debugger.

use std::{
    collections::HashSet,
    env, fs, io,
    io::Write as _,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime},
};

use ferrix_compiler::{
    CompileError, CompileOutput, ImportedModuleAst,
    ast::{ProgramAst, Stmt},
    compile_program_ast_with_named_modules_report, parse_source_with_file_id,
};
use ferrix_core::{
    Obj, Value,
    bytecode::{
        BytecodeContainerMetadata, FunctionId, encode_container, format_instruction,
        inspect_container,
    },
    diagnostics::{SourceLocation, SourceManager},
};
use ferrix_runtime::{
    DebugRequest, RecordProcessRequest, RunBytecodeRequest, RunSourceRequest, RuntimeDaemon,
    RuntimeEvent, RuntimeGateway, RuntimeMetricsReport, RuntimeMode, RuntimeProcessId,
    RuntimeProcessKind, RuntimeProcessRecord, RuntimeProfile,
};
use ferrix_vm::{CallFrame, DebugAction, DebugEvent, DebugOutcome, Debugger, Heap, Vm};

const USAGE: &str = "\
Ferrix

Usage:
  ferrix [--runtime-mode <mode>] [--runtime-home <dir>] [--format human|json] <command>
  ferrix run <file|package> [--stats] [--audit] [--watch]
  ferrix check <file|package> [--watch]
  ferrix compile <file|package> <output> [--explain-optimizations]
  ferrix run-bytecode <file> [--stats] [--audit]
  ferrix debug <file|package>
  ferrix runtime start|stop|status|restart|metrics|events|config
  ferrix ps
  ferrix info <pid>
  ferrix logs
  ferrix inspect <bytecode>
  ferrix explain <source|package>
  ferrix kill <pid>
  ferrix --help
  ferrix --version
";

const MANIFEST_FILES: &[&str] = &["Ferrix.toml", "ferrix.toml"];
const RUNTIME_LAUNCH_DIR: &str = "/tmp/ferrix-runtime-launch";

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let code = run_cli(
        &args,
        |path| fs::read_to_string(path),
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    std::process::exit(code);
}

fn run_cli(
    args: &[String],
    mut read_file: impl FnMut(&str) -> io::Result<String>,
    stdin: &mut impl io::BufRead,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    // Keep command dispatch small and testable by injecting I/O handles.
    let (config, args) = match parse_runtime_options(args, stderr) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    match args.as_slice() {
        [] => {
            write!(stdout, "{USAGE}").expect("stdout write failed");
            0
        }
        [flag] if flag == "--help" || flag == "-h" => {
            write!(stdout, "{USAGE}").expect("stdout write failed");
            0
        }
        [flag] if flag == "--version" || flag == "-V" => {
            writeln!(stdout, "ferrix {}", env!("CARGO_PKG_VERSION")).expect("stdout write failed");
            0
        }
        [command, path, options @ ..] if command == "run" => {
            let Some(options) = parse_run_display_options(options, stderr) else {
                return 64;
            };
            run_file(path, &config, options, stdout, stderr)
        }
        [command, path, options @ ..] if command == "check" => {
            let Some(options) = parse_check_display_options(options, stderr) else {
                return 64;
            };
            check_file(path, &config, options, &mut read_file, stdout, stderr)
        }
        [command, path, output, options @ ..] if command == "compile" => {
            let Some(options) = parse_compile_display_options(options, stderr) else {
                return 64;
            };
            compile_bytecode(
                path,
                output,
                &config,
                options,
                &mut read_file,
                stdout,
                stderr,
            )
        }
        [command, path, options @ ..] if command == "run-bytecode" => {
            let Some(options) = parse_run_display_options(options, stderr) else {
                return 64;
            };
            run_bytecode(path, &config, options, stdout, stderr)
        }
        [command, path] if command == "debug" => debug_file(path, &config, stdin, stdout, stderr),
        [command, action] if command == "runtime" && action == "serve" => runtime_serve(stderr),
        [command, action, ..] if command == "runtime" && action == "serve" => {
            writeln!(stderr, "error: runtime serve is an internal command")
                .expect("stderr write failed");
            64
        }
        [command, action] if command == "runtime" => {
            runtime_command(action, &config, stdout, stderr)
        }
        [command] if command == "ps" => list_processes(&config, stdout, stderr),
        [command] if command == "logs" => list_logs(&config, stdout, stderr),
        [command, pid] if command == "info" => show_process_info(pid, &config, stdout, stderr),
        [command, pid] if command == "kill" => kill_process(pid, &config, stdout, stderr),
        [command, path] if command == "inspect" => inspect_bytecode(path, &config, stdout, stderr),
        [command, path] if command == "explain" => {
            explain_source(path, &config, &mut read_file, stdout, stderr)
        }
        [command, ..] if command == "run" => {
            writeln!(stderr, "error: expected a file or package path\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "check" => {
            writeln!(stderr, "error: expected a file or package path\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "compile" => {
            writeln!(stderr, "error: expected input and output file paths\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "run-bytecode" => {
            writeln!(stderr, "error: expected a bytecode file path\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "debug" => {
            writeln!(stderr, "error: expected a file or package path\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "runtime" => {
            writeln!(
                stderr,
                "error: expected runtime action start, stop, status, restart, metrics, events, or config\n"
            )
            .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "info" => {
            writeln!(stderr, "error: expected a runtime process id\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "logs" => {
            writeln!(
                stderr,
                "error: logs does not accept a process id; use info <pid>\n"
            )
            .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "kill" => {
            writeln!(stderr, "error: expected a runtime process id\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "inspect" => {
            writeln!(stderr, "error: expected a bytecode file path\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        [command, ..] if command == "explain" => {
            writeln!(stderr, "error: expected a source file or package path\n")
                .expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
        _ => {
            writeln!(stderr, "error: unknown command\n").expect("stderr write failed");
            write!(stderr, "{USAGE}").expect("stderr write failed");
            64
        }
    }
}

#[derive(Clone, Debug)]
struct CliConfig {
    runtime_mode: RuntimeMode,
    runtime_home: PathBuf,
    runtime_config: ferrix_runtime::RuntimeConfig,
    output_format: OutputFormat,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RunDisplayOptions {
    stats: bool,
    audit: bool,
    watch: WatchOptions,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CheckDisplayOptions {
    watch: WatchOptions,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WatchOptions {
    enabled: bool,
    once: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CompileDisplayOptions {
    explain_optimizations: bool,
}

impl Default for CliConfig {
    fn default() -> Self {
        let runtime_config = ferrix_runtime::RuntimeConfig::default();
        Self {
            runtime_mode: runtime_config.mode,
            runtime_home: runtime_config.resolved_home(&ferrix_runtime::default_ferrix_home()),
            runtime_config,
            output_format: OutputFormat::Human,
        }
    }
}

fn parse_runtime_options(
    args: &[String],
    stderr: &mut impl io::Write,
) -> Result<(CliConfig, Vec<String>), i32> {
    if let Err(error) = ferrix_runtime::ensure_default_layout() {
        write!(stderr, "{}", error.render()).expect("stderr write failed");
        return Err(error.exit_code);
    }

    let runtime_config =
        match ferrix_runtime::RuntimeConfig::load(&ferrix_runtime::default_config_path()) {
            Ok(config) => config,
            Err(error) => {
                write!(stderr, "{}", error.render()).expect("stderr write failed");
                return Err(error.exit_code);
            }
        };
    let mut config = CliConfig {
        runtime_mode: runtime_config.mode,
        runtime_home: runtime_config.resolved_home(&ferrix_runtime::default_ferrix_home()),
        runtime_config,
        output_format: OutputFormat::Human,
    };
    let mut rest = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--runtime-mode" => {
                let Some(value) = args.get(index + 1) else {
                    writeln!(stderr, "error: expected value after --runtime-mode")
                        .expect("stderr write failed");
                    return Err(64);
                };
                config.runtime_mode = match value.parse::<RuntimeMode>() {
                    Ok(mode) => mode,
                    Err(error) => {
                        writeln!(stderr, "error: {error}").expect("stderr write failed");
                        return Err(64);
                    }
                };
                config.runtime_config.mode = config.runtime_mode;
                index += 2;
            }
            "--runtime-home" => {
                let Some(value) = args.get(index + 1) else {
                    writeln!(stderr, "error: expected value after --runtime-home")
                        .expect("stderr write failed");
                    return Err(64);
                };
                config.runtime_home = PathBuf::from(value);
                config.runtime_config.home = config.runtime_home.clone();
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    writeln!(stderr, "error: expected value after --format")
                        .expect("stderr write failed");
                    return Err(64);
                };
                config.output_format = match value.as_str() {
                    "human" => OutputFormat::Human,
                    "json" => OutputFormat::Json,
                    _ => {
                        writeln!(stderr, "error: invalid output format `{value}`")
                            .expect("stderr write failed");
                        return Err(64);
                    }
                };
                index += 2;
            }
            _ => {
                rest.extend_from_slice(&args[index..]);
                break;
            }
        }
    }

    Ok((config, rest))
}

fn parse_run_display_options(
    args: &[String],
    stderr: &mut impl io::Write,
) -> Option<RunDisplayOptions> {
    let mut options = RunDisplayOptions::default();
    for arg in args {
        match arg.as_str() {
            "--stats" => options.stats = true,
            "--audit" => options.audit = true,
            "--watch" => options.watch.enabled = true,
            "--watch-once" => {
                options.watch.enabled = true;
                options.watch.once = true;
            }
            _ => {
                writeln!(stderr, "error: unknown run option `{arg}`").expect("stderr write failed");
                return None;
            }
        }
    }
    Some(options)
}

fn parse_check_display_options(
    args: &[String],
    stderr: &mut impl io::Write,
) -> Option<CheckDisplayOptions> {
    let mut options = CheckDisplayOptions::default();
    for arg in args {
        match arg.as_str() {
            "--watch" => options.watch.enabled = true,
            "--watch-once" => {
                options.watch.enabled = true;
                options.watch.once = true;
            }
            _ => {
                writeln!(stderr, "error: unknown check option `{arg}`")
                    .expect("stderr write failed");
                return None;
            }
        }
    }
    Some(options)
}

fn parse_compile_display_options(
    args: &[String],
    stderr: &mut impl io::Write,
) -> Option<CompileDisplayOptions> {
    let mut options = CompileDisplayOptions::default();
    for arg in args {
        match arg.as_str() {
            "--explain-optimizations" => options.explain_optimizations = true,
            _ => {
                writeln!(stderr, "error: unknown compile option `{arg}`")
                    .expect("stderr write failed");
                return None;
            }
        }
    }
    Some(options)
}

fn runtime_command(
    action: &str,
    config: &CliConfig,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    match action {
        "start" => match start_runtime_process(config, stderr) {
            Ok(_) => 0,
            Err(error) => write_runtime_error(error, stderr),
        },
        "stop" => match stop_runtime_process(config) {
            Ok(_) => 0,
            Err(error) => write_runtime_error(error, stderr),
        },
        "status" => match runtime_daemon(config).checked_status() {
            Ok(status) => {
                write_status(stdout, &status, config.output_format);
                0
            }
            Err(error) => write_runtime_error(error, stderr),
        },
        "metrics" => match runtime_request_gateway(config).metrics() {
            Ok(metrics) => {
                write_metrics(stdout, &metrics, config.output_format);
                0
            }
            Err(error) => write_runtime_error(error, stderr),
        },
        "events" => match runtime_request_gateway(config).events() {
            Ok(events) => {
                write_events(stdout, &events, config.output_format);
                0
            }
            Err(error) => write_runtime_error(error, stderr),
        },
        "config" => match runtime_request_gateway(config).config() {
            Ok(runtime_config) => {
                write_runtime_config(stdout, &runtime_config, config.output_format);
                0
            }
            Err(error) => write_runtime_error(error, stderr),
        },
        "restart" => {
            let _ = stop_runtime_process(config);
            match start_runtime_process(config, stderr) {
                Ok(_) => 0,
                Err(error) => write_runtime_error(error, stderr),
            }
        }
        _ => {
            writeln!(stderr, "error: unknown runtime action `{action}`")
                .expect("stderr write failed");
            64
        }
    }
}

fn write_runtime_error(error: ferrix_runtime::RuntimeError, stderr: &mut impl io::Write) -> i32 {
    write!(stderr, "{}", error.render()).expect("stderr write failed");
    error.exit_code
}

fn runtime_serve(stderr: &mut impl io::Write) -> i32 {
    let Some(config) = consume_runtime_launch_config() else {
        writeln!(stderr, "error: runtime serve is an internal command")
            .expect("stderr write failed");
        return 64;
    };

    let mut daemon = runtime_daemon(&config);
    match daemon.serve_forever() {
        Ok(()) => 0,
        Err(error) => {
            write!(stderr, "{}", error.render()).expect("stderr write failed");
            error.exit_code
        }
    }
}

fn consume_runtime_launch_config() -> Option<CliConfig> {
    let actual_parent = current_parent_pid()?;
    if !is_runtime_start_parent(actual_parent) {
        return None;
    }

    let path = runtime_launch_path(actual_parent);
    let source = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(&path);

    let mut parent_pid = None;
    let mut runtime_home = None;
    let mut runtime_config = ferrix_runtime::RuntimeConfig::default();
    for line in source.lines() {
        if let Some(value) = line.strip_prefix("parent_pid=") {
            parent_pid = value.parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("runtime_home=") {
            runtime_home = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("request_timeout_ms=") {
            if let Ok(value) = value.parse() {
                runtime_config.request_timeout_ms = value;
            }
        } else if let Some(value) = line.strip_prefix("max_concurrent_processes=") {
            if let Ok(value) = value.parse() {
                runtime_config.max_concurrent_runtime_processes = value;
            }
        } else if let Some(value) = line.strip_prefix("rate_limit_per_second=")
            && let Ok(value) = value.parse()
        {
            runtime_config.rate_limit_per_second = value;
        }
    }

    if parent_pid? != actual_parent {
        return None;
    }

    let runtime_home = runtime_home?;
    runtime_config.home = runtime_home.clone();
    Some(CliConfig {
        runtime_mode: RuntimeMode::Required,
        runtime_home,
        runtime_config,
        output_format: OutputFormat::Human,
    })
}

fn is_runtime_start_parent(parent_pid: u32) -> bool {
    let cmdline_path = format!("/proc/{parent_pid}/cmdline");
    let Ok(cmdline) = fs::read(cmdline_path) else {
        return false;
    };
    parent_cmdline_allows_runtime_serve(&cmdline)
}

fn current_parent_pid() -> Option<u32> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let after_name = stat.rsplit_once(") ")?.1;
    let mut fields = after_name.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

fn parent_cmdline_allows_runtime_serve(cmdline: &[u8]) -> bool {
    let args = cmdline
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect::<Vec<_>>();
    let Some(executable) = args.first() else {
        return false;
    };
    let executable_name = Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if executable_name != "ferrix-cli" && executable_name != "ferrix" {
        return false;
    }

    let mut runtime_mode = RuntimeMode::Embedded;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--runtime-mode" => {
                let Some(value) = args.get(index + 1) else {
                    return false;
                };
                let Ok(mode) = value.parse::<RuntimeMode>() else {
                    return false;
                };
                runtime_mode = mode;
                index += 2;
            }
            "--runtime-home" => {
                if args.get(index + 1).is_none() {
                    return false;
                }
                index += 2;
            }
            _ => break,
        }
    }

    match args.get(index).map(String::as_str) {
        Some("runtime") => matches!(
            args.get(index + 1).map(String::as_str),
            Some("start" | "restart")
        ),
        Some("run" | "run-bytecode" | "debug") => runtime_mode == RuntimeMode::Managed,
        _ => false,
    }
}

fn write_runtime_launch_config(
    config: &CliConfig,
) -> Result<PathBuf, ferrix_runtime::RuntimeError> {
    let dir = PathBuf::from(RUNTIME_LAUNCH_DIR);
    fs::create_dir_all(&dir).map_err(runtime_state_error)?;
    let parent_pid = std::process::id();
    let path = runtime_launch_path(parent_pid);
    let _ = fs::remove_file(&path);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(runtime_state_error)?;
    writeln!(file, "parent_pid={parent_pid}").map_err(runtime_state_error)?;
    writeln!(file, "runtime_home={}", config.runtime_home.display())
        .map_err(runtime_state_error)?;
    writeln!(
        file,
        "request_timeout_ms={}",
        config.runtime_config.request_timeout_ms
    )
    .map_err(runtime_state_error)?;
    writeln!(
        file,
        "max_concurrent_processes={}",
        config.runtime_config.max_concurrent_runtime_processes
    )
    .map_err(runtime_state_error)?;
    writeln!(
        file,
        "rate_limit_per_second={}",
        config.runtime_config.rate_limit_per_second
    )
    .map_err(runtime_state_error)?;
    Ok(path)
}

fn runtime_launch_path(parent_pid: u32) -> PathBuf {
    PathBuf::from(RUNTIME_LAUNCH_DIR).join(format!("{parent_pid}.launch"))
}

fn runtime_state_error(error: io::Error) -> ferrix_runtime::RuntimeError {
    ferrix_runtime::RuntimeError::new(
        66,
        ferrix_runtime::RuntimeErrorKind::DaemonState {
            message: error.to_string(),
        },
    )
}

fn start_runtime_process(
    config: &CliConfig,
    stderr: &mut impl io::Write,
) -> Result<ferrix_runtime::RuntimeStatusReport, ferrix_runtime::RuntimeError> {
    let daemon = runtime_daemon(config);
    if daemon.ping()? {
        return Err(ferrix_runtime::RuntimeError::new(
            70,
            ferrix_runtime::RuntimeErrorKind::ServiceAlreadyRunning,
        ));
    }

    fs::create_dir_all(daemon.home()).map_err(runtime_state_error)?;
    let log_path = daemon.home().join("daemon.log");
    let stdout_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(runtime_state_error)?;
    let stderr_log = stdout_log.try_clone().map_err(runtime_state_error)?;

    let launch_path = write_runtime_launch_config(config)?;
    let exe = env::current_exe().map_err(runtime_state_error)?;
    let spawn_result = Command::new(exe)
        .arg("runtime")
        .arg("serve")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log))
        .spawn();
    if let Err(error) = spawn_result {
        let _ = fs::remove_file(launch_path);
        return Err(runtime_state_error(error));
    }

    if let Err(error) = wait_for_runtime(&daemon, stderr) {
        let _ = fs::remove_file(launch_path);
        return Err(error);
    }
    daemon.status()
}

fn stop_runtime_process(
    config: &CliConfig,
) -> Result<ferrix_runtime::RuntimeStatusReport, ferrix_runtime::RuntimeError> {
    let mut daemon = runtime_daemon(config);
    if !daemon.ping()? {
        let _ = daemon.checked_status();
        return Err(ferrix_runtime::RuntimeError::new(
            70,
            ferrix_runtime::RuntimeErrorKind::ServiceNotRunning,
        ));
    }

    daemon.stop_process()?;
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(25));
        if !daemon.ping()? {
            break;
        }
    }
    daemon.stop()
}

fn runtime_daemon(config: &CliConfig) -> RuntimeDaemon {
    RuntimeDaemon::with_home_and_config(config.runtime_home.clone(), config.runtime_config.clone())
}

fn wait_for_runtime(
    daemon: &RuntimeDaemon,
    _stderr: &mut impl io::Write,
) -> Result<(), ferrix_runtime::RuntimeError> {
    for _ in 0..80 {
        if daemon.ping()? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(ferrix_runtime::RuntimeError::new(
        69,
        ferrix_runtime::RuntimeErrorKind::RuntimeUnavailable {
            mode: RuntimeMode::Required,
        },
    ))
}

fn write_status(
    stdout: &mut impl io::Write,
    status: &ferrix_runtime::RuntimeStatusReport,
    format: OutputFormat,
) {
    if format == OutputFormat::Json {
        writeln!(
            stdout,
            "{{\"runtime\":\"{}\",\"version\":\"{}\",\"protocol\":\"{}\",\"uptime_ms\":{},\"processes\":{},\"active\":{},\"completed\":{},\"failed\":{},\"event_queue\":{},\"events_dropped\":{}}}",
            json_escape(status.health.as_str()),
            json_escape(&status.version),
            status.protocol_version,
            status
                .uptime_ms
                .map_or_else(|| "null".to_string(), |value| value.to_string()),
            status.process_count,
            status.active_process_count,
            status.completed_process_count,
            status.failed_process_count,
            status.event_queue_len,
            status.dropped_event_count
        )
        .expect("stdout write failed");
        return;
    }

    writeln!(stdout, "runtime: {}", status.health.as_str()).expect("stdout write failed");
    writeln!(stdout, "version: {}", status.version).expect("stdout write failed");
    writeln!(stdout, "protocol: {}", status.protocol_version).expect("stdout write failed");
    if let Some(uptime_ms) = status.uptime_ms {
        writeln!(stdout, "uptime_ms: {uptime_ms}").expect("stdout write failed");
    }
    writeln!(stdout, "processes: {}", status.process_count).expect("stdout write failed");
    writeln!(stdout, "active: {}", status.active_process_count).expect("stdout write failed");
    writeln!(stdout, "completed: {}", status.completed_process_count).expect("stdout write failed");
    writeln!(stdout, "failed: {}", status.failed_process_count).expect("stdout write failed");
    writeln!(stdout, "event_queue: {}", status.event_queue_len).expect("stdout write failed");
    writeln!(stdout, "events_dropped: {}", status.dropped_event_count)
        .expect("stdout write failed");
}

fn write_metrics(
    stdout: &mut impl io::Write,
    metrics: &RuntimeMetricsReport,
    format: OutputFormat,
) {
    if format == OutputFormat::Json {
        writeln!(
            stdout,
            "{{\"processes\":{},\"active\":{},\"completed\":{},\"failed\":{},\"events\":{},\"events_dropped\":{},\"checkpoints\":{},\"middleware_requests\":{},\"executed_instructions\":{},\"allocations\":{},\"gc_collections\":{},\"native_calls\":{},\"execution_time_ms\":{}}}",
            metrics.process_count,
            metrics.active_process_count,
            metrics.completed_process_count,
            metrics.failed_process_count,
            metrics.event_queue_len,
            metrics.dropped_event_count,
            metrics.checkpoint_count,
            metrics.middleware_request_count,
            metrics.executed_instructions,
            metrics.allocations,
            metrics.gc_collections,
            metrics.native_calls,
            metrics.execution_time_ms
        )
        .expect("stdout write failed");
        return;
    }

    writeln!(stdout, "processes: {}", metrics.process_count).expect("stdout write failed");
    writeln!(stdout, "active: {}", metrics.active_process_count).expect("stdout write failed");
    writeln!(stdout, "completed: {}", metrics.completed_process_count)
        .expect("stdout write failed");
    writeln!(stdout, "failed: {}", metrics.failed_process_count).expect("stdout write failed");
    writeln!(stdout, "events: {}", metrics.event_queue_len).expect("stdout write failed");
    writeln!(stdout, "events_dropped: {}", metrics.dropped_event_count)
        .expect("stdout write failed");
    writeln!(stdout, "checkpoints: {}", metrics.checkpoint_count).expect("stdout write failed");
    writeln!(
        stdout,
        "middleware_requests: {}",
        metrics.middleware_request_count
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "executed_instructions: {}",
        metrics.executed_instructions
    )
    .expect("stdout write failed");
    writeln!(stdout, "allocations: {}", metrics.allocations).expect("stdout write failed");
    writeln!(stdout, "gc_collections: {}", metrics.gc_collections).expect("stdout write failed");
    writeln!(stdout, "native_calls: {}", metrics.native_calls).expect("stdout write failed");
    writeln!(stdout, "execution_time_ms: {}", metrics.execution_time_ms)
        .expect("stdout write failed");
}

fn write_events(stdout: &mut impl io::Write, events: &[RuntimeEvent], format: OutputFormat) {
    if format == OutputFormat::Json {
        write!(stdout, "{{\"events\":[").expect("stdout write failed");
        for (index, event) in events.iter().enumerate() {
            if index > 0 {
                write!(stdout, ",").expect("stdout write failed");
            }
            write!(
                stdout,
                "{{\"id\":{},\"timestamp_ms\":{},\"kind\":\"{}\",\"severity\":\"{}\",\"process\":{},\"session\":{},\"message\":\"{}\"}}",
                event.id,
                event.timestamp_ms,
                json_escape(runtime_event_kind_name(&event.kind)),
                event.metadata.severity.as_str(),
                event
                    .process_id
                    .map_or_else(|| "null".to_string(), |process| process.0.to_string()),
                event
                    .session_id
                    .map_or_else(|| "null".to_string(), |session| session.0.to_string()),
                json_escape(event.metadata.message.as_deref().unwrap_or_default())
            )
            .expect("stdout write failed");
        }
        writeln!(stdout, "]}}").expect("stdout write failed");
        return;
    }

    writeln!(stdout, "id\tseverity\tkind\tprocess\tsession\tmessage").expect("stdout write failed");
    for event in events {
        writeln!(
            stdout,
            "{}\t{}\t{}\t{}\t{}\t{}",
            event.id,
            event.metadata.severity.as_str(),
            runtime_event_kind_name(&event.kind),
            event
                .process_id
                .map_or_else(|| "-".to_string(), |process| process.0.to_string()),
            event
                .session_id
                .map_or_else(|| "-".to_string(), |session| session.0.to_string()),
            event.metadata.message.as_deref().unwrap_or_default()
        )
        .expect("stdout write failed");
    }
}

fn write_runtime_config(
    stdout: &mut impl io::Write,
    config: &ferrix_runtime::RuntimeConfig,
    format: OutputFormat,
) {
    if format == OutputFormat::Json {
        writeln!(
            stdout,
            "{{\"mode\":\"{}\",\"home\":\"{}\",\"auto_start\":{},\"default_profile\":\"{}\",\"log_level\":\"{}\",\"audit_enabled\":{},\"stats_enabled\":{},\"request_timeout_ms\":{},\"max_concurrent_processes\":{},\"rate_limit_per_second\":{}}}",
            config.mode.as_str(),
            json_escape(&config.home.display().to_string()),
            config.auto_start,
            config.default_profile.as_str(),
            config.log_level.as_str(),
            config.audit_enabled,
            config.stats_enabled,
            config.request_timeout_ms,
            config.max_concurrent_runtime_processes,
            config.rate_limit_per_second
        )
        .expect("stdout write failed");
        return;
    }

    writeln!(stdout, "mode: {}", config.mode.as_str()).expect("stdout write failed");
    writeln!(stdout, "home: {}", config.home.display()).expect("stdout write failed");
    writeln!(stdout, "auto_start: {}", config.auto_start).expect("stdout write failed");
    writeln!(
        stdout,
        "default_profile: {}",
        config.default_profile.as_str()
    )
    .expect("stdout write failed");
    writeln!(stdout, "log_level: {}", config.log_level.as_str()).expect("stdout write failed");
    writeln!(stdout, "audit_enabled: {}", config.audit_enabled).expect("stdout write failed");
    writeln!(stdout, "stats_enabled: {}", config.stats_enabled).expect("stdout write failed");
    writeln!(stdout, "request_timeout_ms: {}", config.request_timeout_ms)
        .expect("stdout write failed");
    writeln!(
        stdout,
        "max_concurrent_processes: {}",
        config.max_concurrent_runtime_processes
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "rate_limit_per_second: {}",
        config.rate_limit_per_second
    )
    .expect("stdout write failed");
}

fn runtime_event_kind_name(kind: &ferrix_runtime::RuntimeEventKind) -> &'static str {
    match kind {
        ferrix_runtime::RuntimeEventKind::RuntimeStarted => "runtime_started",
        ferrix_runtime::RuntimeEventKind::RuntimeStopped => "runtime_stopped",
        ferrix_runtime::RuntimeEventKind::ProcessStarted => "process_started",
        ferrix_runtime::RuntimeEventKind::ProcessCompleted => "process_completed",
        ferrix_runtime::RuntimeEventKind::ProcessFailed => "process_failed",
        ferrix_runtime::RuntimeEventKind::ProcessKilled => "process_killed",
        ferrix_runtime::RuntimeEventKind::DebuggerAttached => "debugger_attached",
        ferrix_runtime::RuntimeEventKind::ProfileSelected(_) => "profile_selected",
        ferrix_runtime::RuntimeEventKind::CheckpointRecorded => "checkpoint_recorded",
        ferrix_runtime::RuntimeEventKind::AuditEvent(_) => "audit_event",
        ferrix_runtime::RuntimeEventKind::ProgramStarted => "program_started",
        ferrix_runtime::RuntimeEventKind::ProgramCompleted => "program_completed",
        ferrix_runtime::RuntimeEventKind::ProgramFailed => "program_failed",
        ferrix_runtime::RuntimeEventKind::NativeFunctionCalled(_) => "native_function_called",
        ferrix_runtime::RuntimeEventKind::CapabilityDenied(_) => "capability_denied",
        ferrix_runtime::RuntimeEventKind::ExceptionThrown => "exception_thrown",
        ferrix_runtime::RuntimeEventKind::ExceptionHandled => "exception_handled",
        ferrix_runtime::RuntimeEventKind::ModuleLoaded(_) => "module_loaded",
        ferrix_runtime::RuntimeEventKind::GcStarted => "gc_started",
        ferrix_runtime::RuntimeEventKind::GcCompleted => "gc_completed",
        ferrix_runtime::RuntimeEventKind::DebuggerBreakpointHit => "debugger_breakpoint_hit",
        ferrix_runtime::RuntimeEventKind::CustomExtensionCalled(_) => "custom_extension_called",
        ferrix_runtime::RuntimeEventKind::InstructionBudgetExceeded => {
            "instruction_budget_exceeded"
        }
    }
}

fn json_string_array(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .collect::<Vec<_>>()
        .join(",")
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn list_processes(
    config: &CliConfig,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    let runtime = runtime_request_gateway(config);
    match runtime.list_processes() {
        Ok(processes) => {
            writeln!(stdout, "pid\tsession\tstatus\tkind\tpath").expect("stdout write failed");
            for process in processes {
                write_process_row(stdout, &process);
            }
            0
        }
        Err(error) => {
            write!(stderr, "{}", error.render()).expect("stderr write failed");
            error.exit_code
        }
    }
}

fn write_process_row(stdout: &mut impl io::Write, process: &RuntimeProcessRecord) {
    writeln!(
        stdout,
        "{}\t{}\t{}\t{}\t{}",
        process.id,
        process.session_id,
        process.status.as_str(),
        process.kind.as_str(),
        process.path.display()
    )
    .expect("stdout write failed");
}

fn list_logs(config: &CliConfig, stdout: &mut impl io::Write, stderr: &mut impl io::Write) -> i32 {
    let runtime = runtime_request_gateway(config);
    match runtime.list_logs() {
        Ok(processes) => {
            writeln!(stdout, "pid\tkind\tstatus\texit\tpath").expect("stdout write failed");
            for process in processes {
                writeln!(
                    stdout,
                    "{}\t{}\t{}\t{}\t{}",
                    process.id,
                    process.kind.as_str(),
                    process.status.as_str(),
                    process
                        .exit_code
                        .map_or_else(|| "-".to_string(), |code| code.to_string()),
                    process.path.display()
                )
                .expect("stdout write failed");
            }
            0
        }
        Err(error) => write_runtime_error(error, stderr),
    }
}

fn show_process_info(
    pid: &str,
    config: &CliConfig,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    let Some(process_id) = parse_process_id(pid, stderr) else {
        return 64;
    };
    let runtime = runtime_request_gateway(config);
    match runtime.process_info(process_id) {
        Ok(process) => {
            let Some(process) = process else {
                writeln!(stderr, "error: unknown runtime process {process_id}")
                    .expect("stderr write failed");
                return 66;
            };
            write_process_info(stdout, &process);
            if let Ok(logs) = runtime.process_output(process_id)
                && !logs.is_empty()
            {
                writeln!(stdout, "output:").expect("stdout write failed");
                write!(stdout, "{logs}").expect("stdout write failed");
            }
            0
        }
        Err(error) => write_runtime_error(error, stderr),
    }
}

fn write_process_info(stdout: &mut impl io::Write, process: &RuntimeProcessRecord) {
    writeln!(stdout, "pid: {}", process.id).expect("stdout write failed");
    writeln!(stdout, "request: {}", process.request_id).expect("stdout write failed");
    writeln!(stdout, "correlation: {}", process.correlation_id).expect("stdout write failed");
    writeln!(stdout, "session: {}", process.session_id).expect("stdout write failed");
    writeln!(stdout, "status: {}", process.status.as_str()).expect("stdout write failed");
    writeln!(stdout, "kind: {}", process.kind.as_str()).expect("stdout write failed");
    writeln!(stdout, "path: {}", process.path.display()).expect("stdout write failed");
    writeln!(
        stdout,
        "exit: {}",
        process
            .exit_code
            .map_or_else(|| "-".to_string(), |code| code.to_string())
    )
    .expect("stdout write failed");
    writeln!(stdout, "started_at_ms: {}", process.started_at_ms).expect("stdout write failed");
    if let Some(ended_at_ms) = process.ended_at_ms {
        writeln!(stdout, "ended_at_ms: {ended_at_ms}").expect("stdout write failed");
    }
    if let Some(error) = &process.last_error {
        writeln!(stdout, "error: {}", error.trim_end()).expect("stdout write failed");
    }
    write_runtime_stats(stdout, &process.stats);
}

fn kill_process(
    pid: &str,
    config: &CliConfig,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    let Some(process_id) = parse_process_id(pid, stderr) else {
        return 64;
    };
    let runtime = runtime_request_gateway(config);
    match runtime.kill_process(process_id) {
        Ok(process) => {
            writeln!(stdout, "killed process {}", process.id).expect("stdout write failed");
            0
        }
        Err(error) => {
            write!(stderr, "{}", error.render()).expect("stderr write failed");
            error.exit_code
        }
    }
}

fn runtime_request_gateway(config: &CliConfig) -> RuntimeGateway {
    let mode = if config.runtime_mode == RuntimeMode::Embedded {
        RuntimeMode::Required
    } else {
        config.runtime_mode
    };
    RuntimeGateway::with_home(mode, config.runtime_home.clone())
}

fn parse_process_id(pid: &str, stderr: &mut impl io::Write) -> Option<RuntimeProcessId> {
    match pid.parse::<u64>() {
        Ok(pid) => Some(RuntimeProcessId(pid)),
        Err(_) => {
            writeln!(stderr, "error: invalid runtime process id `{pid}`")
                .expect("stderr write failed");
            None
        }
    }
}

fn record_cli_history(
    config: &CliConfig,
    kind: RuntimeProcessKind,
    path: &str,
    exit_code: i32,
    output: &str,
    last_error: Option<&str>,
) {
    let mut request = RecordProcessRequest::new(kind, path, exit_code).with_output(output);
    if let Some(error) = last_error {
        request = request.with_last_error(error);
    }
    let _ = RuntimeGateway::with_home(config.runtime_mode, config.runtime_home.clone())
        .record_process(request);
}

fn check_file(
    path: &str,
    config: &CliConfig,
    options: CheckDisplayOptions,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    if options.watch.enabled {
        return watch_check_file(path, config, options, read_file, stdout, stderr);
    }
    check_file_once(path, config, read_file, stderr)
}

fn check_file_once(
    path: &str,
    config: &CliConfig,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stderr: &mut impl io::Write,
) -> i32 {
    let code = match compile_file(path, read_file, stderr) {
        Ok(_) => 0,
        Err(code) => code,
    };
    record_cli_history(
        config,
        RuntimeProcessKind::Check,
        path,
        code,
        "",
        (code != 0).then_some("check failed"),
    );
    code
}

fn compile_bytecode(
    path: &str,
    output: &str,
    config: &CliConfig,
    options: CompileDisplayOptions,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    // Source compilation and bytecode encoding are separated so diagnostics
    // still point at the original source file before serialization happens.
    let (_, compiled) = match compile_file(path, read_file, stderr) {
        Ok(compiled) => compiled,
        Err(code) => {
            record_cli_history(
                config,
                RuntimeProcessKind::Compile,
                path,
                code,
                "",
                Some("compile failed"),
            );
            return code;
        }
    };
    let mut metadata = BytecodeContainerMetadata::for_program(compiled.program.as_program());
    metadata.feature_flags |= compiled.report.analysis.required_feature_flags;
    metadata.required_capabilities = compiled.report.analysis.required_capabilities.clone();
    metadata.optimization_level = u8::from(compiled.report.optimization.changed());
    let bytes = match encode_container(compiled.program.as_program(), Some(metadata)) {
        Ok(bytes) => bytes,
        Err(error) => {
            writeln!(stderr, "error: could not encode bytecode: {error}")
                .expect("stderr write failed");
            record_cli_history(
                config,
                RuntimeProcessKind::Compile,
                path,
                65,
                "",
                Some("bytecode encoding failed"),
            );
            return 65;
        }
    };
    if let Err(error) = fs::write(output, bytes) {
        writeln!(stderr, "error: could not write `{output}`: {error}")
            .expect("stderr write failed");
        record_cli_history(
            config,
            RuntimeProcessKind::Compile,
            path,
            66,
            "",
            Some("bytecode write failed"),
        );
        return 66;
    }
    if options.explain_optimizations {
        write_compile_report(stdout, &compiled.report);
    }
    record_cli_history(config, RuntimeProcessKind::Compile, path, 0, output, None);
    0
}

fn write_compile_report(stdout: &mut impl io::Write, report: &ferrix_compiler::CompileReport) {
    writeln!(
        stdout,
        "features: {}",
        comma_or_none(&report.analysis.required_features)
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "capabilities: {}",
        comma_or_none(&report.analysis.required_capabilities)
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "modules: {}",
        comma_or_none(&report.analysis.module_dependencies)
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "natives: {}",
        comma_or_none(&report.analysis.native_dependencies)
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "bytecode: functions={} instructions={}",
        report.analysis.bytecode_function_count, report.analysis.bytecode_instruction_count
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "optimizer: chunks={} passes={} transformations={} changed={}",
        report.optimization.chunks.len(),
        report.optimization.total_passes(),
        report.optimization.total_transformations(),
        report.optimization.changed()
    )
    .expect("stdout write failed");
    for chunk in &report.optimization.chunks {
        writeln!(
            stdout,
            "  chunk {}: before={} after={} transformations={}",
            chunk.chunk_name,
            chunk.instructions_before,
            chunk.instructions_after,
            chunk.total_transformations()
        )
        .expect("stdout write failed");
        for pass in &chunk.passes {
            writeln!(
                stdout,
                "    {}: changed={} inspected={} transformations={}",
                pass.name, pass.changed, pass.instructions_inspected, pass.transformations_applied
            )
            .expect("stdout write failed");
        }
    }
}

fn inspect_bytecode(
    path: &str,
    config: &CliConfig,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            writeln!(stderr, "error: could not read `{path}`: {error}")
                .expect("stderr write failed");
            return 66;
        }
    };
    let metadata = match inspect_container(&bytes) {
        Ok(metadata) => metadata,
        Err(error) => {
            writeln!(stderr, "error: could not inspect bytecode: {error}")
                .expect("stderr write failed");
            return 65;
        }
    };
    write_inspect_metadata(stdout, &metadata, config.output_format);
    0
}

fn explain_source(
    path: &str,
    config: &CliConfig,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    let (_, compiled) = match compile_file(path, read_file, stderr) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    if config.output_format == OutputFormat::Json {
        write_compile_report_json(stdout, &compiled.report);
    } else {
        write_compile_report(stdout, &compiled.report);
    }
    record_cli_history(
        config,
        RuntimeProcessKind::Check,
        path,
        0,
        "explain\n",
        None,
    );
    0
}

fn write_inspect_metadata(
    stdout: &mut impl io::Write,
    metadata: &BytecodeContainerMetadata,
    format: OutputFormat,
) {
    if format == OutputFormat::Json {
        writeln!(
            stdout,
            "{{\"bytecode_format_version\":{},\"min_ferrix_version\":\"{}\",\"feature_flags\":{},\"required_capabilities\":[{}],\"entry\":{},\"module_name\":{},\"debug_section_present\":{},\"checksum\":{},\"optimization_level\":{},\"import_table_present\":{},\"export_table_present\":{},\"interface_metadata_present\":{}}}",
            metadata.bytecode_format_version,
            json_escape(&metadata.min_ferrix_version),
            metadata.feature_flags,
            json_string_array(&metadata.required_capabilities),
            metadata.entry.0,
            metadata
                .module_name
                .as_ref()
                .map_or_else(|| "null".to_string(), |name| format!("\"{}\"", json_escape(name))),
            metadata.debug_section_present,
            metadata.checksum,
            metadata.optimization_level,
            metadata.import_table_present,
            metadata.export_table_present,
            metadata.interface_metadata_present
        )
        .expect("stdout write failed");
        return;
    }

    writeln!(
        stdout,
        "bytecode_format_version: {}",
        metadata.bytecode_format_version
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "min_ferrix_version: {}",
        metadata.min_ferrix_version
    )
    .expect("stdout write failed");
    writeln!(stdout, "feature_flags: {}", metadata.feature_flags).expect("stdout write failed");
    writeln!(
        stdout,
        "required_capabilities: {}",
        comma_or_none(&metadata.required_capabilities)
    )
    .expect("stdout write failed");
    writeln!(stdout, "entry: {}", metadata.entry.0).expect("stdout write failed");
    writeln!(
        stdout,
        "module_name: {}",
        metadata.module_name.as_deref().unwrap_or("<none>")
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "debug_section_present: {}",
        metadata.debug_section_present
    )
    .expect("stdout write failed");
    writeln!(stdout, "checksum: {}", metadata.checksum).expect("stdout write failed");
    writeln!(
        stdout,
        "optimization_level: {}",
        metadata.optimization_level
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "import_table_present: {}",
        metadata.import_table_present
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "export_table_present: {}",
        metadata.export_table_present
    )
    .expect("stdout write failed");
    writeln!(
        stdout,
        "interface_metadata_present: {}",
        metadata.interface_metadata_present
    )
    .expect("stdout write failed");
}

fn write_compile_report_json(stdout: &mut impl io::Write, report: &ferrix_compiler::CompileReport) {
    writeln!(
        stdout,
        "{{\"features\":[{}],\"capabilities\":[{}],\"modules\":[{}],\"natives\":[{}],\"bytecode\":{{\"functions\":{},\"instructions\":{}}},\"optimizer\":{{\"chunks\":{},\"passes\":{},\"transformations\":{},\"changed\":{}}}}}",
        json_string_array(&report.analysis.required_features),
        json_string_array(&report.analysis.required_capabilities),
        json_string_array(&report.analysis.module_dependencies),
        json_string_array(&report.analysis.native_dependencies),
        report.analysis.bytecode_function_count,
        report.analysis.bytecode_instruction_count,
        report.optimization.chunks.len(),
        report.optimization.total_passes(),
        report.optimization.total_transformations(),
        report.optimization.changed()
    )
    .expect("stdout write failed");
}

fn comma_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "<none>".to_string()
    } else {
        values.join(",")
    }
}

fn run_bytecode(
    path: &str,
    config: &CliConfig,
    options: RunDisplayOptions,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    let runtime = match runtime_gateway(config, stderr) {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    let mut request = RunBytecodeRequest::new(path);
    request.collect_stats = options.stats;
    request.collect_audit = options.audit;
    match runtime.run_bytecode(request) {
        Ok(result) => write_run_result(stdout, result, options),
        Err(error) => {
            let rendered = error.render();
            write!(stderr, "{rendered}").expect("stderr write failed");
            error.exit_code
        }
    }
}

fn run_file(
    path: &str,
    config: &CliConfig,
    options: RunDisplayOptions,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    if options.watch.enabled {
        return watch_run_file(path, config, options, stdout, stderr);
    }
    run_file_once(path, config, options, stdout, stderr)
}

fn run_file_once(
    path: &str,
    config: &CliConfig,
    options: RunDisplayOptions,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    // Normal source execution is delegated to ferrix-runtime so the CLI remains
    // a thin command surface instead of wiring compiler, stdlib, and VM itself.
    let runtime = match runtime_gateway(config, stderr) {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    let mut request = RunSourceRequest::new(path);
    request.collect_stats = options.stats;
    request.collect_audit = options.audit;
    match runtime.run_source(request) {
        Ok(result) => write_run_result(stdout, result, options),
        Err(error) => {
            let rendered = error.render();
            write!(stderr, "{rendered}").expect("stderr write failed");
            error.exit_code
        }
    }
}

fn watch_run_file(
    path: &str,
    config: &CliConfig,
    mut options: RunDisplayOptions,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    options.watch.enabled = false;
    let mut last_stamp = None;
    loop {
        let stamp = watched_path_stamp(Path::new(path));
        if last_stamp != Some(stamp) {
            writeln!(stdout, "watch: running {path}").expect("stdout write failed");
            let code = run_file_once(path, config, options, stdout, stderr);
            if options.watch.once {
                return code;
            }
            last_stamp = Some(stamp);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn watch_check_file(
    path: &str,
    config: &CliConfig,
    options: CheckDisplayOptions,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    let mut last_stamp = None;
    loop {
        let stamp = watched_path_stamp(Path::new(path));
        if last_stamp != Some(stamp) {
            writeln!(stdout, "watch: checking {path}").expect("stdout write failed");
            let code = check_file_once(path, config, read_file, stderr);
            if options.watch.once {
                return code;
            }
            last_stamp = Some(stamp);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn watched_path_stamp(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn write_run_result(
    stdout: &mut impl io::Write,
    result: ferrix_runtime::RunResult,
    options: RunDisplayOptions,
) -> i32 {
    write!(stdout, "{}", result.output).expect("stdout write failed");
    if let Some(value) = result.value_display {
        writeln!(stdout, "{value}").expect("stdout write failed");
    }
    if options.stats {
        write_runtime_stats(stdout, &result.stats);
    }
    if options.audit {
        write_audit_events(stdout, &result.audit_events);
    }
    result.exit_code
}

fn write_runtime_stats(stdout: &mut impl io::Write, stats: &ferrix_runtime::RuntimeStats) {
    writeln!(stdout, "stats:").expect("stdout write failed");
    writeln!(
        stdout,
        "  executed_instructions: {}",
        stats.executed_instructions
    )
    .expect("stdout write failed");
    writeln!(stdout, "  native_calls: {}", stats.native_calls).expect("stdout write failed");
    writeln!(stdout, "  allocations: {}", stats.allocations).expect("stdout write failed");
    writeln!(stdout, "  heap_objects: {}", stats.heap_objects).expect("stdout write failed");
    writeln!(stdout, "  gc_collections: {}", stats.gc_collections).expect("stdout write failed");
    writeln!(
        stdout,
        "  incremental_gc_steps: {}",
        stats.incremental_gc_steps
    )
    .expect("stdout write failed");
    writeln!(stdout, "  max_call_depth: {}", stats.max_call_depth).expect("stdout write failed");
    writeln!(stdout, "  max_register_count: {}", stats.max_register_count)
        .expect("stdout write failed");
    writeln!(stdout, "  thrown_errors: {}", stats.thrown_errors).expect("stdout write failed");
    writeln!(stdout, "  handled_exceptions: {}", stats.handled_exceptions)
        .expect("stdout write failed");
    writeln!(stdout, "  execution_time_ms: {}", stats.execution_time_ms)
        .expect("stdout write failed");
}

fn write_audit_events(stdout: &mut impl io::Write, audit_events: &[String]) {
    writeln!(stdout, "audit:").expect("stdout write failed");
    if audit_events.is_empty() {
        writeln!(stdout, "  <empty>").expect("stdout write failed");
    } else {
        for event in audit_events {
            writeln!(stdout, "  {event}").expect("stdout write failed");
        }
    }
}

fn debug_file(
    path: &str,
    config: &CliConfig,
    stdin: &mut impl io::BufRead,
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> i32 {
    // Debugger source preparation goes through ferrix-runtime so package and
    // import resolution stay aligned with normal execution.
    let runtime = match runtime_gateway(config, stderr) {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    let compiled = match runtime.prepare_debug(DebugRequest::new(path)) {
        Ok(compiled) => compiled,
        Err(error) => {
            let rendered = error.render();
            record_cli_history(
                config,
                RuntimeProcessKind::Debug,
                path,
                error.exit_code,
                "",
                Some(&rendered),
            );
            write!(stderr, "{rendered}").expect("stderr write failed");
            return error.exit_code;
        }
    };
    let sources = compiled.sources;
    let program = compiled.program;

    let mut vm = Vm::new();
    vm.set_capabilities(RuntimeProfile::Cli.default_capabilities().iter().copied());
    ferrix_stdlib::install(&mut vm, program.as_program());
    let outcome = {
        let mut debugger = CliDebugger::new(stdin, stdout, &sources);
        vm.run_program_with_debugger(&program, &mut debugger)
    };

    match outcome {
        Ok(DebugOutcome::Completed(value)) => {
            if value == Value::Nil {
                writeln!(stdout, "debug: finished").expect("stdout write failed");
                record_cli_history(
                    config,
                    RuntimeProcessKind::Debug,
                    path,
                    0,
                    "debug: finished\n",
                    None,
                );
            } else {
                let output = format!("debug: finished with {}\n", display_value(&vm, value));
                write!(stdout, "{output}").expect("stdout write failed");
                record_cli_history(config, RuntimeProcessKind::Debug, path, 0, &output, None);
            }
            0
        }
        Ok(DebugOutcome::Quit) => {
            writeln!(stdout, "debug: quit").expect("stdout write failed");
            record_cli_history(
                config,
                RuntimeProcessKind::Debug,
                path,
                0,
                "debug: quit\n",
                None,
            );
            0
        }
        Err(error) => {
            let diagnostic = error.to_diagnostic_with_program(program.as_program());
            let rendered = sources.render_diagnostic(&diagnostic);
            record_cli_history(
                config,
                RuntimeProcessKind::Debug,
                path,
                70,
                "",
                Some(&rendered),
            );
            write!(stderr, "{rendered}").expect("stderr write failed");
            70
        }
    }
}

fn runtime_gateway(config: &CliConfig, stderr: &mut impl io::Write) -> Result<RuntimeGateway, i32> {
    let mode = config.runtime_mode;
    if mode == RuntimeMode::Managed
        && let Err(error) = start_runtime_process(config, stderr)
    {
        write!(stderr, "{}", error.render()).expect("stderr write failed");
        return Err(error.exit_code);
    }
    Ok(RuntimeGateway::with_home(mode, config.runtime_home.clone()))
}

fn compile_file(
    path: &str,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    stderr: &mut impl io::Write,
) -> Result<(SourceManager, CompileOutput), i32> {
    // Load the import graph into one source manager so parse/codegen/runtime
    // diagnostics can all render against the same file table.
    let input = match resolve_compile_input(Path::new(path)) {
        Ok(input) => input,
        Err(LoadError::Read { path, error }) => {
            writeln!(
                stderr,
                "error: could not read `{}`: {error}",
                path.display()
            )
            .expect("stderr write failed");
            return Err(66);
        }
        Err(LoadError::Manifest { path, message }) => {
            writeln!(
                stderr,
                "error: invalid package manifest `{}`: {message}",
                path.display()
            )
            .expect("stderr write failed");
            return Err(65);
        }
        Err(_) => {
            writeln!(stderr, "error: could not prepare source input").expect("stderr write failed");
            return Err(65);
        }
    };

    let mut sources = SourceManager::new();
    let graph = match load_module_graph(
        &input.entry_path,
        input.package.as_ref(),
        read_file,
        &mut sources,
    ) {
        Ok(graph) => graph,
        Err(LoadError::Read { path, error }) => {
            writeln!(
                stderr,
                "error: could not read `{}`: {error}",
                path.display()
            )
            .expect("stderr write failed");
            return Err(66);
        }
        Err(LoadError::ReadImport {
            importer,
            module,
            path,
            error,
        }) => {
            writeln!(
                stderr,
                "error: could not resolve import `{module}` from `{}` as `{}`: {error}",
                importer.display(),
                path.display()
            )
            .expect("stderr write failed");
            return Err(66);
        }
        Err(LoadError::PackageImport {
            importer,
            package,
            module,
            searched,
        }) => {
            let searched = searched
                .iter()
                .map(|path| format!("`{}`", path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                stderr,
                "error: could not resolve package import `{module}` from `{}` in package `{package}`; searched {searched}",
                importer.display()
            )
            .expect("stderr write failed");
            return Err(66);
        }
        Err(LoadError::Compile(error)) => {
            write!(
                stderr,
                "{}",
                sources.render_diagnostic(&error.to_diagnostic())
            )
            .expect("stderr write failed");
            return Err(65);
        }
        Err(LoadError::ImportCycle { path }) => {
            writeln!(stderr, "error: import cycle involving `{}`", path.display())
                .expect("stderr write failed");
            return Err(65);
        }
        Err(LoadError::Manifest { path, message }) => {
            writeln!(
                stderr,
                "error: invalid package manifest `{}`: {message}",
                path.display()
            )
            .expect("stderr write failed");
            return Err(65);
        }
    };

    let compiled = match compile_program_ast_with_named_modules_report(graph.entry, graph.modules) {
        Ok(program) => program,
        Err(error) => {
            write!(
                stderr,
                "{}",
                sources.render_diagnostic(&error.to_diagnostic())
            )
            .expect("stderr write failed");
            return Err(65);
        }
    };

    Ok((sources, compiled))
}

struct CompileInput {
    /// Source file compiled as the program entrypoint.
    entry_path: PathBuf,
    /// Package metadata used to resolve imports when the input is a package.
    package: Option<PackageContext>,
}

#[derive(Clone)]
struct PackageContext {
    /// Human-readable package name from the manifest.
    name: String,
    /// Absolute package root used as the boundary for local module lookup.
    root: PathBuf,
    /// Absolute module search roots tried in manifest order.
    module_roots: Vec<PathBuf>,
    /// Future external package metadata retained from the manifest.
    dependencies: Vec<PackageDependency>,
}

#[derive(Clone)]
struct PackageDependency {
    /// Dependency package name or locator.
    name: String,
    /// Optional future version requirement parsed from `name@requirement`.
    requirement: Option<String>,
}

struct PackageManifest {
    name: String,
    entry: PathBuf,
    module_roots: Option<Vec<PathBuf>>,
    dependencies: Vec<PackageDependency>,
}

impl PackageContext {
    fn dependency_metadata(&self) -> Vec<(&str, Option<&str>)> {
        self.dependencies
            .iter()
            .map(|dependency| (dependency.name.as_str(), dependency.requirement.as_deref()))
            .collect()
    }
}

fn resolve_compile_input(path: &Path) -> Result<CompileInput, LoadError> {
    if path.is_dir() {
        return load_package_input(path);
    }

    if is_manifest_path(path) {
        let root = path.parent().unwrap_or_else(|| Path::new("."));
        return load_package_input_from_manifest(root, path);
    }

    Ok(CompileInput {
        entry_path: path.to_path_buf(),
        package: None,
    })
}

fn load_package_input(root: &Path) -> Result<CompileInput, LoadError> {
    let manifest_path = MANIFEST_FILES
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| LoadError::Read {
            path: root.join(MANIFEST_FILES[0]),
            error: io::Error::new(io::ErrorKind::NotFound, "package manifest not found"),
        })?;
    load_package_input_from_manifest(root, &manifest_path)
}

fn load_package_input_from_manifest(
    root: &Path,
    manifest_path: &Path,
) -> Result<CompileInput, LoadError> {
    let source = fs::read_to_string(manifest_path).map_err(|error| LoadError::Read {
        path: manifest_path.to_path_buf(),
        error,
    })?;
    let manifest = parse_package_manifest(&source, manifest_path)?;
    let root = normalize_existing_path(root);
    let entry_path = root.join(&manifest.entry);
    let module_roots = manifest
        .module_roots
        .unwrap_or_else(|| default_module_roots(&manifest.entry))
        .into_iter()
        .map(|root_path| root.join(root_path))
        .collect::<Vec<_>>();
    let package = PackageContext {
        name: manifest.name,
        root,
        module_roots,
        dependencies: manifest.dependencies,
    };
    let _ = package.dependency_metadata();

    Ok(CompileInput {
        entry_path,
        package: Some(package),
    })
}

fn parse_package_manifest(source: &str, path: &Path) -> Result<PackageManifest, LoadError> {
    let mut name = None;
    let mut entry = None;
    let mut module_roots = None;
    let mut dependencies = Vec::new();

    for (line_index, raw_line) in source.lines().enumerate() {
        let line_number = line_index + 1;
        let line = strip_manifest_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        let Some((raw_key, raw_value)) = line.split_once('=') else {
            return Err(manifest_error(path, line_number, "expected `key = value`"));
        };
        let key = raw_key.trim();
        let value = raw_value.trim();
        match key {
            "name" => name = Some(parse_manifest_string(value, path, line_number)?),
            "entry" => {
                entry = Some(PathBuf::from(parse_manifest_string(
                    value,
                    path,
                    line_number,
                )?))
            }
            "module_roots" => {
                module_roots = Some(
                    parse_manifest_string_array(value, path, line_number)?
                        .into_iter()
                        .map(PathBuf::from)
                        .collect(),
                );
            }
            "dependencies" => {
                dependencies = parse_manifest_string_array(value, path, line_number)?
                    .into_iter()
                    .map(parse_dependency)
                    .collect();
            }
            _ => return Err(manifest_error(path, line_number, "unknown manifest key")),
        }
    }

    let name = name.ok_or_else(|| manifest_error(path, 0, "missing `name`"))?;
    let entry = entry.ok_or_else(|| manifest_error(path, 0, "missing `entry`"))?;

    Ok(PackageManifest {
        name,
        entry,
        module_roots,
        dependencies,
    })
}

fn strip_manifest_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

fn parse_manifest_string(
    value: &str,
    path: &Path,
    line_number: usize,
) -> Result<String, LoadError> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(manifest_error(path, line_number, "expected quoted string"));
    };
    Ok(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn parse_manifest_string_array(
    value: &str,
    path: &Path,
    line_number: usize,
) -> Result<Vec<String>, LoadError> {
    let Some(inner) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return Err(manifest_error(path, line_number, "expected string array"));
    };
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    inner
        .split(',')
        .map(|item| parse_manifest_string(item.trim(), path, line_number))
        .collect()
}

fn parse_dependency(value: String) -> PackageDependency {
    let Some((name, requirement)) = value.split_once('@') else {
        return PackageDependency {
            name: value,
            requirement: None,
        };
    };
    PackageDependency {
        name: name.to_string(),
        requirement: Some(requirement.to_string()),
    }
}

fn manifest_error(path: &Path, line_number: usize, message: &str) -> LoadError {
    let message = if line_number == 0 {
        message.to_string()
    } else {
        format!("line {line_number}: {message}")
    };
    LoadError::Manifest {
        path: path.to_path_buf(),
        message,
    }
}

fn default_module_roots(entry: &Path) -> Vec<PathBuf> {
    vec![
        entry
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    ]
}

struct LoadedGraph {
    /// Entry file AST compiled as the program's main module.
    entry: ProgramAst,
    /// Imported module ASTs paired with namespace names.
    modules: Vec<ImportedModuleAst>,
}

enum LoadError {
    /// A source file could not be read from disk.
    Read { path: PathBuf, error: io::Error },
    /// Package manifest syntax or required fields are invalid.
    Manifest { path: PathBuf, message: String },
    /// An imported source file could not be resolved relative to its importer.
    ReadImport {
        importer: PathBuf,
        module: String,
        path: PathBuf,
        error: io::Error,
    },
    /// A package-local import was not found under the manifest module roots.
    PackageImport {
        importer: PathBuf,
        package: String,
        module: String,
        searched: Vec<PathBuf>,
    },
    /// Lexing/parsing one source file failed.
    Compile(CompileError),
    /// Recursive imports reached a file already on the active load stack.
    ImportCycle { path: PathBuf },
}

fn load_module_graph(
    entry_path: &Path,
    package: Option<&PackageContext>,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    sources: &mut SourceManager,
) -> Result<LoadedGraph, LoadError> {
    // `loaded` prevents duplicate work; `visiting` detects import cycles.
    let mut loaded = HashSet::new();
    let mut visiting = HashSet::new();
    let mut modules = Vec::new();
    let entry = load_module(
        entry_path,
        package,
        read_file,
        sources,
        &mut loaded,
        &mut visiting,
        &mut modules,
    )?
    .expect("entry module is always loaded");

    Ok(LoadedGraph { entry, modules })
}

fn load_module(
    path: &Path,
    package: Option<&PackageContext>,
    read_file: &mut impl FnMut(&str) -> io::Result<String>,
    sources: &mut SourceManager,
    loaded: &mut HashSet<PathBuf>,
    visiting: &mut HashSet<PathBuf>,
    modules: &mut Vec<ImportedModuleAst>,
) -> Result<Option<ProgramAst>, LoadError> {
    let key = module_key(path);
    if loaded.contains(&key) {
        return Ok(None);
    }
    if !visiting.insert(key.clone()) {
        return Err(LoadError::ImportCycle {
            path: path.to_path_buf(),
        });
    }

    let path_name = path.display().to_string();
    let source = read_file(&path_name).map_err(|error| LoadError::Read {
        path: path.to_path_buf(),
        error,
    })?;
    let file_id = sources.add_file(path_name, source.clone());
    let ast = parse_source_with_file_id(&source, file_id).map_err(LoadError::Compile)?;

    for module in module_imports(&ast) {
        let import_path = resolve_import(path, module, package)?;
        let loaded_module = load_module(
            &import_path,
            package,
            read_file,
            sources,
            loaded,
            visiting,
            modules,
        )
        .map_err(|error| match error {
            LoadError::Read {
                path: missing_path,
                error,
            } => LoadError::ReadImport {
                importer: path.to_path_buf(),
                module: module.to_string(),
                path: missing_path,
                error,
            },
            error => error,
        })?;
        if let Some(module_ast) = loaded_module {
            modules.push(ImportedModuleAst {
                name: module.to_string(),
                ast: module_ast,
            });
        }
    }

    visiting.remove(&key);
    loaded.insert(key);
    Ok(Some(ast))
}

fn module_imports(ast: &ProgramAst) -> impl Iterator<Item = &str> {
    ast.statements.iter().filter_map(|stmt| match stmt {
        Stmt::Import { module, .. } => Some(module.as_str()),
        _ => None,
    })
}

fn resolve_import(
    importer: &Path,
    module: &str,
    package: Option<&PackageContext>,
) -> Result<PathBuf, LoadError> {
    let module_path = module_file_path(module);
    let Some(package) = package else {
        return Ok(importer
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(module_path));
    };

    let importer_key = module_key(importer);
    if !importer_key.starts_with(&package.root) {
        return Err(LoadError::PackageImport {
            importer: importer.to_path_buf(),
            package: package.name.clone(),
            module: module.to_string(),
            searched: Vec::new(),
        });
    }

    let searched = package
        .module_roots
        .iter()
        .map(|root| root.join(&module_path))
        .collect::<Vec<_>>();
    searched
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| LoadError::PackageImport {
            importer: importer.to_path_buf(),
            package: package.name.clone(),
            module: module.to_string(),
            searched,
        })
}

fn module_file_path(module: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for segment in module.split('.') {
        path.push(segment);
    }
    path.set_extension("fx");
    path
}

fn module_key(path: &Path) -> PathBuf {
    normalize_existing_path(path)
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    fs::canonicalize(&absolute).unwrap_or(absolute)
}

fn is_manifest_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| MANIFEST_FILES.contains(&name))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Breakpoint {
    /// Specific function to stop in, or `None` for any function.
    Instruction {
        function: Option<FunctionId>,
        instruction_ip: usize,
    },
    /// Source line breakpoint, optionally scoped to a file name/path.
    SourceLine { file: Option<String>, line: usize },
}

enum DebugMode {
    /// Stop before every instruction.
    Step,
    /// Run until program end or a matching breakpoint.
    Continue,
}

/// Small interactive debugger used by `ferrix debug`.
struct CliDebugger<'a, I, W>
where
    I: io::BufRead,
    W: io::Write,
{
    input: &'a mut I,
    output: &'a mut W,
    sources: &'a SourceManager,
    mode: DebugMode,
    breakpoints: HashSet<Breakpoint>,
    selected_frame: usize,
    watches: Vec<String>,
}

impl<'a, I, W> CliDebugger<'a, I, W>
where
    I: io::BufRead,
    W: io::Write,
{
    fn new(input: &'a mut I, output: &'a mut W, sources: &'a SourceManager) -> Self {
        Self {
            input,
            output,
            sources,
            mode: DebugMode::Step,
            breakpoints: HashSet::new(),
            selected_frame: 0,
            watches: Vec::new(),
        }
    }

    fn should_stop(&self, event: &DebugEvent<'_>) -> bool {
        // Function-scoped breakpoints win when present; global breakpoints are
        // useful for tiny programs where instruction ids are enough.
        matches!(self.mode, DebugMode::Step)
            || self.breakpoints.contains(&Breakpoint::Instruction {
                function: Some(event.function),
                instruction_ip: event.instruction_ip,
            })
            || self.breakpoints.contains(&Breakpoint::Instruction {
                function: None,
                instruction_ip: event.instruction_ip,
            })
            || self.matches_source_breakpoint(event)
    }

    fn matches_source_breakpoint(&self, event: &DebugEvent<'_>) -> bool {
        let Some((location, line_text_ip)) = self.source_line_hit(event) else {
            return false;
        };

        self.breakpoints.iter().any(|breakpoint| {
            let Breakpoint::SourceLine { file, line } = breakpoint else {
                return false;
            };
            *line == location.line
                && line_text_ip == event.instruction_ip
                && file
                    .as_deref()
                    .is_none_or(|file| source_file_matches(self.sources, location, file))
        })
    }

    fn source_line_hit(&self, event: &DebugEvent<'_>) -> Option<(SourceLocation, usize)> {
        let location = self.sources.location(event.source_span?)?;
        let function = event.program.function(event.function)?;
        let chunk = function.chunk()?;
        let first_ip = chunk.source_map.iter().enumerate().find_map(|(ip, span)| {
            let span = span.as_ref().copied()?;
            let span_location = self.sources.location(span)?;
            (span_location.file_id == location.file_id && span_location.line == location.line)
                .then_some(ip)
        })?;
        Some((location, first_ip))
    }

    fn print_stop(&mut self, event: &DebugEvent<'_>) {
        self.normalize_selected_frame(event);
        writeln!(
            self.output,
            "stopped at {} {} ip={}: {}",
            event.function,
            event.function_name,
            event.instruction_ip,
            format_instruction(event.instruction)
        )
        .expect("stdout write failed");
        if let Some(span) = event.source_span
            && let Some(location) = self.sources.location(span)
        {
            let name = self
                .sources
                .file(location.file_id)
                .map(|file| file.name.as_str())
                .unwrap_or("<source>");
            writeln!(
                self.output,
                " --> {name}:{}:{}",
                location.line, location.column
            )
            .expect("stdout write failed");
            if let Some(line) = self.sources.line_text(location.file_id, location.line) {
                writeln!(self.output, "  | {line}").expect("stdout write failed");
            }
        }
        self.print_watches(event);
    }

    fn command_loop(&mut self, event: &DebugEvent<'_>) -> DebugAction {
        // This loop keeps state-changing debugger commands local to the CLI
        // while the VM only sees Step/Continue/Quit actions.
        loop {
            write!(self.output, "debug> ").expect("stdout write failed");
            self.output.flush().expect("stdout flush failed");

            let mut line = String::new();
            let bytes = self.input.read_line(&mut line).expect("stdin read failed");
            if bytes == 0 {
                return DebugAction::Quit;
            }

            let command = line.trim();
            if command.is_empty() || matches!(command, "s" | "step" | "n" | "next") {
                self.mode = DebugMode::Step;
                return DebugAction::Step;
            }
            if matches!(command, "c" | "continue") {
                self.mode = DebugMode::Continue;
                return DebugAction::Continue;
            }
            if matches!(command, "q" | "quit") {
                return DebugAction::Quit;
            }
            if matches!(command, "r" | "registers" | "regs") {
                self.print_registers(event);
                continue;
            }
            if matches!(command, "bt" | "stack" | "frames") {
                self.print_stack(event);
                continue;
            }
            if matches!(command, "frame") {
                self.print_selected_frame(event);
                continue;
            }
            if let Some(spec) = command.strip_prefix("frame ") {
                self.select_frame(event, spec.trim());
                continue;
            }
            if matches!(command, "i" | "instruction") {
                self.print_stop(event);
                continue;
            }
            if matches!(command, "disasm" | "disassemble" | "u") {
                self.print_disassembly(event, 2);
                continue;
            }
            if let Some(spec) = command
                .strip_prefix("disasm ")
                .or_else(|| command.strip_prefix("disassemble "))
                .or_else(|| command.strip_prefix("u "))
            {
                let radius = spec.trim().parse().unwrap_or(2);
                self.print_disassembly(event, radius);
                continue;
            }
            if let Some(expr) = command.strip_prefix("watch ") {
                self.add_watch(event, expr.trim());
                continue;
            }
            if matches!(command, "watches" | "watch") {
                self.print_watches(event);
                continue;
            }
            if let Some(spec) = command
                .strip_prefix("unwatch ")
                .or_else(|| command.strip_prefix("uw "))
            {
                self.remove_watch(spec.trim());
                continue;
            }
            if matches!(command, "h" | "help") {
                self.print_help();
                continue;
            }
            if let Some(spec) = command
                .strip_prefix("break ")
                .or_else(|| command.strip_prefix("b "))
            {
                self.set_breakpoint(event, spec.trim());
                continue;
            }
            if let Some(spec) = command
                .strip_prefix("clear ")
                .or_else(|| command.strip_prefix("cl "))
            {
                self.clear_breakpoint(event, spec.trim());
                continue;
            }
            if matches!(command, "clear" | "cl") {
                self.breakpoints.clear();
                writeln!(self.output, "cleared all breakpoints").expect("stdout write failed");
                continue;
            }

            writeln!(self.output, "unknown command `{command}`; type `help`")
                .expect("stdout write failed");
        }
    }

    fn print_registers(&mut self, event: &DebugEvent<'_>) {
        let Some(frame) = self.selected_frame_view(event) else {
            writeln!(self.output, "frame #{} is unavailable", self.selected_frame)
                .expect("stdout write failed");
            return;
        };

        writeln!(
            self.output,
            "registers for frame #{} {} ({}, ip={})",
            self.selected_frame, frame.name, frame.function, frame.ip
        )
        .expect("stdout write failed");

        if frame.registers.is_empty() {
            writeln!(self.output, "registers: <empty>").expect("stdout write failed");
            return;
        }

        for (index, value) in frame.registers.iter().copied().enumerate() {
            writeln!(
                self.output,
                "r{index} = {}",
                display_heap_value(event.heap, value)
            )
            .expect("stdout write failed");
        }
    }

    fn print_stack(&mut self, event: &DebugEvent<'_>) {
        self.normalize_selected_frame(event);
        for (index, frame) in event.frames.iter().rev().enumerate() {
            let view = self.frame_view(event, index, frame);
            let selected = if index == self.selected_frame {
                " *"
            } else {
                ""
            };
            writeln!(
                self.output,
                "#{index}{selected} at {} ({}, ip={})",
                view.name, view.function, view.ip
            )
            .expect("stdout write failed");
        }
    }

    fn print_selected_frame(&mut self, event: &DebugEvent<'_>) {
        self.normalize_selected_frame(event);
        let Some(frame) = self.selected_frame_view(event) else {
            writeln!(self.output, "frame #{} is unavailable", self.selected_frame)
                .expect("stdout write failed");
            return;
        };

        writeln!(
            self.output,
            "selected frame #{} at {} ({}, ip={})",
            self.selected_frame, frame.name, frame.function, frame.ip
        )
        .expect("stdout write failed");
    }

    fn select_frame(&mut self, event: &DebugEvent<'_>, spec: &str) {
        let Ok(index) = spec.parse::<usize>() else {
            writeln!(self.output, "invalid frame `{spec}`").expect("stdout write failed");
            return;
        };

        if index >= event.frames.len() {
            writeln!(
                self.output,
                "frame #{index} is unavailable; stack has {} frame(s)",
                event.frames.len()
            )
            .expect("stdout write failed");
            return;
        }

        self.selected_frame = index;
        self.print_selected_frame(event);
    }

    fn add_watch(&mut self, event: &DebugEvent<'_>, expr: &str) {
        if expr.is_empty() {
            writeln!(self.output, "watch expression is empty").expect("stdout write failed");
            return;
        }

        self.watches.push(expr.to_string());
        let index = self.watches.len() - 1;
        match self.evaluate_watch(event, expr) {
            Ok(value) => writeln!(
                self.output,
                "watch #{index}: {expr} = {}",
                display_heap_value(event.heap, value)
            ),
            Err(error) => writeln!(self.output, "watch #{index}: {expr} ({error})"),
        }
        .expect("stdout write failed");
    }

    fn remove_watch(&mut self, spec: &str) {
        let Ok(index) = spec.parse::<usize>() else {
            writeln!(self.output, "invalid watch `{spec}`").expect("stdout write failed");
            return;
        };

        if index >= self.watches.len() {
            writeln!(self.output, "watch #{index} is unavailable").expect("stdout write failed");
            return;
        }

        let expr = self.watches.remove(index);
        writeln!(self.output, "removed watch #{index}: {expr}").expect("stdout write failed");
    }

    fn print_watches(&mut self, event: &DebugEvent<'_>) {
        for (index, expr) in self.watches.iter().enumerate() {
            match self.evaluate_watch(event, expr) {
                Ok(value) => writeln!(
                    self.output,
                    "watch #{index}: {expr} = {}",
                    display_heap_value(event.heap, value)
                ),
                Err(error) => writeln!(self.output, "watch #{index}: {expr} ({error})"),
            }
            .expect("stdout write failed");
        }
    }

    fn evaluate_watch(&self, event: &DebugEvent<'_>, expr: &str) -> Result<Value, String> {
        let frame = self
            .selected_frame_view(event)
            .ok_or_else(|| format!("frame #{} is unavailable", self.selected_frame))?;

        if let Some(register) = parse_register(expr) {
            return frame
                .registers
                .get(register)
                .copied()
                .ok_or_else(|| format!("register r{register} is unavailable"));
        }

        let function = event
            .program
            .function(frame.function)
            .ok_or_else(|| format!("function {} is unavailable", frame.function))?;
        let chunk = function
            .chunk()
            .ok_or_else(|| format!("{} is native and has no locals", frame.function))?;
        let Some((register, _)) = chunk
            .debug_local_names
            .iter()
            .enumerate()
            .find(|(_, name)| name.as_deref() == Some(expr))
        else {
            return Err(format!("unknown watch expression `{expr}`"));
        };

        frame
            .registers
            .get(register)
            .copied()
            .ok_or_else(|| format!("local `{expr}` is unavailable"))
    }

    fn print_disassembly(&mut self, event: &DebugEvent<'_>, radius: usize) {
        let Some(frame) = self.selected_frame_view(event) else {
            writeln!(self.output, "frame #{} is unavailable", self.selected_frame)
                .expect("stdout write failed");
            return;
        };
        let Some(function) = event.program.function(frame.function) else {
            writeln!(self.output, "function {} is unavailable", frame.function)
                .expect("stdout write failed");
            return;
        };
        let Some(chunk) = function.chunk() else {
            writeln!(
                self.output,
                "{} is native and has no bytecode",
                frame.function
            )
            .expect("stdout write failed");
            return;
        };

        let start = frame.ip.saturating_sub(radius);
        let end = frame
            .ip
            .saturating_add(radius)
            .saturating_add(1)
            .min(chunk.instructions.len());
        writeln!(
            self.output,
            "disassembly for frame #{} {} ({}, ip={})",
            self.selected_frame, frame.name, frame.function, frame.ip
        )
        .expect("stdout write failed");
        for ip in start..end {
            let marker = if ip == frame.ip { "=>" } else { "  " };
            writeln!(
                self.output,
                "{marker} {ip:04} {}",
                format_instruction(&chunk.instructions[ip])
            )
            .expect("stdout write failed");
        }
    }

    fn print_help(&mut self) {
        writeln!(
            self.output,
            "\
commands:
  step | s              execute one instruction
  continue | c          run until program end or breakpoint
  break <ip>            stop at instruction ip in any function
  break <fn>:<ip>       stop at instruction ip in function id or name
  break line <line>     stop at source line in any file
  break <file>:<line>   stop at source line in a file/path
  clear [breakpoint]    clear one breakpoint or all breakpoints
  registers | r         print selected-frame registers
  stack | bt | frames   print call stack
  frame [index]         select or print the inspected frame
  watch <expr>          watch register rN or a debug local name
  watches               print watch expressions
  unwatch <index>       remove a watch expression
  disasm [radius]       print bytecode around selected frame
  instruction | i       print current instruction
  quit | q              stop debugging"
        )
        .expect("stdout write failed");
    }

    fn set_breakpoint(&mut self, event: &DebugEvent<'_>, spec: &str) {
        let Some(breakpoint) = parse_breakpoint(event, spec) else {
            writeln!(self.output, "invalid breakpoint `{spec}`").expect("stdout write failed");
            return;
        };
        self.breakpoints.insert(breakpoint.clone());
        self.print_breakpoint("set", &breakpoint);
    }

    fn clear_breakpoint(&mut self, event: &DebugEvent<'_>, spec: &str) {
        let Some(breakpoint) = parse_breakpoint(event, spec) else {
            writeln!(self.output, "invalid breakpoint `{spec}`").expect("stdout write failed");
            return;
        };
        if self.breakpoints.remove(&breakpoint) {
            self.print_breakpoint("cleared", &breakpoint);
        } else {
            self.print_breakpoint("not found", &breakpoint);
        }
    }

    fn print_breakpoint(&mut self, prefix: &str, breakpoint: &Breakpoint) {
        match breakpoint {
            Breakpoint::Instruction {
                function: Some(function),
                instruction_ip,
            } => writeln!(
                self.output,
                "{prefix} breakpoint at {function}:{instruction_ip}"
            ),
            Breakpoint::Instruction {
                function: None,
                instruction_ip,
            } => writeln!(self.output, "{prefix} breakpoint at ip={instruction_ip}"),
            Breakpoint::SourceLine {
                file: Some(file),
                line,
            } => writeln!(self.output, "{prefix} breakpoint at {file}:{line}"),
            Breakpoint::SourceLine { file: None, line } => {
                writeln!(self.output, "{prefix} breakpoint at line {line}")
            }
        }
        .expect("stdout write failed");
    }

    fn normalize_selected_frame(&mut self, event: &DebugEvent<'_>) {
        if self.selected_frame >= event.frames.len() {
            self.selected_frame = 0;
        }
    }

    fn selected_frame_view<'event>(
        &self,
        event: &'event DebugEvent<'_>,
    ) -> Option<FrameView<'event>> {
        let frame = event.frames.iter().rev().nth(self.selected_frame)?;
        Some(self.frame_view(event, self.selected_frame, frame))
    }

    fn frame_view<'event>(
        &self,
        event: &'event DebugEvent<'_>,
        index: usize,
        frame: &'event CallFrame,
    ) -> FrameView<'event> {
        let function = event.program.function(frame.function_id);
        let name = function
            .map(|function| function.name.as_str())
            .unwrap_or("<unknown>");
        let registers = if index == 0 {
            event.registers
        } else {
            &frame.registers
        };
        let ip = if index == 0 {
            event.instruction_ip
        } else {
            frame.ip
        };

        FrameView {
            name,
            function: frame.function_id,
            ip,
            registers,
        }
    }
}

impl<I, W> Debugger for CliDebugger<'_, I, W>
where
    I: io::BufRead,
    W: io::Write,
{
    fn before_instruction(&mut self, event: DebugEvent<'_>) -> DebugAction {
        if !self.should_stop(&event) {
            return DebugAction::Continue;
        }

        self.mode = DebugMode::Step;
        self.print_stop(&event);
        self.command_loop(&event)
    }
}

fn parse_breakpoint(event: &DebugEvent<'_>, spec: &str) -> Option<Breakpoint> {
    if let Some(line) = spec.strip_prefix("line ") {
        return Some(Breakpoint::SourceLine {
            file: None,
            line: line.trim().parse().ok()?,
        });
    }

    if let Some((function, ip)) = spec.split_once(':') {
        let target = function.trim();
        let ip_or_line = ip.trim().parse().ok()?;
        if let Some(function) = parse_function(event, target) {
            return Some(Breakpoint::Instruction {
                function: Some(function),
                instruction_ip: ip_or_line,
            });
        }

        return Some(Breakpoint::SourceLine {
            file: Some(target.to_string()),
            line: ip_or_line,
        });
    }

    Some(Breakpoint::Instruction {
        function: None,
        instruction_ip: spec.parse().ok()?,
    })
}

struct FrameView<'a> {
    name: &'a str,
    function: FunctionId,
    ip: usize,
    registers: &'a [Value],
}

fn parse_register(expr: &str) -> Option<usize> {
    expr.strip_prefix('r')?.parse().ok()
}

fn source_file_matches(sources: &SourceManager, location: SourceLocation, spec: &str) -> bool {
    let Some(file) = sources.file(location.file_id) else {
        return false;
    };
    if file.name == spec || file.name.ends_with(spec) {
        return true;
    }

    Path::new(&file.name)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == spec)
}

fn parse_function(event: &DebugEvent<'_>, spec: &str) -> Option<FunctionId> {
    let id = spec.strip_prefix("fn#").unwrap_or(spec);
    if let Ok(id) = id.parse::<u16>() {
        return Some(FunctionId(id));
    }

    event
        .program
        .functions
        .iter()
        .enumerate()
        .find_map(|(index, function)| {
            if function.name == spec {
                Some(FunctionId(index as u16))
            } else {
                None
            }
        })
}

fn display_value(vm: &Vm, value: Value) -> String {
    display_heap_value(vm.heap(), value)
}

fn display_heap_value(heap: &Heap, value: Value) -> String {
    display_heap_value_inner(heap, value, 0)
}

fn display_heap_value_inner(heap: &Heap, value: Value, depth: usize) -> String {
    if depth >= 8 {
        return "...".to_string();
    }

    match value {
        Value::Obj(reference) => match heap.get(reference) {
            Ok(Obj::String(value)) => value.clone(),
            Ok(Obj::Array(values)) => {
                let values = values
                    .iter()
                    .copied()
                    .map(|value| display_heap_value_inner(heap, value, depth + 1))
                    .collect::<Vec<_>>();
                format!("[{}]", values.join(", "))
            }
            Ok(Obj::Map(entries)) => {
                let entries = entries
                    .iter()
                    .map(|(key, value)| {
                        format!(
                            "{}: {}",
                            display_heap_value_inner(heap, *key, depth + 1),
                            display_heap_value_inner(heap, *value, depth + 1)
                        )
                    })
                    .collect::<Vec<_>>();
                format!("{{{}}}", entries.join(", "))
            }
            Ok(Obj::Record(fields)) => {
                let fields = fields
                    .iter()
                    .map(|(field, value)| {
                        format!(
                            "{}: {}",
                            field,
                            display_heap_value_inner(heap, *value, depth + 1)
                        )
                    })
                    .collect::<Vec<_>>();
                format!("{{{}}}", fields.join(", "))
            }
            _ => value.to_string(),
        },
        _ => value.to_string(),
    }
}
