//! Runtime service request/result tests.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ferrix_core::{
    Value,
    bytecode::{
        BytecodeContainerMetadata, FEATURE_CUSTOM_EXTENSIONS, encode_container, encode_program,
    },
};
use ferrix_runtime::{
    CustomExtension, CustomExtensionMetadata, ExtensionCostClass, HostCapability,
    RunBytecodeRequest, RunSourceRequest, RuntimeDaemon, RuntimeEventBus, RuntimeEventKind,
    RuntimeEventMetadata, RuntimeEventSeverity, RuntimeExtensionRegistry, RuntimeGateway,
    RuntimeMode, RuntimePolicy, RuntimeProcessKind, RuntimeProcessStatus, RuntimeProfile,
    RuntimeService, RuntimeSessionId,
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
fn event_bus_filters_sessions_and_reports_queue_stats() {
    let mut bus = RuntimeEventBus::with_capacity(4);

    bus.publish_event(
        RuntimeEventKind::ProgramFailed,
        None,
        Some(RuntimeSessionId(7)),
        RuntimeEventMetadata::new(RuntimeEventSeverity::Error).with_message("boom"),
    );
    bus.publish(
        RuntimeEventKind::ProgramCompleted,
        None,
        Some(RuntimeSessionId(8)),
    );

    let session_events = bus.events_for_session(RuntimeSessionId(7));
    let stats = bus.stats();

    assert_eq!(session_events.len(), 1);
    assert_eq!(
        session_events[0].metadata.severity,
        RuntimeEventSeverity::Error
    );
    assert_eq!(session_events[0].metadata.message.as_deref(), Some("boom"));
    assert_eq!(stats.len, 2);
    assert_eq!(stats.capacity, 4);
    assert_eq!(stats.dropped_events, 0);
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
fn runtime_policy_combines_profile_and_request_capabilities() {
    let policy = RuntimePolicy::new(RuntimeProfile::Server, [HostCapability::IoOutput]);

    assert!(policy.allows_capability(HostCapability::NativeCall));
    assert!(policy.allows_capability(HostCapability::IoOutput));
    assert!(
        policy
            .require_capability(HostCapability::FsWrite, "write file")
            .is_err()
    );
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
fn runtime_collects_stats_and_audit_when_requested() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "print(\"hello\");\nreturn len([1, 2, 3]);\n",
    );
    let mut request = RunSourceRequest::new(&file);
    request.collect_stats = true;
    request.collect_audit = true;

    let result = RuntimeService::new().run_source(request).unwrap();

    assert_eq!(result.value_display.as_deref(), Some("3"));
    assert!(result.stats.executed_instructions > 0);
    assert_eq!(result.stats.native_calls, 2);
    assert!(result.stats.allocations > 0);
    assert!(result.stats.max_register_count > 0);
    assert!(
        result
            .audit_events
            .iter()
            .any(|event| event.starts_with("program_started"))
    );
    assert!(
        result
            .audit_events
            .iter()
            .any(|event| event == "program_completed exit_code=0")
    );
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
fn safe_profile_runs_pure_bytecode_without_host_capabilities() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");
    let mut request = RunSourceRequest::new(&file);
    request.profile = RuntimeProfile::Safe;

    let result = RuntimeService::new().run_source(request).unwrap();

    assert_eq!(result.value_display.as_deref(), Some("42"));
}

#[test]
fn server_profile_denies_output_capability_by_default() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "print(\"hello\");\nreturn 42;\n");
    let mut request = RunSourceRequest::new(&file);
    request.profile = RuntimeProfile::Server;

    let error = RuntimeService::new().run_source(request).unwrap_err();

    assert_eq!(error.exit_code, 70);
    assert!(
        error
            .render()
            .contains("policy denied `io.output` for profile `server`")
    );
}

#[test]
fn request_capability_grants_allow_profile_restricted_output() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "print(\"hello\");\nreturn 42;\n");
    let mut request = RunSourceRequest::new(&file).with_capability(HostCapability::IoOutput);
    request.profile = RuntimeProfile::Server;

    let result = RuntimeService::new().run_source(request).unwrap();

    assert_eq!(result.output, "hello\n");
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

