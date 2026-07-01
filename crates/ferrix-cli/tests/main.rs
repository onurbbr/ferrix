//! CLI smoke tests for run/debug/import/bytecode workflows.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use ferrix_core::bytecode::{bytecode_features, inspect_container};

#[test]
fn help_prints_usage() {
    let output = run(["--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("ferrix run <file|package>"));
    assert!(stdout(&output).contains("ferrix check <file|package>"));
    assert!(stderr(&output).is_empty());
}

#[test]
fn version_prints_package_version() {
    let output = run(["--version"]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        format!("ferrix {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(stderr(&output).is_empty());
}

#[test]
fn cli_creates_local_ferrix_layout_next_to_binary() {
    let output = run(["--version"]);
    assert_eq!(output.status.code(), Some(0));

    let binary_dir = Path::new(env!("CARGO_BIN_EXE_ferrix-cli"))
        .parent()
        .expect("cli binary should have a parent directory");
    let ferrix_home = binary_dir.join("ferrix");
    let config_path = ferrix_home.join("configs").join("config.toml");
    let runtime_home = ferrix_home.join("services").join("runtime");

    assert!(config_path.exists());
    assert!(runtime_home.is_dir());
    assert!(
        fs::read_to_string(config_path)
            .unwrap()
            .contains("home = \"services/runtime\"")
    );
}

#[test]
fn run_file_prints_non_nil_result() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_can_print_stats_and_audit() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "print(\"hello\");\nreturn len([1, 2, 3]);\n",
    );

    let output = run(["run", file.to_str().unwrap(), "--stats", "--audit"]);

    assert_eq!(output.status.code(), Some(0));
    let stdout = stdout(&output);
    assert!(stdout.contains("hello\n3\n"));
    assert!(stdout.contains("stats:\n"));
    assert!(stdout.contains("  executed_instructions: "));
    assert!(stdout.contains("  native_calls: 2"));
    assert!(stdout.contains("audit:\n"));
    assert!(stdout.contains("program_started"));
    assert!(stdout.contains("program_completed exit_code=0"));
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_requires_runtime_when_mode_is_required() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");
    let runtime_home = dir.join("runtime");

    let output = run([
        "--runtime-mode",
        "required",
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "run",
        file.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(69));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "Ferrix runtime is not running.\nStart it with: ferrix runtime start\n"
    );
}

#[test]
fn runtime_lifecycle_commands_are_silent_except_status() {
    let dir = temp_dir();
    let runtime_home = dir.join("runtime");

    let start = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "start",
    ]);
    assert_eq!(start.status.code(), Some(0));
    assert!(stdout(&start).is_empty());
    assert!(stderr(&start).is_empty());

    let status = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "status",
    ]);
    assert_eq!(status.status.code(), Some(0));
    assert!(stdout(&status).contains("runtime: serving"));
    assert!(stderr(&status).is_empty());

    let duplicate_start = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "start",
    ]);
    assert_eq!(duplicate_start.status.code(), Some(70));
    assert!(stdout(&duplicate_start).is_empty());
    assert_eq!(stderr(&duplicate_start), "Service is already running\n");

    let restart = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "restart",
    ]);
    assert_eq!(restart.status.code(), Some(0));
    assert!(stdout(&restart).is_empty());
    assert!(stderr(&restart).is_empty());

    let status = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "status",
    ]);
    assert_eq!(status.status.code(), Some(0));
    assert!(stdout(&status).contains("runtime: serving"));
    assert!(stderr(&status).is_empty());

    let stop = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "stop",
    ]);
    assert_eq!(stop.status.code(), Some(0));
    assert!(stdout(&stop).is_empty());
    assert!(stderr(&stop).is_empty());

    let duplicate_stop = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "stop",
    ]);
    assert_eq!(duplicate_stop.status.code(), Some(70));
    assert!(stdout(&duplicate_stop).is_empty());
    assert_eq!(stderr(&duplicate_stop), "Service is not running\n");
}

#[test]
fn runtime_serve_is_internal_only() {
    let output = run(["runtime", "serve"]);

    assert_eq!(output.status.code(), Some(64));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "error: runtime serve is an internal command\n"
    );

    let output = run(["runtime", "serve", "--internal"]);

    assert_eq!(output.status.code(), Some(64));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "error: runtime serve is an internal command\n"
    );

    let parent_pid = std::process::id().to_string();
    let output = run([
        "--runtime-mode",
        "managed",
        "runtime",
        "serve",
        "--internal",
        &parent_pid,
    ]);

    assert_eq!(output.status.code(), Some(64));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "error: runtime serve is an internal command\n"
    );
}

