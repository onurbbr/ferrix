//! Tests for source-file indexing and location mapping.

use ferrix_core::diagnostics::SourceManager;

#[test]
fn maps_offsets_to_one_based_locations() {
    let mut sources = SourceManager::new();
    let file = sources.add_file("main.fx", "let x = 1;\nreturn x;\n");

    assert_eq!(sources.file(file).unwrap().location(0).unwrap().line, 1);
    assert_eq!(sources.file(file).unwrap().location(11).unwrap().line, 2);
    assert_eq!(sources.file(file).unwrap().location(18).unwrap().column, 8);
}

#[test]
fn returns_line_text_without_newline() {
    let mut sources = SourceManager::new();
    let file = sources.add_file("main.fx", "let x = 1;\nreturn x;\n");

    assert_eq!(sources.line_text(file, 2), Some("return x;"));
}
