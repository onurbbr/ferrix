//! Bytecode format constants, version metadata, and size limits.

use crate::bytecode::{FunctionKind, Instruction, Program};

/// Version marker stored on bytecode programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BytecodeFormat {
    /// Magic string identifying Ferrix bytecode.
    pub magic: &'static str,
    /// Bytecode instruction/schema version.
    pub version: u16,
    /// Feature flags for future optional format extensions.
    pub feature_flags: u32,
}

/// Maximum supported sizes for bytecode structures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BytecodeLimits {
    /// Maximum constants per chunk.
    pub max_constants: usize,
    /// Maximum strings per chunk.
    pub max_strings: usize,
    /// Maximum functions per program.
    pub max_functions: usize,
    /// Maximum instruction count per bytecode function.
    pub max_instructions_per_function: usize,
    /// Maximum registers per function.
    pub max_registers_per_function: usize,
}

pub const BYTECODE_MAGIC: &str = "FERRIXBC";
pub const BYTECODE_CONTAINER_MAGIC: &str = "FERRIXCT";
pub const CURRENT_BYTECODE_VERSION: u16 = 1;
pub const CURRENT_CONTAINER_VERSION: u16 = 1;
pub const NO_FEATURE_FLAGS: u32 = 0;
pub const FEATURE_CLOSURES: u32 = 1 << 0;
pub const FEATURE_RECORDS: u32 = 1 << 1;
pub const FEATURE_ARRAYS: u32 = 1 << 2;
pub const FEATURE_MAPS: u32 = 1 << 3;
pub const FEATURE_MODULES: u32 = 1 << 4;
pub const FEATURE_NATIVE_CALLS: u32 = 1 << 5;
pub const FEATURE_EXCEPTIONS: u32 = 1 << 6;
pub const FEATURE_CUSTOM_EXTENSIONS: u32 = 1 << 7;
pub const FEATURE_DEBUG_SYMBOLS: u32 = 1 << 8;
pub const FEATURE_OPTIMIZED_INT_OPS: u32 = 1 << 9;
pub const SUPPORTED_BYTECODE_FEATURE_FLAGS: u32 = FEATURE_CLOSURES
    | FEATURE_RECORDS
    | FEATURE_ARRAYS
    | FEATURE_MAPS
    | FEATURE_MODULES
    | FEATURE_NATIVE_CALLS
    | FEATURE_EXCEPTIONS
    | FEATURE_DEBUG_SYMBOLS
    | FEATURE_OPTIMIZED_INT_OPS;

/// Optional bytecode/container feature bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytecodeFeature {
    Closures,
    Records,
    Arrays,
    Maps,
    Modules,
    NativeCalls,
    Exceptions,
    CustomExtensions,
    DebugSymbols,
    OptimizedIntegerOpcodes,
}

impl BytecodeFeature {
    /// Returns the stable dotted feature name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closures => "closures",
            Self::Records => "records",
            Self::Arrays => "arrays",
            Self::Maps => "maps",
            Self::Modules => "modules",
            Self::NativeCalls => "native-calls",
            Self::Exceptions => "exceptions",
            Self::CustomExtensions => "custom-extensions",
            Self::DebugSymbols => "debug-symbols",
            Self::OptimizedIntegerOpcodes => "optimized-integer-opcodes",
        }
    }

    /// Returns the feature flag bit for this feature.
    pub fn flag(self) -> u32 {
        match self {
            Self::Closures => FEATURE_CLOSURES,
            Self::Records => FEATURE_RECORDS,
            Self::Arrays => FEATURE_ARRAYS,
            Self::Maps => FEATURE_MAPS,
            Self::Modules => FEATURE_MODULES,
            Self::NativeCalls => FEATURE_NATIVE_CALLS,
            Self::Exceptions => FEATURE_EXCEPTIONS,
            Self::CustomExtensions => FEATURE_CUSTOM_EXTENSIONS,
            Self::DebugSymbols => FEATURE_DEBUG_SYMBOLS,
            Self::OptimizedIntegerOpcodes => FEATURE_OPTIMIZED_INT_OPS,
        }
    }
}

