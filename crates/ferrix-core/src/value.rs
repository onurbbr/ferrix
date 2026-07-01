//! Runtime value model shared by the compiler, VM, stdlib, and tests.
//!
//! `Value` is intentionally compact and copyable. Heap-backed data such as
//! strings, arrays, and maps are represented through `ObjRef`.

use std::fmt;

use crate::ObjRef;

/// A value that can live in VM registers, constants, arrays, maps, and native calls.
#[derive(Clone, Copy, Default)]
pub enum Value {
    /// Absence of a value; used for uninitialized registers and `nil` source literals.
    #[default]
    Nil,
    /// Boolean value used by comparisons and control-flow conditions.
    Bool(bool),
    /// Signed integer value used by the current arithmetic instruction set.
    Int(i64),
    /// Floating-point value reserved for future arithmetic growth.
    Float(f64),
    /// Reference to a heap object owned by the VM heap.
    Obj(ObjRef),
}

/// Wrapper that renders developer-oriented value output without changing `Display`.
pub struct ValueDebugDisplay<'a>(&'a Value);

impl Value {
    /// Returns a debug-display helper for explicit developer-facing formatting.
    pub fn debug_display(&self) -> ValueDebugDisplay<'_> {
        ValueDebugDisplay(self)
    }

    /// Extracts a heap object reference if this value is `Value::Obj`.
    ///
    /// Used by root scanning, GC tracing, and object-aware native functions.
    pub fn as_obj_ref(&self) -> Option<ObjRef> {
        match self {
            Self::Obj(reference) => Some(*reference),
            _ => None,
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nil, Self::Nil) => true,
            (Self::Bool(lhs), Self::Bool(rhs)) => lhs == rhs,
            (Self::Int(lhs), Self::Int(rhs)) => lhs == rhs,
            (Self::Float(lhs), Self::Float(rhs)) => lhs.to_bits() == rhs.to_bits(),
            (Self::Obj(lhs), Self::Obj(rhs)) => lhs == rhs,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_value_debug(self, f)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => f.write_str("nil"),
            Self::Bool(value) => write!(f, "{value}"),
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::Obj(reference) => write!(f, "<object {reference}>"),
        }
    }
}

impl fmt::Display for ValueDebugDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_value_debug(self.0, f)
    }
}

fn fmt_value_debug(value: &Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match value {
        Value::Nil => f.write_str("Nil"),
        Value::Bool(value) => write!(f, "Bool({value})"),
        Value::Int(value) => write!(f, "Int({value})"),
        Value::Float(value) => write!(f, "Float({value:?})"),
        Value::Obj(reference) => write!(f, "Obj({reference:?})"),
    }
}
