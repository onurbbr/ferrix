//! User-facing diagnostic rendering.

use std::fmt::Write;

use crate::diagnostics::{SourceManager, SourceSpan};

/// Renderable diagnostic with a primary message, optional source span, and notes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Main error message.
    pub message: String,
    /// Optional source location for caret rendering.
    pub span: Option<SourceSpan>,
    /// Extra lines printed after the primary diagnostic.
    pub notes: Vec<String>,
}

impl Diagnostic {
    /// Creates a diagnostic with a message and optional source span.
    pub fn new(message: impl Into<String>, span: Option<SourceSpan>) -> Self {
        Self {
            message: message.into(),
            span,
            notes: Vec::new(),
        }
    }

    /// Attaches diagnostic notes such as runtime stack traces.
    pub fn with_notes(mut self, notes: Vec<String>) -> Self {
        self.notes = notes;
        self
    }

    /// Renders the diagnostic using source text from a `SourceManager`.
    pub fn render(&self, sources: &SourceManager) -> String {
        let mut output = String::new();
        writeln!(&mut output, "error: {}", self.message).expect("writing to String cannot fail");

        let Some(span) = self.span else {
            render_notes(&mut output, &self.notes);
            return output;
        };
        let Some(file) = sources.file(span.file_id) else {
            render_notes(&mut output, &self.notes);
            return output;
        };
        let Some(location) = file.location(span.start) else {
            render_notes(&mut output, &self.notes);
            return output;
        };
        let Some(line_text) = file.line_text(location.line) else {
            render_notes(&mut output, &self.notes);
            return output;
        };

        writeln!(
            &mut output,
            " --> {}:{}:{}",
            file.name, location.line, location.column
        )
        .expect("writing to String cannot fail");
        writeln!(&mut output, "  |").expect("writing to String cannot fail");
        writeln!(&mut output, "{} | {}", location.line, line_text)
            .expect("writing to String cannot fail");
        writeln!(
            &mut output,
            "  | {}{}",
            " ".repeat(location.column.saturating_sub(1)),
            "^".repeat(caret_width(span, line_text, location.column))
        )
        .expect("writing to String cannot fail");

        render_notes(&mut output, &self.notes);
        output
    }
}

fn render_notes(output: &mut String, notes: &[String]) {
    for note in notes {
        writeln!(output, "{note}").expect("writing to String cannot fail");
    }
}

fn caret_width(span: SourceSpan, line_text: &str, column: usize) -> usize {
    if span.is_empty() {
        return 1;
    }

    let line_remaining = line_text.len().saturating_sub(column.saturating_sub(1));
    span.len().min(line_remaining).max(1)
}
