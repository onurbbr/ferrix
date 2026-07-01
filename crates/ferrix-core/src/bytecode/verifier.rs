//! Bytecode verification boundaries.
//!
//! Structural verification checks a single chunk. Program verification checks
//! cross-function references, entrypoint validity, and native metadata.

use std::{error::Error, fmt};

use crate::bytecode::{
    BYTECODE_MAGIC, BytecodeLimits, CURRENT_BYTECODE_VERSION, CaptureId, Chunk, ConstId,
    FunctionId, FunctionKind, Instruction, JumpTarget, Program, Register, StringId,
};

/// Wrapper proving a chunk passed structural verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedChunk(Chunk);

/// Wrapper proving a program passed whole-program verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedProgram(Program);

/// Verifies local chunk invariants such as register and pool bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuralVerifier;

/// Verifies whole-program invariants such as calls and entrypoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramVerifier;

/// Typed bytecode verification error with optional function/ip context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationError {
    /// Function being verified, when known.
    pub function_id: Option<FunctionId>,
    /// Instruction pointer that triggered the error, when known.
    pub instruction_ip: Option<usize>,
    /// Specific verification failure kind.
    pub kind: VerificationErrorKind,
}

/// Detailed verifier failure modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerificationErrorKind {
    ArityExceedsRegisterCount {
        arity: u8,
        register_count: u8,
    },
    SourceMapLengthMismatch {
        instructions: usize,
        source_map: usize,
    },
    DebugLocalNamesOutOfRange {
        debug_local_names: usize,
        register_count: u8,
    },
    TooManyConstants {
        constant_count: usize,
        max: usize,
    },
    TooManyStrings {
        string_count: usize,
        max: usize,
    },
    TooManyFunctions {
        function_count: usize,
        max: usize,
    },
    TooManyInstructions {
        instruction_count: usize,
        max: usize,
    },
    InvalidRegister {
        register: Register,
        register_count: u8,
    },
    InvalidConstant {
        constant: ConstId,
        constant_count: usize,
    },
    InvalidString {
        string: StringId,
        string_count: usize,
    },
    InvalidJumpTarget {
        target: JumpTarget,
        instruction_count: usize,
    },
    InvalidFunction {
        function: FunctionId,
        function_count: usize,
    },
    InvalidEntrypoint {
        entry: FunctionId,
        function_count: usize,
    },
    CallArgumentsOutOfRange {
        args_start: Register,
        arg_count: u8,
        register_count: u8,
    },
    ClosureCapturesOutOfRange {
        captures_start: Register,
        capture_count: u8,
        register_count: u8,
    },
    InvalidCapture {
        capture: CaptureId,
        capture_count: u8,
    },
    ArrayElementsOutOfRange {
        elements_start: Register,
        element_count: u8,
        register_count: u8,
    },
    MapEntriesOutOfRange {
        entries_start: Register,
        entry_count: u8,
        register_count: u8,
    },
    RecordFieldsOutOfRange {
        fields_start: Register,
        field_count: usize,
        register_count: u8,
    },
    WrongCallArity {
        function: FunctionId,
        expected: u8,
        actual: u8,
    },
    UnsupportedBytecodeFormat {
        magic: String,
        version: u16,
    },
    FunctionMetadataMismatch {
        field: &'static str,
    },
    MissingReturn,
}

impl VerifiedChunk {
    /// Verifies and wraps a chunk.
    pub fn new(chunk: Chunk) -> Result<Self, VerificationError> {
        StructuralVerifier::verify(chunk)
    }

    /// Returns the verified chunk without consuming the wrapper.
    pub fn as_chunk(&self) -> &Chunk {
        &self.0
    }

    /// Consumes the wrapper and returns the raw chunk.
    pub fn into_inner(self) -> Chunk {
        self.0
    }
}

impl VerifiedProgram {
    pub fn new(program: Program) -> Result<Self, VerificationError> {
        ProgramVerifier::verify(program)
    }

    pub fn as_program(&self) -> &Program {
        &self.0
    }

