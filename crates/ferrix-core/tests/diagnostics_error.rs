//! Tests for diagnostic rendering with source snippets and notes.

use ferrix_core::diagnostics::{Diagnostic, SourceManager, SourceSpan};

#[test]
fn renders_diagnostic_with_source_caret() {
    let mut sources = SourceManager::new();
    let file = sources.add_file("main.fx", "let x = 1;\nreturn missing;\n");
    let diagnostic = Diagnostic::new(
        "undefined variable `missing`",
        Some(SourceSpan::new(file, 18, 25)),
    );

    let rendered = diagnostic.render(&sources);

    assert_eq!(
        rendered,
        "\
error: undefined variable `missing`
 --> main.fx:2:8
  |
2 | return missing;
  |        ^^^^^^^
"
    );
}

#[test]
fn renders_message_without_source_when_span_is_missing() {
    let diagnostic = Diagnostic::new("program ended without return", None);

    assert_eq!(
        diagnostic.render(&SourceManager::new()),
        "error: program ended without return\n"
    );
}

#[test]
fn renders_notes_after_primary_diagnostic() {
    let diagnostic = Diagnostic::new("division by zero", None).with_notes(vec![
        "stack trace:".to_string(),
        "  at fail (fn#0, instruction 2)".to_string(),
    ]);

    assert_eq!(
        diagnostic.render(&SourceManager::new()),
        "\
error: division by zero
stack trace:
  at fail (fn#0, instruction 2)
"
    );
}
