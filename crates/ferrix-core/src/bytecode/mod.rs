//! Bytecode model, verification, disassembly, and binary serialization.
//!
//! This module is deliberately VM-independent: it defines program structure
//! and validation rules, while `ferrix-vm` owns execution.

pub mod chunk;
pub mod disassembler;
pub mod format;
pub mod instruction;
pub mod optimizer;
pub mod program;
pub mod serialization;
pub mod verifier;

pub use chunk::{Chunk, ChunkBuildError};
pub use disassembler::{Disassembler, format_instruction};
pub use format::{
    BYTECODE_CONTAINER_MAGIC, BYTECODE_MAGIC, BytecodeFeature, BytecodeFormat, BytecodeLimits,
    CURRENT_BYTECODE_VERSION, CURRENT_CONTAINER_VERSION, FEATURE_ARRAYS, FEATURE_CLOSURES,
    FEATURE_CUSTOM_EXTENSIONS, FEATURE_DEBUG_SYMBOLS, FEATURE_EXCEPTIONS, FEATURE_MAPS,
    FEATURE_MODULES, FEATURE_NATIVE_CALLS, FEATURE_OPTIMIZED_INT_OPS, FEATURE_RECORDS,
    NO_FEATURE_FLAGS, SUPPORTED_BYTECODE_FEATURE_FLAGS, bytecode_features,
    infer_program_feature_flags, unsupported_feature_flags,
};
pub use instruction::{
    CaptureId, ConstId, FunctionId, Instruction, JumpTarget, Register, StringId,
};
pub use optimizer::optimize_chunk;
pub use program::{Function, FunctionKind, Program, ProgramBuildError};
pub use serialization::{
    BytecodeContainer, BytecodeContainerMetadata, BytecodeDecodeError, BytecodeEncodeError,
    BytecodeSectionEntry, BytecodeSectionKind, decode_bytecode, decode_container, decode_program,
    encode_container, encode_program, inspect_container,
};
pub use verifier::{
    ProgramVerifier, StructuralVerifier, VerificationError, VerificationErrorKind, VerifiedChunk,
    VerifiedProgram,
};