    pub fn into_inner(self) -> Program {
        self.0
    }
}

impl StructuralVerifier {
    pub fn verify(chunk: Chunk) -> Result<VerifiedChunk, VerificationError> {
        if chunk.arity > chunk.register_count {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::ArityExceedsRegisterCount {
                    arity: chunk.arity,
                    register_count: chunk.register_count,
                },
            ));
        }

        if chunk.source_map.len() != chunk.instructions.len() {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::SourceMapLengthMismatch {
                    instructions: chunk.instructions.len(),
                    source_map: chunk.source_map.len(),
                },
            ));
        }

        if chunk.debug_local_names.len() > usize::from(chunk.register_count) {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::DebugLocalNamesOutOfRange {
                    debug_local_names: chunk.debug_local_names.len(),
                    register_count: chunk.register_count,
                },
            ));
        }

        let limits = BytecodeLimits::current();
        if chunk.constants.len() > limits.max_constants {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::TooManyConstants {
                    constant_count: chunk.constants.len(),
                    max: limits.max_constants,
                },
            ));
        }

        if chunk.strings.len() > limits.max_strings {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::TooManyStrings {
                    string_count: chunk.strings.len(),
                    max: limits.max_strings,
                },
            ));
        }

        if chunk.instructions.len() > limits.max_instructions_per_function {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::TooManyInstructions {
                    instruction_count: chunk.instructions.len(),
                    max: limits.max_instructions_per_function,
                },
            ));
        }

        let mut has_return = false;

        for (ip, instruction) in chunk.instructions.iter().enumerate() {
            if matches!(
                instruction,
                Instruction::Return { .. } | Instruction::Throw { .. }
            ) {
                has_return = true;
            }

            for register in instruction.register_operands() {
                if register.0 >= chunk.register_count {
                    return Err(VerificationError::new(
                        None,
                        Some(ip),
                        VerificationErrorKind::InvalidRegister {
                            register,
                            register_count: chunk.register_count,
                        },
                    ));
                }
            }

            if let Some(constant) = instruction.const_operand()
                && usize::from(constant.0) >= chunk.constants.len()
            {
                return Err(VerificationError::new(
                    None,
                    Some(ip),
                    VerificationErrorKind::InvalidConstant {
                        constant,
                        constant_count: chunk.constants.len(),
                    },
                ));
            }

            for string in instruction.string_operands() {
                if usize::from(string.0) >= chunk.strings.len() {
                    return Err(VerificationError::new(
                        None,
                        Some(ip),
                        VerificationErrorKind::InvalidString {
                            string,
                            string_count: chunk.strings.len(),
                        },
                    ));
                }
            }

            if let Some(target) = instruction.jump_operand()
                && usize::try_from(target.0)
                    .map_or(true, |target| target >= chunk.instructions.len())
            {
                return Err(VerificationError::new(
                    None,
                    Some(ip),
                    VerificationErrorKind::InvalidJumpTarget {
                        target,
                        instruction_count: chunk.instructions.len(),
                    },
                ));
            }

            if let Instruction::CallFunction {
                args_start,
                arg_count,
                ..
            }
            | Instruction::CallValue {
                args_start,
                arg_count,
                ..
            } = instruction
                && *arg_count > 0
            {
                let start = usize::from(args_start.0);
                let end = start + usize::from(*arg_count);
                if end > usize::from(chunk.register_count) {
                    return Err(VerificationError::new(
                        None,
                        Some(ip),
                        VerificationErrorKind::CallArgumentsOutOfRange {
                            args_start: *args_start,
                            arg_count: *arg_count,
                            register_count: chunk.register_count,
                        },
                    ));
                }
            }

            if let Instruction::MakeClosure {
                captures_start,
                capture_count,
                ..
            } = instruction
                && *capture_count > 0
            {
                let start = usize::from(captures_start.0);
                let end = start + usize::from(*capture_count);
                if end > usize::from(chunk.register_count) {
                    return Err(VerificationError::new(
                        None,
                        Some(ip),
                        VerificationErrorKind::ClosureCapturesOutOfRange {
                            captures_start: *captures_start,
                            capture_count: *capture_count,
                            register_count: chunk.register_count,
                        },
                    ));
                }
            }

            if let Some(capture) = instruction.capture_operand()
                && capture.0 >= chunk.capture_count
            {
                return Err(VerificationError::new(
                    None,
                    Some(ip),
                    VerificationErrorKind::InvalidCapture {
                        capture,
                        capture_count: chunk.capture_count,
                    },
                ));
            }

            if let Instruction::ArrayNew {
                elements_start,
                element_count,
                ..
            } = instruction
                && *element_count > 0
            {
                let start = usize::from(elements_start.0);
                let end = start + usize::from(*element_count);
                if end > usize::from(chunk.register_count) {
                    return Err(VerificationError::new(
                        None,
                        Some(ip),
                        VerificationErrorKind::ArrayElementsOutOfRange {
                            elements_start: *elements_start,
                            element_count: *element_count,
                            register_count: chunk.register_count,
                        },
                    ));
                }
            }

            if let Instruction::MapNew {
                entries_start,
                entry_count,
                ..
            } = instruction
                && *entry_count > 0
            {
                let start = usize::from(entries_start.0);
                let end = start + usize::from(*entry_count) * 2;
                if end > usize::from(chunk.register_count) {
                    return Err(VerificationError::new(
                        None,
                        Some(ip),
                        VerificationErrorKind::MapEntriesOutOfRange {
                            entries_start: *entries_start,
                            entry_count: *entry_count,
                            register_count: chunk.register_count,
                        },
                    ));
                }
            }

            if let Instruction::RecordNew {
                fields_start,
                fields,
                ..
            } = instruction
                && !fields.is_empty()
            {
                let start = usize::from(fields_start.0);
                let end = start + fields.len();
                if end > usize::from(chunk.register_count) {
                    return Err(VerificationError::new(
                        None,
                        Some(ip),
                        VerificationErrorKind::RecordFieldsOutOfRange {
                            fields_start: *fields_start,
                            field_count: fields.len(),
                            register_count: chunk.register_count,
                        },
                    ));
                }
            }
        }

        if !has_return {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::MissingReturn,
            ));
        }

        Ok(VerifiedChunk(chunk))
    }
}

