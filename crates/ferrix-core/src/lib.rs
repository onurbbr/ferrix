//! Shared core types for Ferrix.
//!
//! This crate owns bytecode, values, objects, and diagnostics so VM,
//! compiler, assembler, stdlib, and CLI crates can depend on one stable
//! vocabulary without forming dependency cycles.

pub mod bytecode;
pub mod diagnostics;
pub mod object;
pub mod value;

pub use object::{Obj, ObjRef};
pub use value::Value;
