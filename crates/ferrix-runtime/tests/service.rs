//! Runtime service request/result tests.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ferrix_core::bytecode::encode_program;
use ferrix_runtime::{
    RunBytecodeRequest, RunSourceRequest, RuntimeGateway, RuntimeMode, RuntimeService,
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

    let error = RuntimeGateway::new(RuntimeMode::Required)
        .run_source(RunSourceRequest::new(&file))
        .unwrap_err();

    assert_eq!(error.exit_code, 69);
    assert_eq!(
        error.render(),
        "Ferrix runtime is not running.\nStart it with: ferrix runtime start\n"
    );
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