impl ProgramVerifier {
    pub fn verify(program: Program) -> Result<VerifiedProgram, VerificationError> {
        if program.format.magic != BYTECODE_MAGIC
            || program.format.version != CURRENT_BYTECODE_VERSION
        {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::UnsupportedBytecodeFormat {
                    magic: program.format.magic.to_string(),
                    version: program.format.version,
                },
            ));
        }

        let limits = BytecodeLimits::current();
        if program.functions.len() > limits.max_functions {
            return Err(VerificationError::new(
                None,
                None,
                VerificationErrorKind::TooManyFunctions {
                    function_count: program.functions.len(),
                    max: limits.max_functions,
                },
            ));
        }

        if usize::from(program.entry.0) >= program.functions.len() {
            return Err(VerificationError::new(
                Some(program.entry),
                None,
                VerificationErrorKind::InvalidEntrypoint {
                    entry: program.entry,
                    function_count: program.functions.len(),
                },
            ));
        }

        for (function_index, function) in program.functions.iter().enumerate() {
            let function_id = FunctionId(function_index as u16);

            match &function.kind {
                FunctionKind::Bytecode(chunk) => {
                    validate_function_chunk_metadata(function, chunk, function_id)?;
                    StructuralVerifier::verify(chunk.clone())
                        .map_err(|error| error.with_function_id(function_id))?;

                    for (ip, instruction) in chunk.instructions.iter().enumerate() {
                        if let Instruction::CallFunction {
                            function,
                            arg_count,
                            ..
                        } = instruction
                        {
                            let Some(callee) = program.function(*function) else {
                                return Err(VerificationError::new(
                                    Some(function_id),
                                    Some(ip),
                                    VerificationErrorKind::InvalidFunction {
                                        function: *function,
                                        function_count: program.functions.len(),
                                    },
                                ));
                            };

                            if callee.arity != *arg_count {
                                return Err(VerificationError::new(
                                    Some(function_id),
                                    Some(ip),
                                    VerificationErrorKind::WrongCallArity {
                                        function: *function,
                                        expected: callee.arity,
                                        actual: *arg_count,
                                    },
                                ));
                            }
                        }

                        if let Instruction::MakeClosure {
                            function,
                            capture_count,
                            ..
                        } = instruction
                        {
                            let Some(callee) = program.function(*function) else {
                                return Err(VerificationError::new(
                                    Some(function_id),
                                    Some(ip),
                                    VerificationErrorKind::InvalidFunction {
                                        function: *function,
                                        function_count: program.functions.len(),
                                    },
                                ));
                            };

                            if callee.capture_count != *capture_count {
                                return Err(VerificationError::new(
                                    Some(function_id),
                                    Some(ip),
                                    VerificationErrorKind::FunctionMetadataMismatch {
                                        field: "capture_count",
                                    },
                                ));
                            }
                        }
                    }
                }
                FunctionKind::Native { name } => {
                    validate_native_function_metadata(function, name, function_id)?;
                }
            }
        }

        Ok(VerifiedProgram(program))
    }
}