impl BytecodeFormat {
    pub const fn current() -> Self {
        Self {
            magic: BYTECODE_MAGIC,
            version: CURRENT_BYTECODE_VERSION,
            feature_flags: NO_FEATURE_FLAGS,
        }
    }
}

impl Default for BytecodeFormat {
    fn default() -> Self {
        Self::current()
    }
}

impl BytecodeLimits {
    pub const fn current() -> Self {
        Self {
            max_constants: u16::MAX as usize + 1,
            max_strings: u16::MAX as usize + 1,
            max_functions: u16::MAX as usize + 1,
            max_instructions_per_function: u32::MAX as usize + 1,
            max_registers_per_function: u8::MAX as usize + 1,
        }
    }
}

impl Default for BytecodeLimits {
    fn default() -> Self {
        Self::current()
    }
}

/// Returns the known feature bits present in a flag set.
pub fn bytecode_features(flags: u32) -> Vec<BytecodeFeature> {
    [
        BytecodeFeature::Closures,
        BytecodeFeature::Records,
        BytecodeFeature::Arrays,
        BytecodeFeature::Maps,
        BytecodeFeature::Modules,
        BytecodeFeature::NativeCalls,
        BytecodeFeature::Exceptions,
        BytecodeFeature::CustomExtensions,
        BytecodeFeature::DebugSymbols,
        BytecodeFeature::OptimizedIntegerOpcodes,
    ]
    .into_iter()
    .filter(|feature| flags & feature.flag() != 0)
    .collect()
}

/// Returns unsupported known and unknown feature bits.
pub fn unsupported_feature_flags(flags: u32, supported: u32) -> u32 {
    flags & !supported
}

/// Infers feature flags required by the bytecode instructions in a program.
pub fn infer_program_feature_flags(program: &Program) -> u32 {
    let mut flags = program.format.feature_flags;
    for function in &program.functions {
        match &function.kind {
            FunctionKind::Native { .. } => flags |= FEATURE_NATIVE_CALLS,
            FunctionKind::Bytecode(chunk) => {
                if chunk.source_map.iter().any(Option::is_some)
                    || chunk.debug_local_names.iter().any(Option::is_some)
                {
                    flags |= FEATURE_DEBUG_SYMBOLS;
                }
                for instruction in &chunk.instructions {
                    flags |= instruction_feature_flags(instruction);
                }
            }
        }
    }
    flags
}

fn instruction_feature_flags(instruction: &Instruction) -> u32 {
    match instruction {
        Instruction::MakeClosure { .. }
        | Instruction::LoadCapture { .. }
        | Instruction::LoadCaptureCell { .. }
        | Instruction::StoreCapture { .. }
        | Instruction::CallValue { .. }
        | Instruction::MakeUpvalue { .. }
        | Instruction::LoadUpvalue { .. }
        | Instruction::StoreUpvalue { .. } => FEATURE_CLOSURES,
        Instruction::ArrayNew { .. }
        | Instruction::ArrayGet { .. }
        | Instruction::ArraySet { .. } => FEATURE_ARRAYS,
        Instruction::MapNew { .. }
        | Instruction::IndexGet { .. }
        | Instruction::IndexSet { .. } => FEATURE_MAPS,
        Instruction::RecordNew { .. }
        | Instruction::FieldGet { .. }
        | Instruction::FieldSet { .. } => FEATURE_RECORDS,
        Instruction::PushHandler { .. } | Instruction::PopHandler | Instruction::Throw { .. } => {
            FEATURE_EXCEPTIONS
        }
        Instruction::AddInt { .. }
        | Instruction::SubInt { .. }
        | Instruction::MulInt { .. }
        | Instruction::DivInt { .. }
        | Instruction::LessInt { .. }
        | Instruction::LessEqualInt { .. }
        | Instruction::GreaterInt { .. }
        | Instruction::GreaterEqualInt { .. } => FEATURE_OPTIMIZED_INT_OPS,
        _ => NO_FEATURE_FLAGS,
    }
}
