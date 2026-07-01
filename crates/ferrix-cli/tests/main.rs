//! CLI smoke tests for run/debug/import/bytecode workflows.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

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
fn run_file_prints_non_nil_result() {
    let dir = temp_dir();
    let file = write_file(&dir, "main.fx", "return 40 + 2;\n");

    let output = run(["run", file.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "42\n");
    assert!(stderr(&output).is_empty());
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
