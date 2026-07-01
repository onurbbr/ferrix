//! Debugger hook types for stepping through a verified Ferrix program.
//!
//! A debugger implements [`Debugger`] and receives a [`DebugEvent`] before each
//! bytecode instruction. The callback can step, continue, or stop execution.

use ferrix_core::{
    Value,
    bytecode::{FunctionId, Instruction, Program},
    diagnostics::SourceSpan,
};

use crate::{CallFrame, Heap};

/// Decision returned by a debugger callback after inspecting an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugAction {
    /// Execute the current instruction and report the next instruction.
    Step,
    /// Continue execution until completion or until the debugger chooses to stop.
    Continue,
    /// Stop execution and return [`DebugOutcome::Quit`].
    Quit,
}

/// Result of a debug run that can stop before normal program completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugOutcome {
    /// Program reached a `return` instruction with the final value.
    Completed(Value),
    /// Debugger requested termination before the program completed.
    Quit,
}

/// Snapshot of VM state passed to a debugger before an instruction executes.
pub struct DebugEvent<'a> {
    /// Full program being executed.
    pub program: &'a Program,
    /// Function that owns the current instruction.
    pub function: FunctionId,
    /// Human-readable function name for UI output.
    pub function_name: &'a str,
    /// Instruction pointer within the current function chunk.
    pub instruction_ip: usize,
    /// Instruction about to be executed.
    pub instruction: &'a Instruction,
    /// Current frame registers before the instruction mutates them.
    pub registers: &'a [Value],
    /// Call stack, including the current frame.
    pub frames: &'a [CallFrame],
    /// Heap view for inspecting referenced objects.
    pub heap: &'a Heap,
    /// Source span mapped from bytecode, when compilation supplied one.
    pub source_span: Option<SourceSpan>,
}

/// Interface implemented by interactive or scripted debuggers.
pub trait Debugger {
    /// Called immediately before each instruction executes.
    fn before_instruction(&mut self, event: DebugEvent<'_>) -> DebugAction;
}
