//! Ferrix source compiler.
//!
//! This crate turns source text into a verified bytecode program by lexing,
//! parsing, performing semantic checks, and emitting register bytecode.

pub mod ast;
pub mod codegen;
pub mod error;
pub mod lexer;
pub mod parser;
pub mod sema;

pub use codegen::{
    ImportedModuleAst, compile_program_ast, compile_program_ast_with_modules,
    compile_program_ast_with_named_modules, compile_source, compile_source_with_file_id,
    parse_source_with_file_id,
};
pub use error::{CompileError, CompileErrorKind};