#[test]
fn required_runtime_mode_runs_through_started_daemon_and_records_process() {
    let dir = temp_dir();
    let runtime_home = dir.join("runtime");
    let file = write_file(
        &dir,
        "main.fx",
        "\
print(\"hello\");
return 42;
",
    );

    let start = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "start",
    ]);
    assert_eq!(start.status.code(), Some(0));

    let output = run([
        "--runtime-mode",
        "required",
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "run",
        file.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "hello\n42\n");
    assert!(stderr(&output).is_empty());

    let ps = run(["--runtime-home", runtime_home.to_str().unwrap(), "ps"]);
    assert_eq!(ps.status.code(), Some(0));
    assert!(stdout(&ps).contains("pid\tsession\tstatus\tkind\tpath"));
    assert!(!stdout(&ps).contains("completed\trun"));
    assert!(stderr(&ps).is_empty());

    let logs = run(["--runtime-home", runtime_home.to_str().unwrap(), "logs"]);
    assert_eq!(logs.status.code(), Some(0));
    assert!(stdout(&logs).contains("pid\tkind\tstatus\texit\tpath"));
    assert!(stdout(&logs).contains("1\trun\tcompleted\t0\t"));
    assert!(stdout(&logs).contains(file.to_str().unwrap()));
    assert!(stderr(&logs).is_empty());

    let info = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "info",
        "1",
    ]);
    assert_eq!(info.status.code(), Some(0));
    assert!(stdout(&info).contains("kind: run"));
    assert!(stdout(&info).contains("output:\nhello\n42\n"));
    assert!(stderr(&info).is_empty());

    let stop = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "stop",
    ]);
    assert_eq!(stop.status.code(), Some(0));
}

#[test]
fn managed_runtime_mode_starts_daemon_automatically() {
    let dir = temp_dir();
    let runtime_home = dir.join("runtime");
    let file = write_file(&dir, "main.fx", "return 42;\n");

    let output = run([
        "--runtime-mode",
        "managed",
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "run",
        file.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());

    let status = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "status",
    ]);
    assert_eq!(status.status.code(), Some(0));
    assert!(stdout(&status).contains("runtime: serving"));
    assert!(stdout(&status).contains("completed: 1"));

    let stop = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "stop",
    ]);
    assert_eq!(stop.status.code(), Some(0));
}

#[test]
fn check_file_does_not_require_runtime_service_state() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "print(42);\nreturn 99;\n");
    let runtime_home = dir.join("runtime");

    let output = run([
        "--runtime-mode",
        "required",
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "check",
        file.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
}

#[test]
fn invalid_runtime_mode_is_reported_for_execution_commands() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 42;\n");

    let output = run(["--runtime-mode", "daemon", "run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(64));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output)
            .contains("invalid runtime mode `daemon`; expected embedded, required, or managed")
    );
}

#[test]
fn compile_and_run_bytecode_file() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");
    let bytecode = dir.join("main.fxb");

    let compile = run([
        "compile",
        file.to_str().unwrap(),
        bytecode.to_str().unwrap(),
    ]);
    assert_eq!(compile.status.code(), Some(0));
    assert!(stdout(&compile).is_empty());
    assert!(stderr(&compile).is_empty());

    let output = run(["run-bytecode", bytecode.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn logs_lists_recorded_cli_file_operations_by_kind() {
    let dir = temp_dir();
    let runtime_home = dir.join("runtime");
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");
    let bytecode = dir.join("main.fxb");

    let check = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "check",
        file.to_str().unwrap(),
    ]);
    assert_eq!(check.status.code(), Some(0));

    let compile = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "compile",
        file.to_str().unwrap(),
        bytecode.to_str().unwrap(),
    ]);
    assert_eq!(compile.status.code(), Some(0));

    let run_bytecode = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "run-bytecode",
        bytecode.to_str().unwrap(),
    ]);
    assert_eq!(run_bytecode.status.code(), Some(0));

    let debug = run_with_input(
        [
            "--runtime-home",
            runtime_home.to_str().unwrap(),
            "debug",
            file.to_str().unwrap(),
        ],
        "quit\n",
    );
    assert_eq!(debug.status.code(), Some(0));

    let start = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "start",
    ]);
    assert_eq!(start.status.code(), Some(0));

    let logs = run(["--runtime-home", runtime_home.to_str().unwrap(), "logs"]);
    assert_eq!(logs.status.code(), Some(0));
    let logs = stdout(&logs);
    assert!(logs.contains("pid\tkind\tstatus\texit\tpath"));
    assert!(logs.contains("1\tcheck\tcompleted\t0\t"));
    assert!(logs.contains("2\tcompile\tcompleted\t0\t"));
    assert!(logs.contains("3\trun-bytecode\tcompleted\t0\t"));
    assert!(logs.contains("4\tdebug\tcompleted\t0\t"));
    assert!(logs.contains(file.to_str().unwrap()));
    assert!(logs.contains(bytecode.to_str().unwrap()));

    let stop = run([
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "runtime",
        "stop",
    ]);
    assert_eq!(stop.status.code(), Some(0));
}

