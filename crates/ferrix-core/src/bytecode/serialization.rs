//! Binary bytecode serialization.
//!
//! Encodes verified program data into a compact deterministic byte format and
//! decodes it back through the normal verifier boundary.

use std::{error::Error, fmt};

use crate::{
    ObjRef, Value,
    bytecode::{
        BYTECODE_CONTAINER_MAGIC, BYTECODE_MAGIC, BytecodeFormat, CURRENT_CONTAINER_VERSION, Chunk,
        Function, FunctionId, FunctionKind, Instruction, JumpTarget, Program, Register,
        VerifiedProgram, infer_program_feature_flags,
    },
    diagnostics::{FileId, SourceSpan},
};

const FORMAT_VERSION: u16 = 1;

/// Optional section kind in a Ferrix bytecode container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytecodeSectionKind {
    /// Encoded legacy program payload.
    ProgramPayload,
    /// Reserved import table metadata.
    ImportTable,
    /// Reserved export table metadata.
    ExportTable,
    /// Reserved interface metadata.
    InterfaceMetadata,
    /// Reserved debug/source-map metadata.
    Debug,
}

impl BytecodeSectionKind {
    fn tag(self) -> u8 {
        match self {
            Self::ProgramPayload => 0,
            Self::ImportTable => 1,
            Self::ExportTable => 2,
            Self::InterfaceMetadata => 3,
            Self::Debug => 4,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, BytecodeDecodeError> {
        match tag {
            0 => Ok(Self::ProgramPayload),
            1 => Ok(Self::ImportTable),
            2 => Ok(Self::ExportTable),
            3 => Ok(Self::InterfaceMetadata),
            4 => Ok(Self::Debug),
            _ => Err(BytecodeDecodeError::InvalidSectionKind { tag }),
        }
    }
}

/// One section entry discovered in a bytecode container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BytecodeSectionEntry {
    /// Section kind.
    pub kind: BytecodeSectionKind,
    /// Byte offset in the container where the section payload starts.
    pub offset: usize,
    /// Section payload length in bytes.
    pub len: usize,
}

/// Stable metadata wrapper for serialized bytecode containers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BytecodeContainerMetadata {
    /// Bytecode instruction/schema version required by the payload.
    pub bytecode_format_version: u16,
    /// Minimum Ferrix version expected by producer/tooling.
    pub min_ferrix_version: String,
    /// Required bytecode feature flags.
    pub feature_flags: u32,
    /// Dotted host capabilities required by the payload.
    pub required_capabilities: Vec<String>,
    /// Entry function encoded in the payload.
    pub entry: FunctionId,
    /// Optional module or package name.
    pub module_name: Option<String>,
    /// Whether debug/source metadata is present.
    pub debug_section_present: bool,
    /// Simple checksum of the program payload.
    pub checksum: u64,
    /// Producer optimization level.
    pub optimization_level: u8,
    /// Whether an import table section is present.
    pub import_table_present: bool,
    /// Whether an export table section is present.
    pub export_table_present: bool,
    /// Whether an interface metadata section is present.
    pub interface_metadata_present: bool,
}

impl BytecodeContainerMetadata {
    /// Builds metadata from the current program model.
    pub fn for_program(program: &Program) -> Self {
        Self {
            bytecode_format_version: program.format.version,
            min_ferrix_version: env!("CARGO_PKG_VERSION").to_string(),
            feature_flags: infer_program_feature_flags(program),
            required_capabilities: Vec::new(),
            entry: program.entry,
            module_name: None,
            debug_section_present: program_has_debug_metadata(program),
            checksum: 0,
            optimization_level: 0,
            import_table_present: false,
            export_table_present: false,
            interface_metadata_present: false,
        }
    }

    /// Adds one required host capability by dotted name.
    pub fn with_required_capability(mut self, capability: impl Into<String>) -> Self {
        self.required_capabilities.push(capability.into());
        self
    }

    /// Sets optional module or package name.
    pub fn with_module_name(mut self, module_name: impl Into<String>) -> Self {
        self.module_name = Some(module_name.into());
        self
    }
}

/// Decoded bytecode container with metadata and verified program payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BytecodeContainer {
    /// Stable container metadata.
    pub metadata: BytecodeContainerMetadata,
    /// Section entries discovered while decoding.
    pub sections: Vec<BytecodeSectionEntry>,
    /// Verified program payload.
    pub program: VerifiedProgram,
}

/// Errors produced while encoding a bytecode program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BytecodeEncodeError {
    /// A length field cannot fit in the binary format.
    LengthTooLarge { field: &'static str },
}

