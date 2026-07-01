//! Source storage and byte-offset to line/column mapping.

use crate::diagnostics::{FileId, SourceSpan};

/// Source file registered with `SourceManager`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    /// Stable file id.
    pub id: FileId,
    /// Display name, usually a path.
    pub name: String,
    /// Full source text.
    pub source: String,
    line_starts: Vec<usize>,
}

/// One-based source location derived from a byte offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    /// Source file containing this location.
    pub file_id: FileId,
    /// One-based line number.
    pub line: usize,
    /// One-based column number.
    pub column: usize,
}

/// Stores source files and renders diagnostics against them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceManager {
    files: Vec<SourceFile>,
}

impl SourceManager {
    /// Creates an empty source manager.
    pub fn new() -> Self {
        Self { files: Vec::new() }
    }

    /// Adds a file and returns its assigned `FileId`.
    pub fn add_file(&mut self, name: impl Into<String>, source: impl Into<String>) -> FileId {
        let id = FileId(self.files.len() as u32);
        let source = source.into();
        self.files.push(SourceFile {
            id,
            name: name.into(),
            line_starts: line_starts(&source),
            source,
        });
        id
    }

    /// Looks up a source file by id.
    pub fn file(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    /// Maps a span start to a one-based source location.
    pub fn location(&self, span: SourceSpan) -> Option<SourceLocation> {
        let file = self.file(span.file_id)?;
        file.location(span.start)
    }

    /// Returns line text without trailing newline characters.
    pub fn line_text(&self, file_id: FileId, line: usize) -> Option<&str> {
        self.file(file_id)?.line_text(line)
    }

    /// Renders a diagnostic using the stored files.
    pub fn render_diagnostic(&self, diagnostic: &crate::diagnostics::Diagnostic) -> String {
        diagnostic.render(self)
    }
}

impl SourceFile {
    /// Maps a byte offset to a one-based source location.
    pub fn location(&self, offset: usize) -> Option<SourceLocation> {
        if offset > self.source.len() {
            return None;
        }

        let line_index = match self.line_starts.binary_search(&offset) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        };
        let line_start = *self.line_starts.get(line_index)?;

        Some(SourceLocation {
            file_id: self.id,
            line: line_index + 1,
            column: offset.saturating_sub(line_start) + 1,
        })
    }

    /// Returns one-based line text without trailing newlines.
    pub fn line_text(&self, line: usize) -> Option<&str> {
        let line_index = line.checked_sub(1)?;
        let start = *self.line_starts.get(line_index)?;
        let end = self
            .line_starts
            .get(line_index + 1)
            .copied()
            .unwrap_or(self.source.len());
        let line = self.source[start..end].trim_end_matches(['\r', '\n']);

        Some(line)
    }
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' && index + 1 < source.len() {
            starts.push(index + 1);
        }
    }
    starts
}
