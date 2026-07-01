//! Bytecode model, verification, disassembly, and binary serialization.
//!
//! This module is deliberately VM-independent: it defines program structure
//! and validation rules, while `ferrix-vm` owns execution.

pub mod chunk;
pub mod disassembler;
pub mod format;
pub mod instruction;
pub mod program;
pub mod serialization;
pub mod verifier;

pub use chunk::{Chunk, ChunkBuildError};
pub use disassembler::{Disassembler, format_instruction};
pub use format::{
    BYTECODE_MAGIC, BytecodeFormat, BytecodeLimits, CURRENT_BYTECODE_VERSION, NO_FEATURE_FLAGS,
};
pub use instruction::{ConstId, FunctionId, Instruction, JumpTarget, Register, StringId};
pub use program::{Function, FunctionKind, Program, ProgramBuildError};
pub use serialization::{BytecodeDecodeError, BytecodeEncodeError, decode_program, encode_program};
pub use verifier::{
    ProgramVerifier, StructuralVerifier, VerificationError, VerificationErrorKind, VerifiedChunk,
    VerifiedProgram,
};