/// Errors produced while decoding a bytecode program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BytecodeDecodeError {
    /// Input ended before the expected structure was fully read.
    UnexpectedEof,
    /// File magic did not match Ferrix bytecode.
    InvalidMagic,
    /// Serialization format version is unknown.
    UnsupportedVersion { version: u16 },
    /// Bytecode container format version is unknown.
    UnsupportedContainerVersion { version: u16 },
    /// Encoded section kind is unknown.
    InvalidSectionKind { tag: u8 },
    /// Bytecode container did not include a program payload section.
    MissingProgramSection,
    /// Bytecode container payload checksum did not match metadata.
    ChecksumMismatch { expected: u64, actual: u64 },
    /// String data was not valid UTF-8.
    InvalidUtf8,
    /// Encoded `Value` tag is unknown.
    InvalidValueTag { tag: u8 },
    /// Encoded instruction opcode is unknown.
    InvalidInstructionOpcode { opcode: u8 },
    /// Encoded function kind is unknown.
    InvalidFunctionKind { tag: u8 },
    /// Decoded program failed normal bytecode verification.
    InvalidBytecode(crate::bytecode::VerificationError),
    /// A length field could not be represented on this platform.
    LengthTooLarge { field: &'static str },
    /// Extra bytes remained after decoding a complete program.
    TrailingBytes { count: usize },
}

/// Encodes a bytecode program to bytes.
pub fn encode_program(program: &Program) -> Result<Vec<u8>, BytecodeEncodeError> {
    let mut encoder = Encoder { bytes: Vec::new() };
    encoder.bytes.extend_from_slice(BYTECODE_MAGIC.as_bytes());
    encoder.u16(FORMAT_VERSION);
    encoder.u16(program.format.version);
    encoder.u32(program.format.feature_flags);
    encoder.u16(program.entry.0);
    encoder.len("functions", program.functions.len())?;
    for function in &program.functions {
        encoder.function(function)?;
    }
    Ok(encoder.bytes)
}

/// Encodes a program into a metadata-rich bytecode container.
pub fn encode_container(
    program: &Program,
    metadata: Option<BytecodeContainerMetadata>,
) -> Result<Vec<u8>, BytecodeEncodeError> {
    let payload = encode_program(program)?;
    let mut metadata = metadata.unwrap_or_else(|| BytecodeContainerMetadata::for_program(program));
    metadata.bytecode_format_version = program.format.version;
    metadata.feature_flags |= infer_program_feature_flags(program);
    metadata.entry = program.entry;
    metadata.debug_section_present |= program_has_debug_metadata(program);
    metadata.checksum = checksum64(&payload);

    let mut encoder = Encoder { bytes: Vec::new() };
    encoder
        .bytes
        .extend_from_slice(BYTECODE_CONTAINER_MAGIC.as_bytes());
    encoder.u16(CURRENT_CONTAINER_VERSION);
    encoder.container_metadata(&metadata)?;
    encoder.len("sections", 1)?;
    encoder.u8(BytecodeSectionKind::ProgramPayload.tag());
    encoder.len("section.program_payload", payload.len())?;
    encoder.bytes.extend_from_slice(&payload);
    Ok(encoder.bytes)
}

/// Decodes bytes into a verified bytecode program.
pub fn decode_program(bytes: &[u8]) -> Result<VerifiedProgram, BytecodeDecodeError> {
    let mut decoder = Decoder { bytes, cursor: 0 };
    decoder.magic()?;
    let serialization_version = decoder.u16()?;
    if serialization_version != FORMAT_VERSION {
        return Err(BytecodeDecodeError::UnsupportedVersion {
            version: serialization_version,
        });
    }
    let bytecode_version = decoder.u16()?;
    let feature_flags = decoder.u32()?;
    let entry = FunctionId(decoder.u16()?);
    let function_count = decoder.len("functions")?;
    let mut program = Program::new(entry).with_format(BytecodeFormat {
        magic: BYTECODE_MAGIC,
        version: bytecode_version,
        feature_flags,
    });
    for _ in 0..function_count {
        program
            .add_function(decoder.function()?)
            .map_err(|_| BytecodeDecodeError::LengthTooLarge { field: "functions" })?;
    }
    if decoder.remaining() != 0 {
        return Err(BytecodeDecodeError::TrailingBytes {
            count: decoder.remaining(),
        });
    }
    VerifiedProgram::new(program).map_err(BytecodeDecodeError::InvalidBytecode)
}

/// Decodes either a legacy program payload or a bytecode container.
pub fn decode_bytecode(bytes: &[u8]) -> Result<VerifiedProgram, BytecodeDecodeError> {
    if bytes.starts_with(BYTECODE_CONTAINER_MAGIC.as_bytes()) {
        decode_container(bytes).map(|container| container.program)
    } else {
        decode_program(bytes)
    }
}

