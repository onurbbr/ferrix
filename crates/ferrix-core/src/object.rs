//! Heap object types and generational object references.
//!
//! The VM owns object storage; core only defines the stable handles and object
//! variants so bytecode, values, and native APIs can talk about them.

use std::fmt;

use crate::{Value, bytecode::FunctionId};

/// Stable handle to a heap object.
///
/// `index` identifies a heap slot and `generation` invalidates stale handles
/// after mark/sweep reuses a slot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjRef {
    /// Heap slot index.
    pub index: u32,
    /// Generation counter for stale-reference detection.
    pub generation: u32,
}

/// Heap-allocated object variants supported by Ferrix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Obj {
    /// UTF-8 string object.
    String(String),
    /// Mutable array of VM values.
    Array(Vec<Value>),
    /// Mutable map represented as insertion-ordered key/value pairs.
    Map(Vec<(Value, Value)>),
    /// Function object placeholder for callable object growth.
    Function(FunctionId),
    /// First-class function closure with captured values.
    Closure {
        /// Function table entry executed when the closure is called.
        function: FunctionId,
        /// Values captured from the surrounding lexical environment.
        captures: Vec<Value>,
    },
    /// Native function metadata object placeholder.
    NativeFunction(String),
    /// Module object placeholder for richer module systems.
    Module(String),
}

impl ObjRef {
    /// Creates an object reference from a heap slot index and generation.
    pub const fn new(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }
}

impl fmt::Debug for ObjRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}:{}", self.index, self.generation)
    }
}

impl fmt::Display for ObjRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}:{}", self.index, self.generation)
    }
}
