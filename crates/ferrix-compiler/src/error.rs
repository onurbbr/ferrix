//! Compiler error model shared by lexer, parser, semantic analysis, and codegen.

use std::{error::Error, fmt};

use ferrix_core::{
    bytecode::VerificationError,
    diagnostics::{Diagnostic, SourceSpan},
};

/// Source-level compile error with optional span information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    /// Structured error category.
    pub kind: CompileErrorKind,
    /// Source span that should be highlighted, when known.
    pub span: Option<SourceSpan>,
}

/// Specific compiler failure category.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileErrorKind {
    UnexpectedCharacter {
        character: char,
    },
    InvalidInteger {
        literal: String,
    },
    UnterminatedString,
    InvalidStringEscape {
        escape: char,
    },
    UnexpectedToken {
        expected: String,
        found: String,
    },
    ExpectedExpression,
    UndefinedVariable {
        name: String,
    },
    UndefinedFunction {
        name: String,
    },
    UndefinedModuleExport {
        module: String,
        name: String,
    },
    UndefinedMethod {
        name: String,
    },
    DuplicateVariable {
        name: String,
    },
    DuplicateFunction {
        name: String,
    },
    DuplicateParameter {
        name: String,
    },
    WrongCallArity {
        name: String,
        expected: usize,
        actual: usize,
    },
    TypeMismatch {
        expected: String,
        found: String,
    },
    UnknownType {
        name: String,
    },
    TooManyParameters {
        max: usize,
    },
    TooManyArguments {
        max: usize,
    },
    TooManyArrayElements {
        max: usize,
    },
    TooManyMapEntries {
        max: usize,
    },
    TooManyRecordFields {
        max: usize,
    },
    TooManyRegisters,
    TooManyInstructions,
    TooManyConstants {
        max: usize,
    },
    TooManyStrings {
        max: usize,
    },
    TooManyFunctions {
        max: usize,
    },
    BytecodeVerification(VerificationError),
}

impl CompileError {
    /// Creates a compiler error with an optional source span.
    pub fn new(kind: CompileErrorKind, span: Option<SourceSpan>) -> Self {
        Self { kind, span }
    }

    /// Converts the compile error into a renderable diagnostic.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::new(self.to_string(), self.span)
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            CompileErrorKind::UnexpectedCharacter { character } => {
                write!(f, "unexpected character `{character}`")
            }
            CompileErrorKind::InvalidInteger { literal } => {
                write!(f, "invalid integer literal `{literal}`")
            }
            CompileErrorKind::UnterminatedString => f.write_str("unterminated string literal"),
            CompileErrorKind::InvalidStringEscape { escape } => {
                write!(f, "invalid string escape `\\{escape}`")
            }
            CompileErrorKind::UnexpectedToken { expected, found } => {
                write!(f, "expected {expected}, found {found}")
            }
            CompileErrorKind::ExpectedExpression => f.write_str("expected expression"),
            CompileErrorKind::UndefinedVariable { name } => {
                write!(f, "undefined variable `{name}`")
            }
            CompileErrorKind::UndefinedFunction { name } => {
                write!(f, "undefined function `{name}`")
            }
            CompileErrorKind::UndefinedModuleExport { module, name } => {
                write!(f, "undefined export `{name}` in module `{module}`")
            }
            CompileErrorKind::UndefinedMethod { name } => {
                write!(f, "undefined method `{name}`")
            }
            CompileErrorKind::DuplicateVariable { name } => {
                write!(f, "duplicate variable `{name}`")
            }
            CompileErrorKind::DuplicateFunction { name } => {
                write!(f, "duplicate function `{name}`")
            }
            CompileErrorKind::DuplicateParameter { name } => {
                write!(f, "duplicate parameter `{name}`")
            }
            CompileErrorKind::WrongCallArity {
                name,
                expected,
                actual,
            } => write!(
                f,
                "wrong argument count for `{name}`; expected {expected}, got {actual}"
            ),
            CompileErrorKind::TypeMismatch { expected, found } => {
                write!(f, "type mismatch; expected {expected}, got {found}")
            }
            CompileErrorKind::UnknownType { name } => write!(f, "unknown type `{name}`"),
            CompileErrorKind::TooManyParameters { max } => {
                write!(f, "too many function parameters; maximum is {max}")
            }
            CompileErrorKind::TooManyArguments { max } => {
                write!(f, "too many call arguments; maximum is {max}")
            }
            CompileErrorKind::TooManyArrayElements { max } => {
                write!(f, "too many array elements; maximum is {max}")
            }
            CompileErrorKind::TooManyMapEntries { max } => {
                write!(f, "too many map entries; maximum is {max}")
            }
            CompileErrorKind::TooManyRecordFields { max } => {
                write!(f, "too many record fields; maximum is {max}")
            }
            CompileErrorKind::TooManyRegisters => f.write_str("too many registers in function"),
            CompileErrorKind::TooManyInstructions => {
                f.write_str("too many instructions in function")
            }
            CompileErrorKind::TooManyConstants { max } => {
                write!(f, "too many constants; maximum is {max}")
            }
            CompileErrorKind::TooManyStrings { max } => {
                write!(f, "too many strings; maximum is {max}")
            }
            CompileErrorKind::TooManyFunctions { max } => {
                write!(f, "too many functions; maximum is {max}")
            }
            CompileErrorKind::BytecodeVerification(error) => write!(f, "{error}"),
        }
    }
}

impl Error for CompileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            CompileErrorKind::BytecodeVerification(error) => Some(error),
            _ => None,
        }
    }
}