#[test]
fn runtime_rejects_unsupported_bytecode_container_features() {
    let dir = temp_dir();
    let source = write_file(&dir, "main.fx", "return 42;\n");
    let bytecode = dir.join("main.fxb");
    let runtime = RuntimeService::new();
    let compiled = runtime.compile_source_path(&source).unwrap();
    let mut metadata = BytecodeContainerMetadata::for_program(compiled.program.as_program());
    metadata.feature_flags |= FEATURE_CUSTOM_EXTENSIONS;
    fs::write(
        &bytecode,
        encode_container(compiled.program.as_program(), Some(metadata)).unwrap(),
    )
    .unwrap();

    let error = runtime
        .run_bytecode(RunBytecodeRequest::new(&bytecode))
        .unwrap_err();

    assert_eq!(error.exit_code, 65);
    assert!(
        error
            .render()
            .contains("unsupported bytecode feature `custom-extensions`")
    );
}

#[test]
fn runtime_rejects_container_required_capabilities() {
    let dir = temp_dir();
    let source = write_file(&dir, "main.fx", "return 42;\n");
    let bytecode = dir.join("main.fxb");
    let runtime = RuntimeService::new();
    let compiled = runtime.compile_source_path(&source).unwrap();
    let metadata = BytecodeContainerMetadata::for_program(compiled.program.as_program())
        .with_required_capability("fs.write");
    fs::write(
        &bytecode,
        encode_container(compiled.program.as_program(), Some(metadata)).unwrap(),
    )
    .unwrap();
    let mut request = RunBytecodeRequest::new(&bytecode);
    request.profile = RuntimeProfile::Safe;

    let error = runtime.run_bytecode(request).unwrap_err();

    assert_eq!(error.exit_code, 70);
    assert!(
        error
            .render()
            .contains("policy denied `fs.write` for profile `safe`")
    );
}

#[test]
fn runtime_inspects_bytecode_container_metadata_without_execution() {
    let dir = temp_dir();
    let source = write_file(&dir, "main.fx", "return 42;\n");
    let bytecode = dir.join("main.fxb");
    let runtime = RuntimeService::new();
    let compiled = runtime.compile_source_path(&source).unwrap();
    let metadata = BytecodeContainerMetadata::for_program(compiled.program.as_program())
        .with_module_name("demo");
    fs::write(
        &bytecode,
        encode_container(compiled.program.as_program(), Some(metadata)).unwrap(),
    )
    .unwrap();

    let inspected = runtime
        .inspect_bytecode(ferrix_runtime::InspectBytecodeRequest { path: bytecode })
        .unwrap();

    assert!(
        inspected
            .diagnostics
            .iter()
            .any(|line| line == "module=demo")
    );
    assert!(
        inspected
            .diagnostics
            .iter()
            .any(|line| line.starts_with("checksum="))
    );
}

#[test]
fn custom_extension_registry_checks_policy_and_dispatches_handler() {
    let mut registry = RuntimeExtensionRegistry::new();
    registry.register(CustomExtension::new(
        CustomExtensionMetadata {
            id: "math.double".to_string(),
            name: "Double".to_string(),
            arity: 1,
            output_register: Some(0),
            required_capabilities: Vec::new(),
            cost: ExtensionCostClass::Cheap,
            docs: "Doubles an integer.".to_string(),
        },
        |args: &[Value]| {
            let Value::Int(value) = args[0] else {
                return Ok(Value::Nil);
            };
            Ok(Value::Int(value * 2))
        },
    ));
    let policy = RuntimePolicy::new(RuntimeProfile::Trusted, []);

    let result = registry
        .call("math.double", &[Value::Int(21)], &policy)
        .unwrap();

    assert_eq!(result.value, Value::Int(42));
    assert_eq!(
        result.audit_event,
        "custom_extension_called id=math.double arity=1"
    );
}

#[test]
fn custom_extension_registry_reports_missing_and_denied_handlers() {
    let registry = RuntimeExtensionRegistry::new();
    let safe_policy = RuntimePolicy::new(RuntimeProfile::Safe, []);

    let missing = registry.call("missing", &[], &safe_policy).unwrap_err();

    assert!(
        missing
            .render()
            .contains("missing custom extension handler")
    );

    let mut registry = RuntimeExtensionRegistry::new();
    registry.register(CustomExtension::new(
        CustomExtensionMetadata {
            id: "host.echo".to_string(),
            name: "Echo".to_string(),
            arity: 0,
            output_register: None,
            required_capabilities: Vec::new(),
            cost: ExtensionCostClass::Normal,
            docs: "Returns nil.".to_string(),
        },
        |_args: &[Value]| Ok(Value::Nil),
    ));

    let denied = registry.call("host.echo", &[], &safe_policy).unwrap_err();

    assert!(
        denied
            .render()
            .contains("policy denied `extension.call` for profile `safe`")
    );
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