/// Decodes a metadata-rich bytecode container.
pub fn decode_container(bytes: &[u8]) -> Result<BytecodeContainer, BytecodeDecodeError> {
    let mut decoder = Decoder { bytes, cursor: 0 };
    decoder.container_magic()?;
    let version = decoder.u16()?;
    if version != CURRENT_CONTAINER_VERSION {
        return Err(BytecodeDecodeError::UnsupportedContainerVersion { version });
    }
    let metadata = decoder.container_metadata()?;
    let section_count = decoder.len("sections")?;
    let mut sections = Vec::with_capacity(section_count);
    let mut program_payload = None;
    for _ in 0..section_count {
        let kind = BytecodeSectionKind::from_tag(decoder.u8()?)?;
        let len = decoder.len("section")?;
        let offset = decoder.cursor;
        let payload = decoder.take(len)?;
        if kind == BytecodeSectionKind::ProgramPayload {
            program_payload = Some(payload.to_vec());
        }
        sections.push(BytecodeSectionEntry { kind, offset, len });
    }
    if decoder.remaining() != 0 {
        return Err(BytecodeDecodeError::TrailingBytes {
            count: decoder.remaining(),
        });
    }
    let payload = program_payload.ok_or(BytecodeDecodeError::MissingProgramSection)?;
    let actual = checksum64(&payload);
    if metadata.checksum != 0 && metadata.checksum != actual {
        return Err(BytecodeDecodeError::ChecksumMismatch {
            expected: metadata.checksum,
            actual,
        });
    }
    let program = decode_program(&payload)?;
    Ok(BytecodeContainer {
        metadata,
        sections,
        program,
    })
}

