//! Instruction and typed operand definitions for Ferrix bytecode.

use std::fmt;

/// Register operand index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Register(pub u8);

/// Constant-pool operand index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstId(pub u16);

/// String-pool operand index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StringId(pub u16);

/// Function-table operand index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(pub u16);

/// Absolute instruction target used by jump instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JumpTarget(pub u32);

/// Captured value operand index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CaptureId(pub u8);

/// Register VM instruction set with explicit typed operands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    LoadConst {
        dst: Register,
        constant: ConstId,
    },
    LoadString {
        dst: Register,
        string: StringId,
    },
    Move {
        dst: Register,
        src: Register,
    },
    Add {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    AddInt {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Sub {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    SubInt {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Mul {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    MulInt {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Div {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    DivInt {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Jump {
        target: JumpTarget,
    },
    JumpIfFalse {
        condition: Register,
        target: JumpTarget,
    },
    JumpIfTrue {
        condition: Register,
        target: JumpTarget,
    },
    Equal {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    NotEqual {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Less {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    LessInt {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    LessEqual {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    LessEqualInt {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Greater {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    GreaterInt {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    GreaterEqual {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    GreaterEqualInt {
        dst: Register,
        lhs: Register,
        rhs: Register,
    },
    Not {
        dst: Register,
        src: Register,
    },
    CallFunction {
        dst: Register,
        function: FunctionId,
        args_start: Register,
        arg_count: u8,
    },
    MakeUpvalue {
        dst: Register,
        src: Register,
    },
    LoadUpvalue {
        dst: Register,
        upvalue: Register,
    },
    StoreUpvalue {
        upvalue: Register,
        src: Register,
    },
    MakeClosure {
        dst: Register,
        function: FunctionId,
        captures_start: Register,
        capture_count: u8,
    },
    LoadCapture {
        dst: Register,
        capture: CaptureId,
    },
    LoadCaptureCell {
        dst: Register,
        capture: CaptureId,
    },
    StoreCapture {
        capture: CaptureId,
        src: Register,
    },
    CallValue {
        dst: Register,
        callee: Register,
        args_start: Register,
        arg_count: u8,
    },
    ArrayNew {
        dst: Register,
        elements_start: Register,
        element_count: u8,
    },
    MapNew {
        dst: Register,
        entries_start: Register,
        entry_count: u8,
    },
    RecordNew {
        dst: Register,
        fields_start: Register,
        fields: Vec<StringId>,
    },
    IndexGet {
        dst: Register,
        target: Register,
        index: Register,
    },
    IndexSet {
        target: Register,
        index: Register,
        value: Register,
    },
    ArrayGet {
        dst: Register,
        array: Register,
        index: Register,
    },
    ArraySet {
        array: Register,
        index: Register,
        value: Register,
    },
    FieldGet {
        dst: Register,
        target: Register,
        field: StringId,
    },
    FieldSet {
        target: Register,
        field: StringId,
        value: Register,
    },
    PushHandler {
        error: Register,
        target: JumpTarget,
    },
    PopHandler,
    Throw {
        src: Register,
    },
    Return {
        src: Register,
    },
}

impl Instruction {
    /// Returns every register mentioned by this instruction.
    ///
    /// Used by the verifier to check register bounds.
    pub fn register_operands(&self) -> Vec<Register> {
        match self {
            Self::LoadConst { dst, .. } | Self::LoadString { dst, .. } => vec![*dst],
            Self::Move { dst, src } => vec![*dst, *src],
            Self::Add { dst, lhs, rhs }
            | Self::AddInt { dst, lhs, rhs }
            | Self::Sub { dst, lhs, rhs }
            | Self::SubInt { dst, lhs, rhs }
            | Self::Mul { dst, lhs, rhs }
            | Self::MulInt { dst, lhs, rhs }
            | Self::Div { dst, lhs, rhs }
            | Self::DivInt { dst, lhs, rhs }
            | Self::Equal { dst, lhs, rhs }
            | Self::NotEqual { dst, lhs, rhs }
            | Self::Less { dst, lhs, rhs }
            | Self::LessInt { dst, lhs, rhs }
            | Self::LessEqual { dst, lhs, rhs }
            | Self::LessEqualInt { dst, lhs, rhs }
            | Self::Greater { dst, lhs, rhs }
            | Self::GreaterInt { dst, lhs, rhs }
            | Self::GreaterEqual { dst, lhs, rhs }
            | Self::GreaterEqualInt { dst, lhs, rhs } => vec![*dst, *lhs, *rhs],
            Self::Jump { .. } => vec![],
            Self::JumpIfFalse { condition, .. } | Self::JumpIfTrue { condition, .. } => {
                vec![*condition]
            }
            Self::Not { dst, src } => vec![*dst, *src],
            Self::CallFunction {
                dst, args_start, ..
            } => vec![*dst, *args_start],
            Self::MakeUpvalue { dst, src }
            | Self::LoadUpvalue { dst, upvalue: src }
            | Self::StoreUpvalue { upvalue: dst, src } => vec![*dst, *src],
            Self::MakeClosure {
                dst,
                captures_start,
                ..
            } => vec![*dst, *captures_start],
            Self::LoadCapture { dst, .. } => vec![*dst],
            Self::LoadCaptureCell { dst, .. } => vec![*dst],
            Self::StoreCapture { src, .. } => vec![*src],
            Self::CallValue {
                dst,
                callee,
                args_start,
                ..
            } => vec![*dst, *callee, *args_start],
            Self::ArrayNew {
                dst,
                elements_start,
                ..
            } => vec![*dst, *elements_start],
            Self::MapNew {
                dst, entries_start, ..
            } => vec![*dst, *entries_start],
            Self::RecordNew {
                dst, fields_start, ..
            } => vec![*dst, *fields_start],
            Self::IndexGet { dst, target, index } => vec![*dst, *target, *index],
            Self::IndexSet {
                target,
                index,
                value,
            } => vec![*target, *index, *value],
            Self::ArrayGet { dst, array, index } => vec![*dst, *array, *index],
            Self::ArraySet {
                array,
                index,
                value,
            } => vec![*array, *index, *value],
            Self::FieldGet { dst, target, .. } => vec![*dst, *target],
            Self::FieldSet { target, value, .. } => vec![*target, *value],
            Self::PushHandler { error, .. } => vec![*error],
            Self::PopHandler => vec![],
            Self::Throw { src } => vec![*src],
            Self::Return { src } => vec![*src],
        }
    }

    /// Returns the constant operand, if the instruction has one.
    pub fn const_operand(&self) -> Option<ConstId> {
        match self {
            Self::LoadConst { constant, .. } => Some(*constant),
            _ => None,
        }
    }

    /// Returns every string-pool operand mentioned by this instruction.
    ///
    /// Used by the verifier to check string table bounds.
    pub fn string_operands(&self) -> Vec<StringId> {
        match self {
            Self::LoadString { string, .. } => vec![*string],
            Self::RecordNew { fields, .. } => fields.clone(),
            Self::FieldGet { field, .. } | Self::FieldSet { field, .. } => vec![*field],
            _ => Vec::new(),
        }
    }

    /// Returns the jump target, if the instruction has one.
    pub fn jump_operand(&self) -> Option<JumpTarget> {
        match self {
            Self::Jump { target }
            | Self::JumpIfFalse { target, .. }
            | Self::JumpIfTrue { target, .. }
            | Self::PushHandler { target, .. } => Some(*target),
            _ => None,
        }
    }

    /// Returns the function operand, if the instruction has one.
    pub fn function_operand(&self) -> Option<FunctionId> {
        match self {
            Self::CallFunction { function, .. } => Some(*function),
            Self::MakeClosure { function, .. } => Some(*function),
            _ => None,
        }
    }

    /// Returns the capture operand, if the instruction has one.
    pub fn capture_operand(&self) -> Option<CaptureId> {
        match self {
            Self::LoadCapture { capture, .. } => Some(*capture),
            Self::LoadCaptureCell { capture, .. } => Some(*capture),
            Self::StoreCapture { capture, .. } => Some(*capture),
            _ => None,
        }
    }
}

impl fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

impl fmt::Display for ConstId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl fmt::Display for StringId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "str#{}", self.0)
    }
}

impl fmt::Display for FunctionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn#{}", self.0)
    }
}

impl fmt::Display for CaptureId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cap#{}", self.0)
    }
}

impl fmt::Display for JumpTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.0)
    }
}