fn validate_function_chunk_metadata(
    function: &crate::bytecode::Function,
    chunk: &Chunk,
    function_id: FunctionId,
) -> Result<(), VerificationError> {
    if function.name != chunk.name {
        return Err(VerificationError::new(
            Some(function_id),
            None,
            VerificationErrorKind::FunctionMetadataMismatch { field: "name" },
        ));
    }

    if function.arity != chunk.arity {
        return Err(VerificationError::new(
            Some(function_id),
            None,
            VerificationErrorKind::FunctionMetadataMismatch { field: "arity" },
        ));
    }

    if function.register_count != chunk.register_count {
        return Err(VerificationError::new(
            Some(function_id),
            None,
            VerificationErrorKind::FunctionMetadataMismatch {
                field: "register_count",
            },
        ));
    }

    if function.capture_count != chunk.capture_count {
        return Err(VerificationError::new(
            Some(function_id),
            None,
            VerificationErrorKind::FunctionMetadataMismatch {
                field: "capture_count",
            },
        ));
    }

    Ok(())
}

fn validate_native_function_metadata(
    function: &crate::bytecode::Function,
    native_name: &str,
    function_id: FunctionId,
) -> Result<(), VerificationError> {
    if function.name != native_name {
        return Err(VerificationError::new(
            Some(function_id),
            None,
            VerificationErrorKind::FunctionMetadataMismatch { field: "name" },
        ));
    }

    if function.register_count != function.arity {
        return Err(VerificationError::new(
            Some(function_id),
            None,
            VerificationErrorKind::FunctionMetadataMismatch {
                field: "register_count",
            },
        ));
    }

    if function.capture_count != 0 {
        return Err(VerificationError::new(
            Some(function_id),
            None,
            VerificationErrorKind::FunctionMetadataMismatch {
                field: "capture_count",
            },
        ));
    }

    Ok(())
}

impl VerificationError {
    pub fn new(
        function_id: Option<FunctionId>,
        instruction_ip: Option<usize>,
        kind: VerificationErrorKind,
    ) -> Self {
        Self {
            function_id,
            instruction_ip,
            kind,
        }
    }