/// Reads bytecode container metadata without returning the verified payload.
pub fn inspect_container(bytes: &[u8]) -> Result<BytecodeContainerMetadata, BytecodeDecodeError> {
    decode_container(bytes).map(|container| container.metadata)
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn container_metadata(
        &mut self,
        metadata: &BytecodeContainerMetadata,
    ) -> Result<(), BytecodeEncodeError> {
        self.u16(metadata.bytecode_format_version);
        self.string("metadata.min_ferrix_version", &metadata.min_ferrix_version)?;
        self.u32(metadata.feature_flags);
        self.len(
            "metadata.required_capabilities",
            metadata.required_capabilities.len(),
        )?;
        for capability in &metadata.required_capabilities {
            self.string("metadata.required_capability", capability)?;
        }
        self.u16(metadata.entry.0);
        self.option_string("metadata.module_name", metadata.module_name.as_deref())?;
        self.u8(u8::from(metadata.debug_section_present));
        self.u64(metadata.checksum);
        self.u8(metadata.optimization_level);
        self.u8(u8::from(metadata.import_table_present));
        self.u8(u8::from(metadata.export_table_present));
        self.u8(u8::from(metadata.interface_metadata_present));
        Ok(())
    }

    fn function(&mut self, function: &Function) -> Result<(), BytecodeEncodeError> {
        self.string("function.name", &function.name)?;
        self.u8(function.arity);
        self.u8(function.register_count);
        self.u8(function.capture_count);
        match &function.kind {
            FunctionKind::Bytecode(chunk) => {
                self.u8(0);
                self.chunk(chunk)?;
            }
            FunctionKind::Native { name } => {
                self.u8(1);
                self.string("native.name", name)?;
            }
        }
        Ok(())
    }

    fn chunk(&mut self, chunk: &Chunk) -> Result<(), BytecodeEncodeError> {
        self.string("chunk.name", &chunk.name)?;
        self.u8(chunk.arity);
        self.u8(chunk.register_count);
        self.u8(chunk.capture_count);

        self.len("constants", chunk.constants.len())?;
        for value in &chunk.constants {
            self.value(*value);
        }

        self.len("strings", chunk.strings.len())?;
        for value in &chunk.strings {
            self.string("string", value)?;
        }

        self.len("instructions", chunk.instructions.len())?;
        for instruction in &chunk.instructions {
            self.instruction(instruction);
        }

        self.len("source_map", chunk.source_map.len())?;
        for span in &chunk.source_map {
            self.option_span(*span)?;
        }

        self.len("debug_local_names", chunk.debug_local_names.len())?;
        for name in &chunk.debug_local_names {
            self.option_string("debug_local_name", name.as_deref())?;
        }

        Ok(())
    }

    fn value(&mut self, value: Value) {
        match value {
            Value::Nil => self.u8(0),
            Value::Bool(value) => {
                self.u8(1);
                self.u8(u8::from(value));
            }
            Value::Int(value) => {
                self.u8(2);
                self.i64(value);
            }
            Value::Float(value) => {
                self.u8(3);
                self.u64(value.to_bits());
            }
            Value::Obj(reference) => {
                self.u8(4);
                self.u32(reference.index);
                self.u32(reference.generation);
            }
        }
    }

    fn instruction(&mut self, instruction: &Instruction) {
        match instruction {
            Instruction::LoadConst { dst, constant } => {
                self.u8(0);
                self.reg(*dst);
                self.u16(constant.0);
            }
            Instruction::LoadString { dst, string } => {
                self.u8(1);
                self.reg(*dst);
                self.u16(string.0);
            }
            Instruction::Move { dst, src } => {
                self.u8(2);
                self.reg(*dst);
                self.reg(*src);
            }
            Instruction::Add { dst, lhs, rhs } => self.three_reg(3, *dst, *lhs, *rhs),
            Instruction::Sub { dst, lhs, rhs } => self.three_reg(4, *dst, *lhs, *rhs),
            Instruction::Mul { dst, lhs, rhs } => self.three_reg(5, *dst, *lhs, *rhs),
            Instruction::Div { dst, lhs, rhs } => self.three_reg(6, *dst, *lhs, *rhs),
            Instruction::AddInt { dst, lhs, rhs } => self.three_reg(39, *dst, *lhs, *rhs),
            Instruction::SubInt { dst, lhs, rhs } => self.three_reg(40, *dst, *lhs, *rhs),
            Instruction::MulInt { dst, lhs, rhs } => self.three_reg(41, *dst, *lhs, *rhs),
            Instruction::DivInt { dst, lhs, rhs } => self.three_reg(42, *dst, *lhs, *rhs),
            Instruction::Jump { target } => {
                self.u8(7);
                self.u32(target.0);
            }
            Instruction::JumpIfFalse { condition, target } => {
                self.u8(8);
                self.reg(*condition);
                self.u32(target.0);
            }
            Instruction::JumpIfTrue { condition, target } => {
                self.u8(9);
                self.reg(*condition);
                self.u32(target.0);
            }
            Instruction::Equal { dst, lhs, rhs } => self.three_reg(10, *dst, *lhs, *rhs),
            Instruction::NotEqual { dst, lhs, rhs } => self.three_reg(11, *dst, *lhs, *rhs),
            Instruction::Less { dst, lhs, rhs } => self.three_reg(12, *dst, *lhs, *rhs),
            Instruction::LessEqual { dst, lhs, rhs } => self.three_reg(13, *dst, *lhs, *rhs),
            Instruction::Greater { dst, lhs, rhs } => self.three_reg(14, *dst, *lhs, *rhs),
            Instruction::GreaterEqual { dst, lhs, rhs } => self.three_reg(15, *dst, *lhs, *rhs),
            Instruction::LessInt { dst, lhs, rhs } => self.three_reg(43, *dst, *lhs, *rhs),
            Instruction::LessEqualInt { dst, lhs, rhs } => self.three_reg(44, *dst, *lhs, *rhs),
            Instruction::GreaterInt { dst, lhs, rhs } => self.three_reg(45, *dst, *lhs, *rhs),
            Instruction::GreaterEqualInt { dst, lhs, rhs } => {
                self.three_reg(46, *dst, *lhs, *rhs);
            }
            Instruction::Not { dst, src } => {
                self.u8(16);
                self.reg(*dst);
                self.reg(*src);
            }
            Instruction::CallFunction {
                dst,
                function,
                args_start,
                arg_count,
            } => {
                self.u8(17);
                self.reg(*dst);
                self.u16(function.0);
                self.reg(*args_start);
                self.u8(*arg_count);
            }
            Instruction::MakeUpvalue { dst, src } => {
                self.u8(28);
                self.reg(*dst);
                self.reg(*src);
            }
            Instruction::LoadUpvalue { dst, upvalue } => {
                self.u8(29);
                self.reg(*dst);
                self.reg(*upvalue);
            }
            Instruction::StoreUpvalue { upvalue, src } => {
                self.u8(30);
                self.reg(*upvalue);
                self.reg(*src);
            }
            Instruction::MakeClosure {
                dst,
                function,
                captures_start,
                capture_count,
            } => {
                self.u8(25);
                self.reg(*dst);
                self.u16(function.0);
                self.reg(*captures_start);
                self.u8(*capture_count);
            }
            Instruction::LoadCapture { dst, capture } => {
                self.u8(26);
                self.reg(*dst);
                self.u8(capture.0);
            }
            Instruction::LoadCaptureCell { dst, capture } => {
                self.u8(31);
                self.reg(*dst);
                self.u8(capture.0);
            }
            Instruction::StoreCapture { capture, src } => {
                self.u8(32);
                self.u8(capture.0);
                self.reg(*src);
            }
            Instruction::CallValue {
                dst,
                callee,
                args_start,
                arg_count,
            } => {
                self.u8(27);
                self.reg(*dst);
                self.reg(*callee);
                self.reg(*args_start);
                self.u8(*arg_count);
            }
            Instruction::CallExtension {
                dst,
                extension,
                args_start,
                arg_count,
            } => {
                self.u8(47);
                self.reg(*dst);
                self.u16(extension.0);
                self.reg(*args_start);
                self.u8(*arg_count);
            }
            Instruction::ArrayNew {
                dst,
                elements_start,
                element_count,
            } => {
                self.u8(18);
                self.reg(*dst);
                self.reg(*elements_start);
                self.u8(*element_count);
            }
            Instruction::MapNew {
                dst,
                entries_start,
                entry_count,
            } => {
                self.u8(19);
                self.reg(*dst);
                self.reg(*entries_start);
                self.u8(*entry_count);
            }
            Instruction::RecordNew {
                dst,
                fields_start,
                fields,
            } => {
                self.u8(36);
                self.reg(*dst);
                self.reg(*fields_start);
                self.u8(fields.len().try_into().unwrap_or(u8::MAX));
                for field in fields {
                    self.u16(field.0);
                }
            }
            Instruction::IndexGet { dst, target, index } => {
                self.three_reg(20, *dst, *target, *index)
            }
            Instruction::IndexSet {
                target,
                index,
                value,
            } => self.three_reg(21, *target, *index, *value),
            Instruction::ArrayGet { dst, array, index } => self.three_reg(22, *dst, *array, *index),
            Instruction::ArraySet {
                array,
                index,
                value,
            } => self.three_reg(23, *array, *index, *value),
            Instruction::FieldGet { dst, target, field } => {
                self.u8(37);
                self.reg(*dst);
                self.reg(*target);
                self.u16(field.0);
            }
            Instruction::FieldSet {
                target,
                field,
                value,
            } => {
                self.u8(38);
                self.reg(*target);
                self.u16(field.0);
                self.reg(*value);
            }
            Instruction::PushHandler { error, target } => {
                self.u8(33);
                self.reg(*error);
                self.u32(target.0);
            }
            Instruction::PopHandler => {
                self.u8(34);
            }
            Instruction::Throw { src } => {
                self.u8(35);
                self.reg(*src);
            }
            Instruction::Return { src } => {
                self.u8(24);
                self.reg(*src);
            }
        }
    }

    fn three_reg(&mut self, opcode: u8, first: Register, second: Register, third: Register) {
        self.u8(opcode);
        self.reg(first);
        self.reg(second);
        self.reg(third);
    }

    fn option_span(&mut self, span: Option<SourceSpan>) -> Result<(), BytecodeEncodeError> {
        match span {
            Some(span) => {
                self.u8(1);
                self.u32(span.file_id.0);
                self.usize("span.start", span.start)?;
                self.usize("span.end", span.end)?;
            }
            None => self.u8(0),
        }
        Ok(())
    }

    fn option_string(
        &mut self,
        field: &'static str,
        value: Option<&str>,
    ) -> Result<(), BytecodeEncodeError> {
        match value {
            Some(value) => {
                self.u8(1);
                self.string(field, value)?;
            }
            None => self.u8(0),
        }
        Ok(())
    }

    fn string(&mut self, field: &'static str, value: &str) -> Result<(), BytecodeEncodeError> {
        self.len(field, value.len())?;
        self.bytes.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn len(&mut self, field: &'static str, len: usize) -> Result<(), BytecodeEncodeError> {
        self.u32(
            len.try_into()
                .map_err(|_| BytecodeEncodeError::LengthTooLarge { field })?,
        );
        Ok(())
    }

    fn usize(&mut self, field: &'static str, value: usize) -> Result<(), BytecodeEncodeError> {
        self.u64(
            value
                .try_into()
                .map_err(|_| BytecodeEncodeError::LengthTooLarge { field })?,
        );
        Ok(())
    }

    fn reg(&mut self, register: Register) {
        self.u8(register.0);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl Decoder<'_> {
    fn magic(&mut self) -> Result<(), BytecodeDecodeError> {
        let magic = self.take(BYTECODE_MAGIC.len())?;
        if magic == BYTECODE_MAGIC.as_bytes() {
            Ok(())
        } else {
            Err(BytecodeDecodeError::InvalidMagic)
        }
    }

    fn container_magic(&mut self) -> Result<(), BytecodeDecodeError> {
        let magic = self.take(BYTECODE_CONTAINER_MAGIC.len())?;
        if magic == BYTECODE_CONTAINER_MAGIC.as_bytes() {
            Ok(())
        } else {
            Err(BytecodeDecodeError::InvalidMagic)
        }
    }

    fn container_metadata(&mut self) -> Result<BytecodeContainerMetadata, BytecodeDecodeError> {
        let bytecode_format_version = self.u16()?;
        let min_ferrix_version = self.string()?;
        let feature_flags = self.u32()?;
        let capability_count = self.len("metadata.required_capabilities")?;
        let mut required_capabilities = Vec::with_capacity(capability_count);
        for _ in 0..capability_count {
            required_capabilities.push(self.string()?);
        }
        let entry = FunctionId(self.u16()?);
        let module_name = self.option_string()?;
        let debug_section_present = self.u8()? != 0;
        let checksum = self.u64()?;
        let optimization_level = self.u8()?;
        let import_table_present = self.u8()? != 0;
        let export_table_present = self.u8()? != 0;
        let interface_metadata_present = self.u8()? != 0;

        Ok(BytecodeContainerMetadata {
            bytecode_format_version,
            min_ferrix_version,
            feature_flags,
            required_capabilities,
            entry,
            module_name,
            debug_section_present,
            checksum,
            optimization_level,
            import_table_present,
            export_table_present,
            interface_metadata_present,
        })
    }

    fn function(&mut self) -> Result<Function, BytecodeDecodeError> {
        let name = self.string()?;
        let arity = self.u8()?;
        let register_count = self.u8()?;
        let capture_count = self.u8()?;
        match self.u8()? {
            0 => {
                let chunk = self.chunk()?;
                Ok(Function::bytecode(chunk))
            }
            1 => {
                let native_name = self.string()?;
                Ok(Function::native(native_name, arity))
            }
            tag => Err(BytecodeDecodeError::InvalidFunctionKind { tag }),
        }
        .map(|mut function| {
            function.name = name;
            function.arity = arity;
            function.register_count = register_count;
            function.capture_count = capture_count;
            function
        })
    }

    fn chunk(&mut self) -> Result<Chunk, BytecodeDecodeError> {
        let name = self.string()?;
        let arity = self.u8()?;
        let register_count = self.u8()?;
        let capture_count = self.u8()?;
        let mut chunk = Chunk::new(name, register_count)
            .with_arity(arity)
            .with_capture_count(capture_count);

        let constant_count = self.len("constants")?;
        for _ in 0..constant_count {
            chunk.constants.push(self.value()?);
        }

        let string_count = self.len("strings")?;
        for _ in 0..string_count {
            chunk.strings.push(self.string()?);
        }

        let instruction_count = self.len("instructions")?;
        for _ in 0..instruction_count {
            chunk.instructions.push(self.instruction()?);
        }

        let source_map_count = self.len("source_map")?;
        for _ in 0..source_map_count {
            chunk.source_map.push(self.option_span()?);
        }

        let debug_local_count = self.len("debug_local_names")?;
        for _ in 0..debug_local_count {
            chunk.debug_local_names.push(self.option_string()?);
        }

        Ok(chunk)
    }

    fn value(&mut self) -> Result<Value, BytecodeDecodeError> {
        match self.u8()? {
            0 => Ok(Value::Nil),
            1 => Ok(Value::Bool(self.u8()? != 0)),
            2 => Ok(Value::Int(self.i64()?)),
            3 => Ok(Value::Float(f64::from_bits(self.u64()?))),
            4 => Ok(Value::Obj(ObjRef::new(self.u32()?, self.u32()?))),
            tag => Err(BytecodeDecodeError::InvalidValueTag { tag }),
        }
    }

    fn instruction(&mut self) -> Result<Instruction, BytecodeDecodeError> {
        Ok(match self.u8()? {
            0 => Instruction::LoadConst {
                dst: self.reg()?,
                constant: crate::bytecode::ConstId(self.u16()?),
            },
            1 => Instruction::LoadString {
                dst: self.reg()?,
                string: crate::bytecode::StringId(self.u16()?),
            },
            2 => Instruction::Move {
                dst: self.reg()?,
                src: self.reg()?,
            },
            3 => self.three_reg(|dst, lhs, rhs| Instruction::Add { dst, lhs, rhs })?,
            4 => self.three_reg(|dst, lhs, rhs| Instruction::Sub { dst, lhs, rhs })?,
            5 => self.three_reg(|dst, lhs, rhs| Instruction::Mul { dst, lhs, rhs })?,
            6 => self.three_reg(|dst, lhs, rhs| Instruction::Div { dst, lhs, rhs })?,
            39 => self.three_reg(|dst, lhs, rhs| Instruction::AddInt { dst, lhs, rhs })?,
            40 => self.three_reg(|dst, lhs, rhs| Instruction::SubInt { dst, lhs, rhs })?,
            41 => self.three_reg(|dst, lhs, rhs| Instruction::MulInt { dst, lhs, rhs })?,
            42 => self.three_reg(|dst, lhs, rhs| Instruction::DivInt { dst, lhs, rhs })?,
            7 => Instruction::Jump {
                target: JumpTarget(self.u32()?),
            },
            8 => Instruction::JumpIfFalse {
                condition: self.reg()?,
                target: JumpTarget(self.u32()?),
            },
            9 => Instruction::JumpIfTrue {
                condition: self.reg()?,
                target: JumpTarget(self.u32()?),
            },
            10 => self.three_reg(|dst, lhs, rhs| Instruction::Equal { dst, lhs, rhs })?,
            11 => self.three_reg(|dst, lhs, rhs| Instruction::NotEqual { dst, lhs, rhs })?,
            12 => self.three_reg(|dst, lhs, rhs| Instruction::Less { dst, lhs, rhs })?,
            13 => self.three_reg(|dst, lhs, rhs| Instruction::LessEqual { dst, lhs, rhs })?,
            14 => self.three_reg(|dst, lhs, rhs| Instruction::Greater { dst, lhs, rhs })?,
            15 => self.three_reg(|dst, lhs, rhs| Instruction::GreaterEqual { dst, lhs, rhs })?,
            43 => self.three_reg(|dst, lhs, rhs| Instruction::LessInt { dst, lhs, rhs })?,
            44 => self.three_reg(|dst, lhs, rhs| Instruction::LessEqualInt { dst, lhs, rhs })?,
            45 => self.three_reg(|dst, lhs, rhs| Instruction::GreaterInt { dst, lhs, rhs })?,
            46 => self.three_reg(|dst, lhs, rhs| Instruction::GreaterEqualInt { dst, lhs, rhs })?,
            16 => Instruction::Not {
                dst: self.reg()?,
                src: self.reg()?,
            },
            17 => Instruction::CallFunction {
                dst: self.reg()?,
                function: FunctionId(self.u16()?),
                args_start: self.reg()?,
                arg_count: self.u8()?,
            },
            28 => Instruction::MakeUpvalue {
                dst: self.reg()?,
                src: self.reg()?,
            },
            29 => Instruction::LoadUpvalue {
                dst: self.reg()?,
                upvalue: self.reg()?,
            },
            30 => Instruction::StoreUpvalue {
                upvalue: self.reg()?,
                src: self.reg()?,
            },
            25 => Instruction::MakeClosure {
                dst: self.reg()?,
                function: FunctionId(self.u16()?),
                captures_start: self.reg()?,
                capture_count: self.u8()?,
            },
            26 => Instruction::LoadCapture {
                dst: self.reg()?,
                capture: crate::bytecode::CaptureId(self.u8()?),
            },
            31 => Instruction::LoadCaptureCell {
                dst: self.reg()?,
                capture: crate::bytecode::CaptureId(self.u8()?),
            },
            32 => Instruction::StoreCapture {
                capture: crate::bytecode::CaptureId(self.u8()?),
                src: self.reg()?,
            },
            27 => Instruction::CallValue {
                dst: self.reg()?,
                callee: self.reg()?,
                args_start: self.reg()?,
                arg_count: self.u8()?,
            },
            47 => Instruction::CallExtension {
                dst: self.reg()?,
                extension: crate::bytecode::StringId(self.u16()?),
                args_start: self.reg()?,
                arg_count: self.u8()?,
            },
            18 => Instruction::ArrayNew {
                dst: self.reg()?,
                elements_start: self.reg()?,
                element_count: self.u8()?,
            },
            19 => Instruction::MapNew {
                dst: self.reg()?,
                entries_start: self.reg()?,
                entry_count: self.u8()?,
            },
            36 => {
                let dst = self.reg()?;
                let fields_start = self.reg()?;
                let field_count = self.u8()?;
                let mut fields = Vec::with_capacity(usize::from(field_count));
                for _ in 0..field_count {
                    fields.push(crate::bytecode::StringId(self.u16()?));
                }
                Instruction::RecordNew {
                    dst,
                    fields_start,
                    fields,
                }
            }
            20 => {
                self.three_reg(|dst, target, index| Instruction::IndexGet { dst, target, index })?
            }
            21 => {
                let target = self.reg()?;
                let index = self.reg()?;
                let value = self.reg()?;
                Instruction::IndexSet {
                    target,
                    index,
                    value,
                }
            }
            22 => {
                let dst = self.reg()?;
                let array = self.reg()?;
                let index = self.reg()?;
                Instruction::ArrayGet { dst, array, index }
            }
            23 => {
                let array = self.reg()?;
                let index = self.reg()?;
                let value = self.reg()?;
                Instruction::ArraySet {
                    array,
                    index,
                    value,
                }
            }
            37 => Instruction::FieldGet {
                dst: self.reg()?,
                target: self.reg()?,
                field: crate::bytecode::StringId(self.u16()?),
            },
            38 => Instruction::FieldSet {
                target: self.reg()?,
                field: crate::bytecode::StringId(self.u16()?),
                value: self.reg()?,
            },
            33 => Instruction::PushHandler {
                error: self.reg()?,
                target: JumpTarget(self.u32()?),
            },
            34 => Instruction::PopHandler,
            35 => Instruction::Throw { src: self.reg()? },
            24 => Instruction::Return { src: self.reg()? },
            opcode => return Err(BytecodeDecodeError::InvalidInstructionOpcode { opcode }),
        })
    }

    fn three_reg(
        &mut self,
        build: impl FnOnce(Register, Register, Register) -> Instruction,
    ) -> Result<Instruction, BytecodeDecodeError> {
        Ok(build(self.reg()?, self.reg()?, self.reg()?))
    }

    fn option_span(&mut self) -> Result<Option<SourceSpan>, BytecodeDecodeError> {
        match self.u8()? {
            0 => Ok(None),
            _ => Ok(Some(SourceSpan::new(
                FileId(self.u32()?),
                self.usize("span.start")?,
                self.usize("span.end")?,
            ))),
        }
    }

    fn option_string(&mut self) -> Result<Option<String>, BytecodeDecodeError> {
        match self.u8()? {
            0 => Ok(None),
            _ => Ok(Some(self.string()?)),
        }
    }

    fn string(&mut self) -> Result<String, BytecodeDecodeError> {
        let len = self.len("string")?;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| BytecodeDecodeError::InvalidUtf8)
    }

    fn len(&mut self, field: &'static str) -> Result<usize, BytecodeDecodeError> {
        usize::try_from(self.u32()?).map_err(|_| BytecodeDecodeError::LengthTooLarge { field })
    }

    fn usize(&mut self, field: &'static str) -> Result<usize, BytecodeDecodeError> {
        usize::try_from(self.u64()?).map_err(|_| BytecodeDecodeError::LengthTooLarge { field })
    }

    fn reg(&mut self) -> Result<Register, BytecodeDecodeError> {
        Ok(Register(self.u8()?))
    }

    fn u8(&mut self) -> Result<u8, BytecodeDecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, BytecodeDecodeError> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, BytecodeDecodeError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, BytecodeDecodeError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn i64(&mut self) -> Result<i64, BytecodeDecodeError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(i64::from_le_bytes(bytes))
    }

    fn take(&mut self, len: usize) -> Result<&[u8], BytecodeDecodeError> {
        let end = self
            .cursor
            .checked_add(len)
            .ok_or(BytecodeDecodeError::UnexpectedEof)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(BytecodeDecodeError::UnexpectedEof)?;
        self.cursor = end;
        Ok(bytes)
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.cursor)
    }
}

