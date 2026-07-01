//! Test-friendly bytecode assembler.
//!
//! This crate provides a small builder and label system for constructing
//! bytecode fixtures without hand-writing instruction indexes.

pub mod builder;
pub mod label;

pub use builder::{Assembler, AssemblerError};
