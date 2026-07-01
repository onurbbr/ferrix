//! VM runtime error types and diagnostic conversion helpers.
//!
//! Errors record the instruction pointer that failed, a structured error kind,
//! and an optional stack trace. CLI and tests can convert them into diagnostics
//! with source spans when bytecode source maps are available.

use std::{error::Error, fmt};

use ferrix_core::{
    ObjRef, Value,
    bytecode::StringId,
    bytecode::{Chunk, ConstId, FunctionId, FunctionKind, JumpTarget, Program, Register},
    diagnostics::{Diagnostic, SourceSpan},
};

/// Runtime failure raised while executing bytecode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmError {
    /// Instruction pointer where the error happened, when known.
    pub instruction_ip: Option<usize>,
    /// Machine-readable reason for the failure.
    pub kind: VmErrorKind,
    /// Call stack captured at the point where the error is reported.
    pub stack_trace: Vec<VmStackFrame>,
}

/// One frame in a VM stack trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmStackFrame {
    /// Function identifier for the active frame.
    pub function: FunctionId,
    /// Function name displayed in diagnostics.
    pub name: String,
    /// Instruction pointer within that function, when known.
    pub instruction_ip: Option<usize>,
}

/// Specific runtime error category with data needed for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VmErrorKind {
    /// Tried to read or write a register outside the current frame.
    InvalidRegister {
        register: Register,
        register_count: u8,
    },
    /// Tried to load a constant outside the current chunk.
    InvalidConstant {
        constant: ConstId,
        constant_count: usize,
    },
    /// Tried to load a string outside the current chunk.
    InvalidString {
        string: StringId,
        string_count: usize,
    },
    /// Tried to jump outside the instruction stream.
    InvalidJumpTarget {
        target: JumpTarget,
        instruction_count: usize,
    },
    /// Tried to call a function id that the program does not contain.
    InvalidFunction {
        function: FunctionId,
        function_count: usize,
    },
    /// Tried to load a capture outside the current closure environment.
    InvalidCapture {
        capture: ferrix_core::bytecode::CaptureId,
        capture_count: usize,
    },
    /// Program declared a native function that was not registered in the VM.
    MissingNativeFunction { function: FunctionId },
    /// Call stack exceeded [`RuntimeLimits`](crate::RuntimeLimits).
    CallDepthExceeded { max_call_depth: usize },
    /// Instruction budget exceeded [`RuntimeLimits`](crate::RuntimeLimits).
    InstructionLimitExceeded { max_instruction_count: usize },
    /// Heap object limit exceeded [`RuntimeLimits`](crate::RuntimeLimits).
    HeapObjectLimitExceeded { max_heap_objects: usize },
    /// Object reference was out of range, stale, or already swept by GC.
    InvalidObjectRef { reference: ObjRef },
    /// Runtime value had a different type than an instruction/native expected.
    TypeError {
        expected: &'static str,
        found: Value,
    },
    /// Integer division attempted with zero as divisor.
    DivisionByZero,
    /// Checked integer arithmetic overflowed.
    ArithmeticOverflow { operation: &'static str },
    /// Array access used an index outside the array bounds.
    IndexOutOfBounds { index: i64, len: usize },
    /// Source program raised a value that no active catch handler recovered.
    UncaughtThrow { value: Value },
    /// Instruction pointer itself moved outside the current chunk.
    InstructionPointerOutOfBounds { ip: usize, instruction_count: usize },
    /// Execution left a function without encountering `return`.
    MissingReturn,
}

impl VmError {
    /// Creates a runtime error without a stack trace.
    pub fn new(instruction_ip: Option<usize>, kind: VmErrorKind) -> Self {
        Self {
            instruction_ip,
            kind,
            stack_trace: Vec::new(),
        }
    }

    /// Attaches a stack trace if the error does not already have one.
    pub fn with_stack_trace(mut self, stack_trace: Vec<VmStackFrame>) -> Self {
        if self.stack_trace.is_empty() {
            self.stack_trace = stack_trace;
        }
        self
    }

