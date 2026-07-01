//! Internal label patch records used by the fluent assembler.

use ferrix_core::bytecode::{Instruction, JumpTarget};

/// Deferred jump target that will be patched once all labels are known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LabelPatch {
    /// Instruction index containing the placeholder jump target.
    pub instruction_index: usize,
    /// User-declared label name to resolve.
    pub label: String,
    /// Shape of jump instruction that must be patched.
    pub kind: LabelPatchKind,
}

/// Jump instruction variants that can carry an assembler label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LabelPatchKind {
    Jump,
    JumpIfFalse,
    JumpIfTrue,
}

impl LabelPatchKind {
    /// Writes the resolved target into the matching jump instruction variant.
    pub fn apply(self, instruction: &mut Instruction, target: JumpTarget) {
        match (self, instruction) {
            (
                Self::Jump,
                Instruction::Jump {
                    target: instruction_target,
                },
            )
            | (
                Self::JumpIfFalse,
                Instruction::JumpIfFalse {
                    target: instruction_target,
                    ..
                },
            )
            | (
                Self::JumpIfTrue,
                Instruction::JumpIfTrue {
                    target: instruction_target,
                    ..
                },
            ) => *instruction_target = target,
            _ => {}
        }
    }
}
