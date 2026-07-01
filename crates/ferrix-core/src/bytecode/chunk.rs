//! Bytecode chunk storage.
//!
//! A `Chunk` is a single bytecode function body: constants, strings,
//! instructions, source spans, and optional debug-local names.

use crate::{
    bytecode::{ConstId, Instruction, StringId},
    diagnostics::SourceSpan,
    value::Value,
};

/// A bytecode function body with pools and debug metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// Human-readable function/chunk name.
    pub name: String,
    /// Number of arguments expected by the function.
    pub arity: u8,
    /// Number of VM registers allocated for this chunk.
    pub register_count: u8,
    /// Constant pool used by `LoadConst`.
    pub constants: Vec<Value>,
    /// String pool used by `LoadString`.
    pub strings: Vec<String>,
    /// Instruction stream executed by the VM.
    pub instructions: Vec<Instruction>,
    /// Optional source span for each instruction.
    pub source_map: Vec<Option<SourceSpan>>,
    /// Optional source/local names for registers.
    pub debug_local_names: Vec<Option<String>>,
}

/// Errors returned while building a chunk pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkBuildError {
    /// Too many constants were inserted into one chunk.
    TooManyConstants { max: usize },
    /// Too many strings were inserted into one chunk.
    TooManyStrings { max: usize },
}

impl Chunk {
    /// Creates an empty chunk with a name and register count.
    pub fn new(name: impl Into<String>, register_count: u8) -> Self {
        Self {
            name: name.into(),
            arity: 0,
            register_count,
            constants: Vec::new(),
            strings: Vec::new(),
            instructions: Vec::new(),
            source_map: Vec::new(),
            debug_local_names: Vec::new(),
        }
    }

    /// Sets the function arity and returns the updated chunk.
    pub fn with_arity(mut self, arity: u8) -> Self {
        self.arity = arity;
        self
    }

    /// Adds a constant to the constant pool and returns its typed id.
    pub fn add_constant(&mut self, value: Value) -> Result<ConstId, ChunkBuildError> {
        if self.constants.len() > u16::MAX as usize {
            return Err(ChunkBuildError::TooManyConstants {
                max: u16::MAX as usize + 1,
            });
        }

        let id = ConstId(self.constants.len() as u16);
        self.constants.push(value);
        Ok(id)
    }

    /// Adds a string to the string pool and returns its typed id.
    pub fn add_string(&mut self, value: impl Into<String>) -> Result<StringId, ChunkBuildError> {
        if self.strings.len() > u16::MAX as usize {
            return Err(ChunkBuildError::TooManyStrings {
                max: u16::MAX as usize + 1,
            });
        }

        let id = StringId(self.strings.len() as u16);
        self.strings.push(value.into());
        Ok(id)
    }

    /// Appends an instruction without source metadata.
    pub fn push_instruction(&mut self, instruction: Instruction) {
        self.instructions.push(instruction);
        self.source_map.push(None);
    }

    /// Appends an instruction and records its optional source span.
    pub fn push_instruction_with_span(
        &mut self,
        instruction: Instruction,
        span: Option<SourceSpan>,
    ) {
        self.instructions.push(instruction);
        self.source_map.push(span);
    }

    /// Stores a human-readable name for a register.
    pub fn set_debug_local_name(
        &mut self,
        register: crate::bytecode::Register,
        name: impl Into<String>,
    ) {
        let index = usize::from(register.0);
        if self.debug_local_names.len() <= index {
            self.debug_local_names.resize(index + 1, None);
        }
        self.debug_local_names[index] = Some(name.into());
    }
}
