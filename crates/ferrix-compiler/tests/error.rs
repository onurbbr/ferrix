//! Tests for typed compiler errors and their diagnostic conversion.

use ferrix_compiler::{CompileError, CompileErrorKind, compile_source_with_file_id};
use ferrix_core::diagnostics::{FileId, SourceManager, SourceSpan};

#[test]
fn compile_error_converts_to_user_diagnostic() {
    let mut sources = SourceManager::new();
    let file = sources.add_file("main.fx", "return missing;\n");

    let err = compile_source_with_file_id("return missing;\n", file).unwrap_err();
    let diagnostic = err.to_diagnostic();

    assert_eq!(
        err.kind,
        CompileErrorKind::UndefinedVariable {
            name: "missing".to_string(),
        }
    );
    assert_eq!(
        sources.render_diagnostic(&diagnostic),
        "\
error: undefined variable `missing`
 --> main.fx:1:8
  |
1 | return missing;
  |        ^^^^^^^
"
    );
}

#[test]
fn compile_error_keeps_optional_source_span() {
    let span = SourceSpan::new(FileId(3), 4, 9);
    let err = CompileError::new(
        CompileErrorKind::UndefinedVariable {
            name: "value".to_string(),
        },
        Some(span),
    );

    assert_eq!(err.to_diagnostic().span, Some(span));
}

#[test]
fn unterminated_string_is_compile_error() {
    let err = compile_source_with_file_id("return \"open;\n", FileId(0)).unwrap_err();

    assert_eq!(err.kind, CompileErrorKind::UnterminatedString);
}

#[test]
fn invalid_string_escape_is_compile_error() {
    let err = compile_source_with_file_id("return \"bad\\q\";\n", FileId(0)).unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::InvalidStringEscape { escape: 'q' }
    );
}

#[test]
fn duplicate_function_is_compile_error() {
    let err = compile_source_with_file_id(
        "\
fn value() { return 1; }
fn value() { return 2; }
return value();
",
        FileId(0),
    )
    .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::DuplicateFunction {
            name: "value".to_string(),
        }
    );
}

#[test]
fn duplicate_builtin_function_is_compile_error() {
    let err = compile_source_with_file_id(
        "fn len(value) { return value; }\nreturn len(1);\n",
        FileId(0),
    )
    .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::DuplicateFunction {
            name: "len".to_string(),
        }
    );
}

#[test]
fn duplicate_parameter_is_compile_error() {
    let err = compile_source_with_file_id(
        "fn same(x, x) { return x; }\nreturn same(1, 2);\n",
        FileId(0),
    )
    .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::DuplicateParameter {
            name: "x".to_string(),
        }
    );
}

#[test]
fn undefined_function_is_compile_error() {
    let err = compile_source_with_file_id("return missing();\n", FileId(0)).unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::UndefinedFunction {
            name: "missing".to_string(),
        }
    );
}

#[test]
fn wrong_function_arity_is_compile_error() {
    let err = compile_source_with_file_id("fn id(x) { return x; }\nreturn id(1, 2);\n", FileId(0))
        .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::WrongCallArity {
            name: "id".to_string(),
            expected: 1,
            actual: 2,
        }
    );
}
