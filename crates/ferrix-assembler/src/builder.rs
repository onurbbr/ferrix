//! Fluent bytecode assembler for tests and low-level tooling.
//!
//! The assembler builds a [`Chunk`] instruction by instruction, supports named
//! labels for forward/backward jumps, and verifies the final chunk by default.

use std::{collections::HashMap, error::Error, fmt};

use ferrix_core::{
    Value,
    bytecode::{
        Chunk, ChunkBuildError, ConstId, Instruction, JumpTarget, Register, VerificationError,
        VerifiedChunk,
    },
};

use crate::label::{LabelPatch, LabelPatchKind};

/// Builder-style assembler for constructing a single bytecode chunk.
#[derive(Clone, Debug)]
pub struct Assembler {
    chunk: Chunk,
    labels: HashMap<String, u32>,
    patches: Vec<LabelPatch>,
    pending_error: Option<AssemblerError>,
}

/// Errors that can happen while assembling or verifying a chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssemblerError {
    /// Constant or string table construction failed.
    ChunkBuild(ChunkBuildError),
    /// Final bytecode verification failed.
    Verification(VerificationError),
    /// A label was declared more than once.
    DuplicateLabel { label: String },
    /// A jump referenced a label that was never declared.
    UndefinedLabel { label: String },
    /// The chunk exceeded the jump target address range.
    TooManyInstructions { max: usize },
}

