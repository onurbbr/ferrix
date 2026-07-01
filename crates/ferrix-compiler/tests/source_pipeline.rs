//! Tests for source-to-diagnostic and source-to-runtime integration helpers.

use ferrix_compiler::{CompileError, CompileErrorKind, compile_source_with_file_id};
use ferrix_core::{
    Value,
    bytecode::{FunctionKind, VerifiedProgram},
    diagnostics::SourceManager,
};
use ferrix_vm::{Vm, VmError, VmErrorKind};

#[derive(Debug)]
enum SourceRunError {
    Compile(CompileError),
    Runtime {
        error: Box<VmError>,
        program: VerifiedProgram,
    },
}

fn compile_and_run(source: &str) -> Result<Value, SourceRunError> {
    let program = compile_source_with_file_id(source, ferrix_core::diagnostics::FileId(0))
        .map_err(SourceRunError::Compile)?;
    let result = Vm::new()
        .run_program(&program)
        .map_err(|error| SourceRunError::Runtime {
            error: Box::new(error),
            program: program.clone(),
        })?;

    Ok(result)
}

fn assert_runtime_error(source: &str, expected: VmErrorKind) -> (VmError, VerifiedProgram) {
    match compile_and_run(source).unwrap_err() {
        SourceRunError::Runtime { error, program } => {
            assert_eq!(error.kind, expected);
            (*error, program)
        }
        SourceRunError::Compile(error) => panic!("expected runtime error, got {error:?}"),
    }
}

fn assert_compile_error(source: &str, expected: CompileErrorKind) -> CompileError {
    match compile_and_run(source).unwrap_err() {
        SourceRunError::Compile(error) => {
            assert_eq!(error.kind, expected);
            error
        }
        SourceRunError::Runtime { error, .. } => panic!("expected compile error, got {error:?}"),
    }
}

#[test]
fn source_pipeline_runs_loop_and_branch_program() {
    let result = compile_and_run(
        "\
let i = 0;
let one = 1;
let total = 0;
while (i < 5) {
    total = total + i;
    i = i + one;
}
if (total == 10) {
    return total;
} else {
    return 0;
}
",
    )
    .unwrap();

    assert_eq!(result, Value::Int(10));
}

#[test]
fn compile_error_helper_checks_typed_kind_and_span() {
    let err = assert_compile_error(
        "return missing;\n",
        CompileErrorKind::UndefinedVariable {
            name: "missing".to_string(),
        },
    );

    assert_eq!(err.span.unwrap().start, 7);
    assert_eq!(err.span.unwrap().end, 14);
}

#[test]
fn compile_error_diagnostic_is_golden_tested() {
    let mut sources = SourceManager::new();
    let source = "return missing;\n";
    let file = sources.add_file("golden.fx", source);
    let err = compile_source_with_file_id(source, file).unwrap_err();

    assert_eq!(
        sources.render_diagnostic(&err.to_diagnostic()),
        "\
error: undefined variable `missing`
 --> golden.fx:1:8
  |
1 | return missing;
  |        ^^^^^^^
"
    );
}

#[test]
fn runtime_error_helper_checks_typed_kind() {
    let (err, _) = assert_runtime_error("return 1 / 0;\n", VmErrorKind::DivisionByZero);

    assert_eq!(err.instruction_ip, Some(2));
}

#[test]
fn runtime_error_diagnostic_uses_compiler_source_map() {
    let source = "return 1 / 0;\n";
    let mut sources = SourceManager::new();
    sources.add_file("runtime.fx", source);

    let (err, program) = assert_runtime_error(source, VmErrorKind::DivisionByZero);
    let function = &program.as_program().functions[program.as_program().entry.0 as usize];
    let FunctionKind::Bytecode(chunk) = &function.kind else {
        panic!("compiler should emit bytecode entrypoint");
    };

    assert_eq!(
        sources.render_diagnostic(&err.to_diagnostic_with_chunk(chunk)),
        "\
error: division by zero at instruction 2
 --> runtime.fx:1:8
  |
1 | return 1 / 0;
  |        ^^^^^
"
    );
}
