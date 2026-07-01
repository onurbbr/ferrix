//! Runtime service request/result tests.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ferrix_core::bytecode::encode_program;
use ferrix_runtime::{
    RunBytecodeRequest, RunSourceRequest, RuntimeDaemon, RuntimeEventBus, RuntimeEventKind,
    RuntimeGateway, RuntimeMode, RuntimeProcessKind, RuntimeProcessStatus, RuntimeService,
};

#[test]
fn runtime_runs_source_file() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");

    let result = RuntimeService::new()
        .run_source(RunSourceRequest::new(&file))
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.value_display.as_deref(), Some("42"));
    assert!(result.output.is_empty());
}

#[test]
fn embedded_gateway_runs_source_file() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");

    let result = RuntimeGateway::embedded()
        .run_source(RunSourceRequest::new(&file))
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.value_display.as_deref(), Some("42"));
}

#[test]
fn required_gateway_reports_missing_runtime_daemon() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");
    let runtime_home = dir.join("runtime");

    let error = RuntimeGateway::with_home(RuntimeMode::Required, runtime_home)
        .run_source(RunSourceRequest::new(&file))
        .unwrap_err();

    assert_eq!(error.exit_code, 69);
    assert_eq!(
        error.render(),
        "Ferrix runtime is not running.\nStart it with: ferrix runtime start\n"
    );
}

#[test]
fn required_gateway_checks_runtime_before_metadata_requests() {
    let dir = temp_dir();
    let runtime_home = dir.join("runtime");

    let error = RuntimeGateway::with_home(RuntimeMode::Required, runtime_home)
        .list_logs()
        .unwrap_err();

    assert_eq!(error.exit_code, 69);
    assert_eq!(
        error.render(),
        "Ferrix runtime is not running.\nStart it with: ferrix runtime start\n"
    );
}

#[test]
fn managed_gateway_starts_runtime_before_metadata_requests() {
    let dir = temp_dir();
    let runtime_home = dir.join("runtime");
    let gateway = RuntimeGateway::with_home(RuntimeMode::Managed, &runtime_home);

    let logs = gateway.list_logs().unwrap();

    assert!(logs.is_empty());
    let mut daemon = RuntimeDaemon::with_home(runtime_home);
    assert!(daemon.status().unwrap().is_serving());
    daemon.stop().unwrap();
}

#[test]
fn daemon_lifecycle_reports_health_status() {
    let dir = temp_dir();
    let mut daemon = RuntimeDaemon::with_home(dir.join("runtime"));

    let stopped = daemon.status().unwrap();
    assert!(!stopped.is_serving());

    let started = daemon.start().unwrap();
    assert!(started.is_serving());
    assert_eq!(started.process_count, 0);

    let stopped = daemon.stop().unwrap();
    assert!(!stopped.is_serving());
}

#[test]
fn daemon_runs_source_and_records_process_logs_and_checkpoint() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "\
print(\"hello\");
return 42;
",
    );
    let mut daemon = RuntimeDaemon::with_home(dir.join("runtime"));
    daemon.start().unwrap();

    let result = daemon.run_source(RunSourceRequest::new(&file)).unwrap();

    assert_eq!(result.value_display.as_deref(), Some("42"));
    assert!(daemon.list_processes().unwrap().is_empty());
    let history = daemon.list_history().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, RuntimeProcessStatus::Completed);
    assert_eq!(history[0].kind, RuntimeProcessKind::Run);
    assert_eq!(daemon.logs(history[0].id).unwrap(), "hello\n42\n");
    assert_eq!(daemon.checkpoints().unwrap().len(), 1);
}

#[test]
fn managed_gateway_starts_daemon_and_records_completed_process() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 42;\n");
    let runtime_home = dir.join("runtime");

    let result = RuntimeGateway::with_home(RuntimeMode::Managed, &runtime_home)
        .run_source(RunSourceRequest::new(&file))
        .unwrap();

    assert_eq!(result.value_display.as_deref(), Some("42"));
    let daemon = RuntimeDaemon::with_home(runtime_home);
    let status = daemon.status().unwrap();
    assert!(status.is_serving());
    assert_eq!(status.completed_process_count, 1);
}

#[test]
fn event_bus_drops_oldest_events_when_capacity_is_reached() {
    let mut bus = RuntimeEventBus::with_capacity(2);

    bus.publish(RuntimeEventKind::RuntimeStarted, None, None);
    bus.publish(RuntimeEventKind::ProcessStarted, None, None);
    bus.publish(RuntimeEventKind::ProcessCompleted, None, None);

    let events = bus.events();
    assert_eq!(events.len(), 2);
    assert_eq!(bus.dropped_events(), 1);
    assert_eq!(events[0].kind, RuntimeEventKind::ProcessStarted);
    assert_eq!(events[1].kind, RuntimeEventKind::ProcessCompleted);
}

#[test]
fn runtime_mode_parses_configuration_names() {
    assert_eq!(
        "embedded".parse::<RuntimeMode>().unwrap(),
        RuntimeMode::Embedded
    );
    assert_eq!(
        "required".parse::<RuntimeMode>().unwrap(),
        RuntimeMode::Required
    );
    assert_eq!(
        "managed".parse::<RuntimeMode>().unwrap(),
        RuntimeMode::Managed
    );
    assert!("daemon".parse::<RuntimeMode>().is_err());
}

#[test]
fn runtime_captures_stdlib_output() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "\
print(\"hello\");
return 42;
",
    );

    let result = RuntimeService::new()
        .run_source(RunSourceRequest::new(&file))
        .unwrap();

    assert_eq!(result.output, "hello\n");
    assert_eq!(result.value_display.as_deref(), Some("42"));
}

#[test]
fn runtime_runs_source_with_static_imports() {
    let dir = temp_dir();
    write_file(
        &dir,
        "math.fx",
        "\
fn add(a, b) {
    return a + b;
}
",
    );
    let file = write_file(
        &dir,
        "main.fx",
        "\
import math;
return math.add(40, 2);
",
    );

    let result = RuntimeService::new()
        .run_source(RunSourceRequest::new(&file))
        .unwrap();

    assert_eq!(result.value_display.as_deref(), Some("42"));
}

#[test]
fn runtime_runs_bytecode_file() {
    let dir = temp_dir();
    let source = write_file(&dir, "main.fx", "return 40 + 2;\n");
    let bytecode = dir.join("main.fxb");
    let runtime = RuntimeService::new();
    let compiled = runtime.compile_source_path(&source).unwrap();
    fs::write(
        &bytecode,
        encode_program(compiled.program.as_program()).unwrap(),
    )
    .unwrap();

    let result = runtime
        .run_bytecode(RunBytecodeRequest::new(&bytecode))
        .unwrap();

    assert_eq!(result.value_display.as_deref(), Some("42"));
}

fn temp_dir() -> PathBuf {
    let mut dir = std::env::temp_dir();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    dir.push(format!("ferrix-runtime-test-{unique}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, source).unwrap();
    path
}
