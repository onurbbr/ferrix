//! Source diagnostics and source-position helpers.
//!
//! Compiler and runtime errors convert into `Diagnostic` values, and the CLI
//! renders them through `SourceManager`.

pub mod error;
pub mod source;
pub mod span;

pub use error::Diagnostic;
pub use source::{SourceFile, SourceLocation, SourceManager};
pub use span::{FileId, SourceSpan};