#[test]
fn logs_with_pid_points_to_info_command() {
    let output = run(["logs", "1"]);

    assert_eq!(output.status.code(), Some(64));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("logs does not accept a process id"));
    assert!(stderr(&output).contains("ferrix info <pid>"));
}

#[test]
fn logs_require_running_runtime_when_mode_is_required() {
    let dir = temp_dir();
    let runtime_home = dir.join("runtime");

    let output = run([
        "--runtime-mode",
        "required",
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "logs",
    ]);

    assert_eq!(output.status.code(), Some(69));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "Ferrix runtime is not running.\nStart it with: ferrix runtime start\n"
    );
}

#[test]
fn logs_require_running_runtime_by_default() {
    let dir = temp_dir();
    let runtime_home = dir.join("runtime");

    let output = run(["--runtime-home", runtime_home.to_str().unwrap(), "logs"]);

    assert_eq!(output.status.code(), Some(69));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "Ferrix runtime is not running.\nStart it with: ferrix runtime start\n"
    );
}

#[test]
fn run_file_prints_string_result() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return \"ferrix\";\n");

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "ferrix\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_prints_function_call_result() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "\
fn add(a, b) {
    return a + b;
}
return add(40, 2);
",
    );

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_supports_static_function_imports() {
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
return add(40, 2);
",
    );

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_supports_namespaced_static_function_imports() {
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

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_supports_explicit_module_exports() {
    let dir = temp_dir();
    write_file(
        &dir,
        "math.fx",
        "\
fn hidden() {
    return 40;
}

export fn answer() {
    return hidden();
}

export let offset = 2;
",
    );
    let file = write_file(
        &dir,
        "main.fx",
        "\
import math;
return math.answer() + math.offset;
",
    );

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_reports_private_module_exports() {
    let dir = temp_dir();
    write_file(
        &dir,
        "math.fx",
        "\
fn hidden() {
    return 42;
}

export fn visible() {
    return hidden();
}
",
    );
    let file = write_file(
        &dir,
        "main.fx",
        "\
import math;
return math.hidden();
",
    );

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(65));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("undefined export `hidden` in module `math`"));
}

#[test]
fn run_file_supports_transitive_static_imports() {
    let dir = temp_dir();
    write_file(
        &dir,
        "base.fx",
        "\
fn one() {
    return 1;
}
",
    );
    write_file(
        &dir,
        "math.fx",
        "\
import base;
fn add_one(value) {
    return value + one();
}
",
    );
    let file = write_file(
        &dir,
        "main.fx",
        "\
import math;
return add_one(41);
",
    );

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_reports_missing_static_imports() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "import missing;\nreturn 1;\n");
    let missing = dir.join("missing.fx");

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(66));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains(&format!(
        "could not resolve import `missing` from `{}` as `{}`",
        file.display(),
        missing.display()
    )));
}

