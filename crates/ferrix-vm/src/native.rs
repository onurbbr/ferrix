//! Native function bridge between Ferrix bytecode and host Rust code.
//!
//! Native callbacks receive a [`NativeContext`] so they can allocate Ferrix
//! objects, inspect heap values, read VM limits, and write user-visible output.

use ferrix_core::{Obj, ObjRef};

use crate::{RuntimeLimits, Vm, VmError};

/// Output sink used by native functions such as the standard-library `print`.
pub trait OutputWriter {
    /// Writes one logical output line.
    fn write_line(&mut self, line: &str) -> Result<(), VmError>;
}

/// Output sink that discards every line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NullOutput;

/// Restricted view of the VM exposed to native functions.
pub struct NativeContext<'vm> {
    vm: &'vm mut Vm,
}

impl<'vm> NativeContext<'vm> {
    pub(crate) fn new(vm: &'vm mut Vm) -> Self {
        Self { vm }
    }

    /// Returns the active runtime limits for the host callback.
    pub fn limits(&self) -> RuntimeLimits {
        self.vm.limits()
    }

    /// Allocates a Ferrix object and returns its heap reference.
    pub fn allocate_object(&mut self, object: Obj) -> Result<ObjRef, VmError> {
        self.vm.allocate_object(object)
    }

    /// Reads an object by heap reference.
    pub fn heap_object(&self, reference: ObjRef) -> Result<&Obj, VmError> {
        self.vm.heap_object(reference)
    }

    /// Writes a line to the VM output sink.
    pub fn write_output_line(&mut self, line: &str) -> Result<(), VmError> {
        self.vm.write_output_line(line)
    }
}

impl OutputWriter for NullOutput {
    fn write_line(&mut self, _line: &str) -> Result<(), VmError> {
        Ok(())
    }
}