impl fmt::Display for BytecodeEncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthTooLarge { field } => write!(f, "`{field}` is too large to encode"),
        }
    }
}

impl fmt::Display for BytecodeDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => f.write_str("unexpected end of bytecode"),
            Self::InvalidMagic => f.write_str("invalid bytecode magic"),
            Self::UnsupportedVersion { version } => {
                write!(f, "unsupported bytecode serialization version {version}")
            }
            Self::UnsupportedContainerVersion { version } => {
                write!(f, "unsupported bytecode container version {version}")
            }
            Self::InvalidSectionKind { tag } => write!(f, "invalid bytecode section kind {tag}"),
            Self::MissingProgramSection => f.write_str("bytecode container has no program section"),
            Self::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "bytecode container checksum mismatch: expected {expected}, got {actual}"
                )
            }
            Self::InvalidUtf8 => f.write_str("invalid utf-8 in bytecode"),
            Self::InvalidValueTag { tag } => write!(f, "invalid value tag {tag}"),
            Self::InvalidInstructionOpcode { opcode } => {
                write!(f, "invalid instruction opcode {opcode}")
            }
            Self::InvalidFunctionKind { tag } => write!(f, "invalid function kind {tag}"),
            Self::InvalidBytecode(error) => write!(f, "{error}"),
            Self::LengthTooLarge { field } => write!(f, "`{field}` is too large to decode"),
            Self::TrailingBytes { count } => write!(f, "bytecode has {count} trailing bytes"),
        }
    }
}

impl Error for BytecodeEncodeError {}

impl Error for BytecodeDecodeError {}

fn program_has_debug_metadata(program: &Program) -> bool {
    program.functions.iter().any(|function| {
        function.chunk().is_some_and(|chunk| {
            chunk.source_map.iter().any(Option::is_some)
                || chunk.debug_local_names.iter().any(Option::is_some)
        })
    })
}

fn checksum64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