#[test]
fn run_package_uses_manifest_entry_and_module_roots() {
    let dir = temp_dir();
    write_file(
        &dir,
        "Ferrix.toml",
        "\
name = \"demo\"
entry = \"src/main.fx\"
module_roots = [\"src\"]
dependencies = [\"stdlib@0.1\"]
",
    );
    write_file(
        &dir,
        "src/math.fx",
        "\
export fn answer() {
    return 42;
}
",
    );
    write_file(
        &dir,
        "src/main.fx",
        "\
import math;
return math.answer();
",
    );

    let output = run(["run", dir.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_package_resolves_nested_modules_from_manifest_roots() {
    let dir = temp_dir();
    write_file(
        &dir,
        "Ferrix.toml",
        "\
name = \"nested\"
entry = \"src/main.fx\"
module_roots = [\"modules\", \"src\"]
",
    );
    write_file(
        &dir,
        "modules/util/math.fx",
        "\
export fn answer() {
    return 42;
}
",
    );
    write_file(
        &dir,
        "src/main.fx",
        "\
import util.math;
return answer();
",
    );

    let output = run(["run", dir.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn check_package_entrypoint_compiles_without_running() {
    let dir = temp_dir();
    write_file(
        &dir,
        "Ferrix.toml",
        "\
name = \"checkable\"
entry = \"src/main.fx\"
module_roots = [\"src\"]
",
    );
    write_file(
        &dir,
        "src/main.fx",
        "\
print(42);
return 99;
",
    );

    let output = run(["check", dir.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).is_empty());
}

#[test]
fn compile_package_entrypoint_writes_bytecode() {
    let dir = temp_dir();
    write_file(
        &dir,
        "Ferrix.toml",
        "\
name = \"compiled\"
entry = \"src/main.fx\"
module_roots = [\"src\"]
",
    );
    write_file(&dir, "src/main.fx", "return 40 + 2;\n");
    let bytecode = dir.join("main.fxb");

    let compile = run(["compile", dir.to_str().unwrap(), bytecode.to_str().unwrap()]);
    assert_eq!(compile.status.code(), Some(0));
    assert!(stdout(&compile).is_empty());
    assert!(stderr(&compile).is_empty());

    let output = run(["run-bytecode", bytecode.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn compile_can_explain_optimizations_and_write_feature_metadata() {
    let dir = temp_dir();
    let source = write_file(
        &dir,
        "main.fx",
        "\
print(\"hello\");
return len([1, 2, 3]);
",
    );
    let bytecode = dir.join("main.fxb");

    let compile = run([
        "compile",
        source.to_str().unwrap(),
        bytecode.to_str().unwrap(),
        "--explain-optimizations",
    ]);

    assert_eq!(compile.status.code(), Some(0));
    let stdout = stdout(&compile);
    assert!(stdout.contains("features: "));
    assert!(stdout.contains("arrays"));
    assert!(stdout.contains("native-calls"));
    assert!(stdout.contains("capabilities: io.output,native.call"));
    assert!(stdout.contains("optimizer: chunks="));
    assert!(stderr(&compile).is_empty());

    let metadata = inspect_container(&fs::read(bytecode).unwrap()).unwrap();
    let features = bytecode_features(metadata.feature_flags)
        .into_iter()
        .map(|feature| feature.as_str().to_string())
        .collect::<Vec<_>>();
    assert!(features.contains(&"arrays".to_string()));
    assert!(features.contains(&"native-calls".to_string()));
    assert_eq!(
        metadata.required_capabilities,
        vec!["io.output".to_string(), "native.call".to_string()]
    );
}

#[test]
fn run_package_reports_missing_modules_with_search_roots() {
    let dir = temp_dir();
    write_file(
        &dir,
        "Ferrix.toml",
        "\
name = \"missing-demo\"
entry = \"src/main.fx\"
module_roots = [\"modules\"]
",
    );
    let file = write_file(
        &dir,
        "src/main.fx",
        "\
import local;
return 1;
",
    );
    write_file(&dir, "src/local.fx", "export fn value() { return 42; }\n");
    let searched = dir.join("modules/local.fx");

    let output = run(["run", dir.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(66));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains(&format!(
        "could not resolve package import `local` from `{}` in package `missing-demo`",
        file.display()
    )));
    assert!(stderr(&output).contains(&format!("`{}`", searched.display())));
}

#[test]
fn debug_file_steps_and_prints_registers() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "let x = 40 + 2;\nreturn x;\n");

    let output = run_with_input(
        ["debug", file.to_str().unwrap()],
        "registers\nstep\ncontinue\n",
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    assert!(stdout(&output).contains("stopped at fn#0 main ip=0"));
    assert!(stdout(&output).contains("r0 = nil"));
    assert!(stdout(&output).contains("debug: finished with 42"));
}

#[test]
fn debug_file_requires_runtime_when_mode_is_required() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 42;\n");
    let runtime_home = dir.join("runtime");

    let output = run([
        "--runtime-mode",
        "required",
        "--runtime-home",
        runtime_home.to_str().unwrap(),
        "debug",
        file.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(69));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        "Ferrix runtime is not running.\nStart it with: ferrix runtime start\n"
    );
}

#[test]
fn debug_file_supports_instruction_breakpoints() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "let x = 1;\nlet y = 41;\nreturn x + y;\n");

    let output = run_with_input(
        ["debug", file.to_str().unwrap()],
        "break 2\ncontinue\nstep\ncontinue\n",
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    assert!(stdout(&output).contains("set breakpoint at ip=2"));
    assert!(stdout(&output).contains("stopped at fn#0 main ip=2"));
    assert!(stdout(&output).contains("debug: finished with 42"));
}

#[test]
fn debug_file_supports_source_line_breakpoints() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "let x = 1;\nlet y = 41;\nreturn x + y;\n");

    let output = run_with_input(
        ["debug", file.to_str().unwrap()],
        "break line 2\ncontinue\ncontinue\n",
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    assert!(stdout(&output).contains("set breakpoint at line 2"));
    assert!(stdout(&output).contains(&format!(" --> {}:2:", file.display())));
    assert!(stdout(&output).contains("  | let y = 41;"));
    assert!(stdout(&output).contains("debug: finished with 42"));
}

#[test]
fn debug_file_supports_watches_and_disassembly() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "let x = 40 + 2;\nreturn x;\n");

    let output = run_with_input(
        ["debug", file.to_str().unwrap()],
        "watch r1\ndisasm 1\nstep\ncontinue\n",
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    assert!(stdout(&output).contains("watch #0: r1 = nil"));
    assert!(stdout(&output).contains("disassembly for frame #0 main"));
    assert!(stdout(&output).contains("=> 0000"));
    assert!(stdout(&output).contains("watch #0: r1 = 40"));
    assert!(stdout(&output).contains("debug: finished with 42"));
}

#[test]
fn debug_file_supports_frame_selection() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "\
fn add(a, b) {
    return a + b;
}
return add(40, 2);
",
    );

    let output = run_with_input(
        ["debug", file.to_str().unwrap()],
        "break add:0\ncontinue\nstack\nframe 1\nregisters\ncontinue\n",
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(stderr(&output).is_empty());
    assert!(stdout(&output).contains("set breakpoint at fn#0:0"));
    assert!(stdout(&output).contains("#0 * at add (fn#0, ip=0)"));
    assert!(stdout(&output).contains("#1 at main (fn#1, ip="));
    assert!(stdout(&output).contains("selected frame #1 at main (fn#1, ip="));
    assert!(stdout(&output).contains("registers for frame #1 main (fn#1, ip="));
    assert!(stdout(&output).contains("debug: finished with 42"));
}

#[test]
fn run_file_prints_array_results_and_supports_len() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "\
let values = [1, 2, 3];
values[1] = len(values);
return values;
",
    );

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "[1, 3, 3]\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_prints_map_results_and_supports_indexing() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "\
let user = { \"name\": \"Onur\" };
user[\"age\"] = 30;
return user;
",
    );

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "{name: Onur, age: 30}\n");
    assert!(stderr(&output).is_empty());
}

#[test]
fn run_file_renders_compile_diagnostics() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return missing;\n");

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(65));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        format!(
            "\
error: undefined variable `missing`
 --> {}:1:8
  |
1 | return missing;
  |        ^^^^^^^
",
            file.display()
        )
    );
}

#[test]
fn run_file_renders_runtime_diagnostics() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 1 / 0;\n");

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(70));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        format!(
            "\
error: division by zero at instruction 2
 --> {}:1:8
  |
1 | return 1 / 0;
  |        ^^^^^
",
            file.display()
        )
    );
}

