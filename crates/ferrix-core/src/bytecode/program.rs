//! Whole-program bytecode representation.
//!
//! A `Program` is the unit passed to the VM: a function table plus an entry
//! function id. Functions can be bytecode chunks or native placeholders.

use crate::bytecode::{BytecodeFormat, Chunk, FunctionId};

/// Bytecode program before/after verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// Bytecode format metadata.
    pub format: BytecodeFormat,
    /// Function table addressed by `FunctionId`.
    pub functions: Vec<Function>,
    /// Entry function executed by `Vm::run_program`.
    pub entry: FunctionId,
}

/// Function metadata and implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    /// Human-readable function name.
    pub name: String,
    /// Number of arguments expected by this function.
    pub arity: u8,
    /// Register count required by this function.
    pub register_count: u8,
    /// Bytecode or native implementation kind.
    pub kind: FunctionKind,
}

/// Function implementation kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FunctionKind {
    /// Ferrix bytecode function body.
    Bytecode(Chunk),
    /// Native function resolved by name/id in the VM.
    Native { name: String },
}

/// Errors returned while adding functions to a program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramBuildError {
    /// The function table exceeded the `FunctionId` address space.
    TooManyFunctions { max: usize },
}

impl Program {
    /// Creates an empty program with the given entry function id.
    pub fn new(entry: FunctionId) -> Self {
        Self {
            format: BytecodeFormat::current(),
            functions: Vec::new(),
            entry,
        }
    }

    /// Overrides bytecode format metadata.
    pub fn with_format(mut self, format: BytecodeFormat) -> Self {
        self.format = format;
        self
    }

    /// Adds a function and returns its assigned `FunctionId`.
    pub fn add_function(&mut self, function: Function) -> Result<FunctionId, ProgramBuildError> {
        if self.functions.len() > u16::MAX as usize {
            return Err(ProgramBuildError::TooManyFunctions {
                max: u16::MAX as usize + 1,
            });
        }

        let id = FunctionId(self.functions.len() as u16);
        self.functions.push(function);
        Ok(id)
    }

    /// Looks up a function by id.
    pub fn function(&self, id: FunctionId) -> Option<&Function> {
        self.functions.get(usize::from(id.0))
    }
}

impl Function {
    /// Wraps a bytecode chunk as a function and copies its metadata.
    pub fn bytecode(mut chunk: Chunk) -> Self {
        chunk.name.shrink_to_fit();
        Self {
            name: chunk.name.clone(),
            arity: chunk.arity,
            register_count: chunk.register_count,
            kind: FunctionKind::Bytecode(chunk),
        }
    }

    /// Creates a native function placeholder.
    pub fn native(name: impl Into<String>, arity: u8) -> Self {
        let name = name.into();
        Self {
            name: name.clone(),
            arity,
            register_count: arity,
            kind: FunctionKind::Native { name },
        }
    }

    /// Returns the bytecode chunk for bytecode functions.
    pub fn chunk(&self) -> Option<&Chunk> {
        match &self.kind {
            FunctionKind::Bytecode(chunk) => Some(chunk),
            FunctionKind::Native { .. } => None,
        }
    }

    /// Returns the native function name for native functions.
    pub fn native_name(&self) -> Option<&str> {
        match &self.kind {
            FunctionKind::Bytecode(_) => None,
            FunctionKind::Native { name } => Some(name),
        }
    }
}