impl Assembler {
    /// Creates an assembler for a chunk with the given function/chunk name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            chunk: Chunk::new(name, 0),
            labels: HashMap::new(),
            patches: Vec::new(),
            pending_error: None,
        }
    }

    /// Sets the register count required by the assembled chunk.
    pub fn registers(mut self, count: u8) -> Self {
        self.chunk.register_count = count;
        self
    }

    /// Sets the number of arguments expected by the assembled chunk.
    pub fn arity(mut self, arity: u8) -> Self {
        self.chunk.arity = arity;
        self
    }

    /// Appends an integer constant to the chunk constant table.
    pub fn int(self, value: i64) -> Self {
        self.constant(Value::Int(value))
    }

    /// Appends a boolean constant to the chunk constant table.
    pub fn bool(self, value: bool) -> Self {
        self.constant(Value::Bool(value))
    }

    /// Appends a nil constant to the chunk constant table.
    pub fn nil(self) -> Self {
        self.constant(Value::Nil)
    }

    /// Appends a string literal to the chunk string table.
    pub fn string(mut self, value: impl Into<String>) -> Self {
        if self.pending_error.is_some() {
            return self;
        }

        if let Err(error) = self.chunk.add_string(value) {
            self.pending_error = Some(AssemblerError::ChunkBuild(error));
        }

        self
    }

    /// Emits `LoadConst` into a destination register.
    pub fn load_const(self, dst: u8, constant: u16) -> Self {
        self.push(Instruction::LoadConst {
            dst: Register(dst),
            constant: ConstId(constant),
        })
    }

    /// Emits `LoadString` into a destination register.
    pub fn load_string(self, dst: u8, string: u16) -> Self {
        self.push(Instruction::LoadString {
            dst: Register(dst),
            string: ferrix_core::bytecode::StringId(string),
        })
    }

    /// Emits a register-to-register move.
    pub fn mov(self, dst: u8, src: u8) -> Self {
        self.push(Instruction::Move {
            dst: Register(dst),
            src: Register(src),
        })
    }

    /// Emits checked integer addition.
    pub fn add(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::Add {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits checked integer subtraction.
    pub fn sub(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::Sub {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits checked integer multiplication.
    pub fn mul(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::Mul {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits checked integer division.
    pub fn div(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::Div {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits an unconditional jump to a named label.
    pub fn jump(self, label: impl Into<String>) -> Self {
        self.push_label_jump(LabelPatchKind::Jump, label.into(), |target| {
            Instruction::Jump { target }
        })
    }

    /// Emits an unconditional jump to a concrete instruction index.
    pub fn jump_to(self, target: u32) -> Self {
        self.push(Instruction::Jump {
            target: JumpTarget(target),
        })
    }

    /// Emits a false-branch jump to a named label.
    pub fn jump_if_false(self, condition: u8, label: impl Into<String>) -> Self {
        let condition = Register(condition);
        self.push_label_jump(LabelPatchKind::JumpIfFalse, label.into(), |target| {
            Instruction::JumpIfFalse { condition, target }
        })
    }

    /// Emits a false-branch jump to a concrete instruction index.
    pub fn jump_if_false_to(self, condition: u8, target: u32) -> Self {
        self.push(Instruction::JumpIfFalse {
            condition: Register(condition),
            target: JumpTarget(target),
        })
    }

    /// Emits a true-branch jump to a named label.
    pub fn jump_if_true(self, condition: u8, label: impl Into<String>) -> Self {
        let condition = Register(condition);
        self.push_label_jump(LabelPatchKind::JumpIfTrue, label.into(), |target| {
            Instruction::JumpIfTrue { condition, target }
        })
    }

    /// Emits a true-branch jump to a concrete instruction index.
    pub fn jump_if_true_to(self, condition: u8, target: u32) -> Self {
        self.push(Instruction::JumpIfTrue {
            condition: Register(condition),
            target: JumpTarget(target),
        })
    }

    /// Emits equality comparison.
    pub fn equal(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::Equal {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits inequality comparison.
    pub fn not_equal(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::NotEqual {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits integer less-than comparison.
    pub fn less(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::Less {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits integer less-than-or-equal comparison.
    pub fn less_equal(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::LessEqual {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits integer greater-than comparison.
    pub fn greater(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::Greater {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits integer greater-than-or-equal comparison.
    pub fn greater_equal(self, dst: u8, lhs: u8, rhs: u8) -> Self {
        self.push(Instruction::GreaterEqual {
            dst: Register(dst),
            lhs: Register(lhs),
            rhs: Register(rhs),
        })
    }

    /// Emits boolean negation.
    pub fn not(self, dst: u8, src: u8) -> Self {
        self.push(Instruction::Not {
            dst: Register(dst),
            src: Register(src),
        })
    }

    /// Emits a function call using a contiguous argument register range.
    pub fn call_function(self, dst: u8, function: u16, args_start: u8, arg_count: u8) -> Self {
        self.push(Instruction::CallFunction {
            dst: Register(dst),
            function: ferrix_core::bytecode::FunctionId(function),
            args_start: Register(args_start),
            arg_count,
        })
    }

    /// Emits array allocation from a contiguous element register range.
    pub fn array_new(self, dst: u8, elements_start: u8, element_count: u8) -> Self {
        self.push(Instruction::ArrayNew {
            dst: Register(dst),
            elements_start: Register(elements_start),
            element_count,
        })
    }

    /// Emits map allocation from contiguous key/value register pairs.
    pub fn map_new(self, dst: u8, entries_start: u8, entry_count: u8) -> Self {
        self.push(Instruction::MapNew {
            dst: Register(dst),
            entries_start: Register(entries_start),
            entry_count,
        })
    }

    /// Emits generic index read for arrays and maps.
    pub fn index_get(self, dst: u8, target: u8, index: u8) -> Self {
        self.push(Instruction::IndexGet {
            dst: Register(dst),
            target: Register(target),
            index: Register(index),
        })
    }

    /// Emits generic index write for arrays and maps.
    pub fn index_set(self, target: u8, index: u8, value: u8) -> Self {
        self.push(Instruction::IndexSet {
            target: Register(target),
            index: Register(index),
            value: Register(value),
        })
    }

    /// Emits direct array index read.
    pub fn array_get(self, dst: u8, array: u8, index: u8) -> Self {
        self.push(Instruction::ArrayGet {
            dst: Register(dst),
            array: Register(array),
            index: Register(index),
        })
    }

    /// Emits direct array index write.
    pub fn array_set(self, array: u8, index: u8, value: u8) -> Self {
        self.push(Instruction::ArraySet {
            array: Register(array),
            index: Register(index),
            value: Register(value),
        })
    }

    /// Declares a named label at the current instruction index.
    pub fn label(mut self, name: impl Into<String>) -> Self {
        if self.pending_error.is_some() {
            return self;
        }

        let name = name.into();
        match self.current_instruction_index() {
            Ok(target) if self.labels.insert(name.clone(), target).is_some() => {
                self.pending_error = Some(AssemblerError::DuplicateLabel { label: name });
            }
            Ok(_) => {}
            Err(error) => self.pending_error = Some(error),
        }

        self
    }

    /// Emits a return from the source register.
    pub fn ret(self, src: u8) -> Self {
        self.push(Instruction::Return { src: Register(src) })
    }

    /// Resolves labels, verifies the chunk, and returns verified bytecode.
    pub fn finish(mut self) -> Result<VerifiedChunk, AssemblerError> {
        if let Some(error) = self.pending_error {
            return Err(error);
        }

        self.patch_labels()?;
        VerifiedChunk::new(self.chunk).map_err(AssemblerError::Verification)
    }

    /// Resolves labels and returns the raw chunk without verification.
    pub fn finish_unverified(mut self) -> Result<Chunk, AssemblerError> {
        if let Some(error) = self.pending_error {
            return Err(error);
        }

        self.patch_labels()?;
        Ok(self.chunk)
    }

    fn constant(mut self, value: Value) -> Self {
        if self.pending_error.is_some() {
            return self;
        }

        if let Err(error) = self.chunk.add_constant(value) {
            self.pending_error = Some(AssemblerError::ChunkBuild(error));
        }

        self
    }

    fn push(mut self, instruction: Instruction) -> Self {
        self.chunk.push_instruction(instruction);
        self
    }

    fn push_label_jump(
        mut self,
        kind: LabelPatchKind,
        label: String,
        make_instruction: impl FnOnce(JumpTarget) -> Instruction,
    ) -> Self {
        if self.pending_error.is_some() {
            return self;
        }

        let instruction_index = self.chunk.instructions.len();
        self.chunk
            .push_instruction(make_instruction(JumpTarget(u32::MAX)));
        self.patches.push(LabelPatch {
            instruction_index,
            label,
            kind,
        });
        self
    }

    fn patch_labels(&mut self) -> Result<(), AssemblerError> {
        for patch in &self.patches {
            let target =
                *self
                    .labels
                    .get(&patch.label)
                    .ok_or_else(|| AssemblerError::UndefinedLabel {
                        label: patch.label.clone(),
                    })?;

            if let Some(instruction) = self.chunk.instructions.get_mut(patch.instruction_index) {
                patch.kind.apply(instruction, JumpTarget(target));
            }
        }

        Ok(())
    }

    fn current_instruction_index(&self) -> Result<u32, AssemblerError> {
        if self.chunk.instructions.len() > u32::MAX as usize {
            return Err(AssemblerError::TooManyInstructions {
                max: u32::MAX as usize + 1,
            });
        }

        Ok(self.chunk.instructions.len() as u32)
    }
}

impl fmt::Display for AssemblerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChunkBuild(ChunkBuildError::TooManyConstants { max }) => {
                write!(f, "too many constants; maximum is {max}")
            }
            Self::ChunkBuild(ChunkBuildError::TooManyStrings { max }) => {
                write!(f, "too many strings; maximum is {max}")
            }
            Self::Verification(error) => write!(f, "{error}"),
            Self::DuplicateLabel { label } => write!(f, "duplicate label `{label}`"),
            Self::UndefinedLabel { label } => write!(f, "undefined label `{label}`"),
            Self::TooManyInstructions { max } => {
                write!(f, "too many instructions; maximum is {max}")
            }
        }
    }
}

impl Error for AssemblerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ChunkBuild(_) => None,
            Self::Verification(error) => Some(error),
            Self::DuplicateLabel { .. }
            | Self::UndefinedLabel { .. }
            | Self::TooManyInstructions { .. } => None,
        }
    }
}
