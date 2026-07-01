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

#[test]
fn arithmetic_operand_type_mismatch_is_compile_error() {
    let err = compile_source_with_file_id("return 1 + true;\n", FileId(0)).unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::TypeMismatch {
            expected: "int".to_string(),
            found: "bool".to_string(),
        }
    );
}

#[test]
fn branch_condition_type_mismatch_is_compile_error() {
    let err =
        compile_source_with_file_id("if (1) { return 2; }\nreturn 3;\n", FileId(0)).unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::TypeMismatch {
            expected: "bool".to_string(),
            found: "int".to_string(),
        }
    );
}

#[test]
fn assignment_type_mismatch_is_compile_error() {
    let err =
        compile_source_with_file_id("let value = 1;\nvalue = false;\nreturn value;\n", FileId(0))
            .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::TypeMismatch {
            expected: "int".to_string(),
            found: "bool".to_string(),
        }
    );
}

#[test]
fn annotated_let_type_mismatch_is_compile_error() {
    let err = compile_source_with_file_id("let value: int = false;\nreturn value;\n", FileId(0))
        .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::TypeMismatch {
            expected: "int".to_string(),
            found: "bool".to_string(),
        }
    );
}

#[test]
fn annotated_function_argument_mismatch_is_compile_error() {
    let err = compile_source_with_file_id(
        "\
fn id(value: int): int {
    return value;
}

return id(false);
",
        FileId(0),
    )
    .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::TypeMismatch {
            expected: "int".to_string(),
            found: "bool".to_string(),
        }
    );
}

#[test]
fn annotated_function_return_mismatch_is_compile_error() {
    let err = compile_source_with_file_id(
        "\
fn bad(): int {
    return false;
}

return bad();
",
        FileId(0),
    )
    .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::TypeMismatch {
            expected: "int".to_string(),
            found: "bool".to_string(),
        }
    );
}

#[test]
fn unknown_type_annotation_is_compile_error() {
    let err = compile_source_with_file_id("let value: integer = 1;\nreturn value;\n", FileId(0))
        .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::UnknownType {
            name: "integer".to_string(),
        }
    );
}

#[test]
fn array_index_type_mismatch_is_compile_error() {
    let err =
        compile_source_with_file_id("let values = [1, 2];\nreturn values[false];\n", FileId(0))
            .unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::TypeMismatch {
            expected: "int".to_string(),
            found: "bool".to_string(),
        }
    );
}

#[test]
fn non_function_call_type_mismatch_is_compile_error() {
    let err =
        compile_source_with_file_id("let value = 1;\nreturn value();\n", FileId(0)).unwrap_err();

    assert_eq!(
        err.kind,
        CompileErrorKind::TypeMismatch {
            expected: "function".to_string(),
            found: "int".to_string(),
        }
    );
}
