//! Bytecode format constants, version metadata, and size limits.

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
pub const CURRENT_BYTECODE_VERSION: u16 = 1;
pub const NO_FEATURE_FLAGS: u32 = 0;

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