#[test]
fn run_file_renders_runtime_stack_trace() {
    let dir = temp_dir();
    let file = write_file(
        &dir,
        "main.fx",
        "\
fn fail() {
    return 1 / 0;
}
return fail();
",
    );

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(70));
    assert!(stdout(&output).is_empty());
    assert_eq!(
        stderr(&output),
        format!(
            "\
error: division by zero at instruction 2
 --> {}:2:12
  |
2 |     return 1 / 0;
  |            ^^^^^
stack trace:
  at fail (fn#0, instruction 2)
  at main (fn#1, instruction 0)
",
            file.display()
        )
    );
}

#[test]
fn missing_file_is_reported() {
    let output = run(["run", "missing.fx"]);

    assert_eq!(output.status.code(), Some(66));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("could not read `missing.fx`"));
}

fn run<const N: usize>(args: [&str; N]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ferrix-cli"))
        .args(args)
        .output()
        .expect("failed to run ferrix-cli")
}

fn run_with_input<const N: usize>(args: [&str; N], input: &str) -> std::process::Output {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_ferrix-cli"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run ferrix-cli");

    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(input.as_bytes())
        .expect("failed to write debugger input");

    child
        .wait_with_output()
        .expect("failed to wait for ferrix-cli")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn temp_dir() -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ferrix-cli-test-{unique}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, source).unwrap();
    path
}