    pub fn with_function_id(mut self, function_id: FunctionId) -> Self {
        self.function_id = Some(function_id);
        self
    }
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            VerificationErrorKind::ArityExceedsRegisterCount {
                arity,
                register_count,
            } => write!(
                f,
                "function arity {arity} exceeds register count {register_count}"
            ),
            VerificationErrorKind::SourceMapLengthMismatch {
                instructions,
                source_map,
            } => write!(
                f,
                "source map length {source_map} does not match instruction count {instructions}"
            ),
            VerificationErrorKind::DebugLocalNamesOutOfRange {
                debug_local_names,
                register_count,
            } => write!(
                f,
                "debug local names length {debug_local_names} exceeds register count {register_count}"
            ),
            VerificationErrorKind::TooManyConstants {
                constant_count,
                max,
            } => write!(
                f,
                "constant count {constant_count} exceeds bytecode format maximum {max}"
            ),
            VerificationErrorKind::TooManyStrings { string_count, max } => write!(
                f,
                "string count {string_count} exceeds bytecode format maximum {max}"
            ),
            VerificationErrorKind::TooManyFunctions {
                function_count,
                max,
            } => write!(
                f,
                "function count {function_count} exceeds bytecode format maximum {max}"
            ),
            VerificationErrorKind::TooManyInstructions {
                instruction_count,
                max,
            } => write!(
                f,
                "instruction count {instruction_count} exceeds bytecode format maximum {max}"
            ),
            VerificationErrorKind::InvalidRegister {
                register,
                register_count,
            } => write!(
                f,
                "invalid register {register}; chunk has {register_count} registers"
            ),
            VerificationErrorKind::InvalidConstant {
                constant,
                constant_count,
            } => write!(
                f,
                "invalid constant {constant}; chunk has {constant_count} constants"
            ),
            VerificationErrorKind::InvalidString {
                string,
                string_count,
            } => write!(
                f,
                "invalid string {string}; chunk has {string_count} strings"
            ),
            VerificationErrorKind::InvalidJumpTarget {
                target,
                instruction_count,
            } => write!(
                f,
                "invalid jump target {target}; chunk has {instruction_count} instructions"
            ),
            VerificationErrorKind::InvalidFunction {
                function,
                function_count,
            } => write!(
                f,
                "invalid function {function}; program has {function_count} functions"
            ),
            VerificationErrorKind::InvalidEntrypoint {
                entry,
                function_count,
            } => write!(
                f,
                "invalid entrypoint {entry}; program has {function_count} functions"
            ),
            VerificationErrorKind::CallArgumentsOutOfRange {
                args_start,
                arg_count,
                register_count,
            } => write!(
                f,
                "call arguments starting at {args_start} with count {arg_count} exceed {register_count} registers"
            ),
            VerificationErrorKind::ClosureCapturesOutOfRange {
                captures_start,
                capture_count,
                register_count,
            } => write!(
                f,
                "closure captures starting at {captures_start} with count {capture_count} exceed {register_count} registers"
            ),
            VerificationErrorKind::InvalidCapture {
                capture,
                capture_count,
            } => write!(
                f,
                "invalid capture {capture}; function has {capture_count} captures"
            ),
            VerificationErrorKind::ArrayElementsOutOfRange {
                elements_start,
                element_count,
                register_count,
            } => write!(
                f,
                "array elements starting at {elements_start} with count {element_count} exceed {register_count} registers"
            ),
            VerificationErrorKind::MapEntriesOutOfRange {
                entries_start,
                entry_count,
                register_count,
            } => write!(
                f,
                "map entries starting at {entries_start} with count {entry_count} exceed {register_count} registers"
            ),
            VerificationErrorKind::RecordFieldsOutOfRange {
                fields_start,
                field_count,
                register_count,
            } => write!(
                f,
                "record fields starting at {fields_start} with count {field_count} exceed {register_count} registers"
            ),
            VerificationErrorKind::WrongCallArity {
                function,
                expected,
                actual,
            } => write!(
                f,
                "wrong call arity for {function}; expected {expected}, got {actual}"
            ),
            VerificationErrorKind::UnsupportedBytecodeFormat { magic, version } => {
                write!(f, "unsupported bytecode format {magic} version {version}")
            }
            VerificationErrorKind::FunctionMetadataMismatch { field } => {
                write!(
                    f,
                    "function metadata field `{field}` does not match chunk metadata"
                )
            }
            VerificationErrorKind::MissingReturn => f.write_str("chunk has no return instruction"),
        }?;

        if let Some(function_id) = self.function_id {
            write!(f, " in {function_id}")?;
        }

        if let Some(ip) = self.instruction_ip {
            write!(f, " at instruction {ip}")?;
        }

        Ok(())
    }
}

impl Error for VerificationError {}