    /// Converts the error into a source-less diagnostic.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::new(self.to_string(), None).with_notes(self.stack_trace_notes())
    }

    /// Converts the error into a diagnostic using a single chunk source map.
    pub fn to_diagnostic_with_chunk(&self, chunk: &Chunk) -> Diagnostic {
        let span = self
            .instruction_ip
            .and_then(|ip| chunk.source_map.get(ip).copied().flatten());
        Diagnostic::new(self.to_string(), span).with_notes(self.stack_trace_notes())
    }

    /// Converts the error into a diagnostic using program-level source maps.
    pub fn to_diagnostic_with_program(&self, program: &Program) -> Diagnostic {
        let span = self
            .stack_trace
            .first()
            .and_then(|frame| source_span_for_frame(program, frame))
            .or_else(|| source_span_for_entry(program, self.instruction_ip));
        Diagnostic::new(self.to_string(), span).with_notes(self.stack_trace_notes())
    }

    fn stack_trace_notes(&self) -> Vec<String> {
        if self.stack_trace.len() < 2 {
            return Vec::new();
        }

        let mut notes = Vec::with_capacity(self.stack_trace.len() + 1);
        notes.push("stack trace:".to_string());
        notes.extend(self.stack_trace.iter().map(|frame| {
            if let Some(ip) = frame.instruction_ip {
                format!("  at {} ({}, instruction {ip})", frame.name, frame.function)
            } else {
                format!("  at {} ({})", frame.name, frame.function)
            }
        }));
        notes
    }
}

fn source_span_for_frame(program: &Program, frame: &VmStackFrame) -> Option<SourceSpan> {
    let function = program.function(frame.function)?;
    let FunctionKind::Bytecode(chunk) = &function.kind else {
        return None;
    };
    let ip = frame.instruction_ip?;

    chunk.source_map.get(ip).copied().flatten()
}

fn source_span_for_entry(program: &Program, instruction_ip: Option<usize>) -> Option<SourceSpan> {
    let function = program.function(program.entry)?;
    let FunctionKind::Bytecode(chunk) = &function.kind else {
        return None;
    };
    let ip = instruction_ip?;

    chunk.source_map.get(ip).copied().flatten()
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            VmErrorKind::InvalidRegister {
                register,
                register_count,
            } => write!(
                f,
                "invalid register {register}; frame has {register_count} registers"
            ),
            VmErrorKind::InvalidConstant {
                constant,
                constant_count,
            } => write!(
                f,
                "invalid constant {constant}; chunk has {constant_count} constants"
            ),
            VmErrorKind::InvalidString {
                string,
                string_count,
            } => write!(
                f,
                "invalid string {string}; chunk has {string_count} strings"
            ),
            VmErrorKind::InvalidJumpTarget {
                target,
                instruction_count,
            } => write!(
                f,
                "invalid jump target {target}; chunk has {instruction_count} instructions"
            ),
            VmErrorKind::InvalidFunction {
                function,
                function_count,
            } => write!(
                f,
                "invalid function {function}; program has {function_count} functions"
            ),
            VmErrorKind::InvalidCapture {
                capture,
                capture_count,
            } => write!(
                f,
                "invalid capture {capture}; frame has {capture_count} captures"
            ),
            VmErrorKind::MissingNativeFunction { function } => {
                write!(f, "missing native implementation for {function}")
            }
            VmErrorKind::CallDepthExceeded { max_call_depth } => {
                write!(f, "call depth limit exceeded; maximum is {max_call_depth}")
            }
            VmErrorKind::InstructionLimitExceeded {
                max_instruction_count,
            } => write!(
                f,
                "instruction limit exceeded; maximum is {max_instruction_count}"
            ),
            VmErrorKind::HeapObjectLimitExceeded { max_heap_objects } => write!(
                f,
                "heap object limit exceeded; maximum is {max_heap_objects}"
            ),
            VmErrorKind::InvalidObjectRef { reference } => {
                write!(f, "invalid object reference {reference}")
            }
            VmErrorKind::TypeError { expected, found } => {
                write!(f, "type error: expected {expected}, found {found:?}")
            }
            VmErrorKind::DivisionByZero => f.write_str("division by zero"),
            VmErrorKind::ArithmeticOverflow { operation } => {
                write!(f, "arithmetic overflow during {operation}")
            }
            VmErrorKind::IndexOutOfBounds { index, len } => {
                write!(f, "array index {index} is out of bounds for length {len}")
            }
            VmErrorKind::UncaughtThrow { value } => {
                write!(f, "uncaught throw: {value:?}")
            }
            VmErrorKind::InstructionPointerOutOfBounds {
                ip,
                instruction_count,
            } => write!(
                f,
                "instruction pointer {ip} is outside {instruction_count} instructions"
            ),
            VmErrorKind::MissingReturn => f.write_str("program ended without return"),
        }?;

        if let Some(ip) = self.instruction_ip {
            write!(f, " at instruction {ip}")?;
        }

        Ok(())
    }
}

impl Error for VmError {}
