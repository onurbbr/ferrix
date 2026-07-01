//! Native function bridge between Ferrix bytecode and host Rust code.
//!
//! Native callbacks receive a [`NativeContext`] so they can allocate Ferrix
//! objects, inspect heap values, read VM limits, and write user-visible output.

use std::{error::Error, fmt, str::FromStr};

use ferrix_core::{Obj, ObjRef};

use crate::{RuntimeLimits, Vm, VmError};

/// Host-visible operation category guarded by runtime policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostCapability {
    /// Permission to call any registered native function.
    NativeCall,
    /// Permission to write user-visible output.
    IoOutput,
    /// Permission to read host files.
    FsRead,
    /// Permission to write host files.
    FsWrite,
    /// Permission to read host environment values.
    EnvRead,
    /// Permission to read host time.
    TimeRead,
    /// Permission to load source or bytecode modules.
    ModuleLoad,
    /// Permission to call custom host extensions.
    ExtensionCall,
}

impl HostCapability {
    /// Returns the stable dotted capability name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NativeCall => "native.call",
            Self::IoOutput => "io.output",
            Self::FsRead => "fs.read",
            Self::FsWrite => "fs.write",
            Self::EnvRead => "env.read",
            Self::TimeRead => "time.read",
            Self::ModuleLoad => "module.load",
            Self::ExtensionCall => "extension.call",
        }
    }
}

impl fmt::Display for HostCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HostCapability {
    type Err = HostCapabilityParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "native.call" => Ok(Self::NativeCall),
            "io.output" => Ok(Self::IoOutput),
            "fs.read" => Ok(Self::FsRead),
            "fs.write" => Ok(Self::FsWrite),
            "env.read" => Ok(Self::EnvRead),
            "time.read" => Ok(Self::TimeRead),
            "module.load" => Ok(Self::ModuleLoad),
            "extension.call" => Ok(Self::ExtensionCall),
            _ => Err(HostCapabilityParseError {
                value: value.to_string(),
            }),
        }
    }
}

/// Error returned when a capability string is not recognized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCapabilityParseError {
    value: String,
}

impl HostCapabilityParseError {
    /// Returns the invalid raw capability name.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for HostCapabilityParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid host capability `{}`", self.value)
    }
}

impl Error for HostCapabilityParseError {}

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

    /// Returns true when the active VM has a host capability.
    pub fn has_capability(&self, capability: HostCapability) -> bool {
        self.vm.has_capability(capability)
    }

    /// Requires one host capability for a native operation.
    pub fn require_capability(
        &mut self,
        capability: HostCapability,
        operation: &'static str,
    ) -> Result<(), VmError> {
        self.vm.require_capability(capability, operation)
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
        self.require_capability(HostCapability::IoOutput, "write output")?;
        self.vm.write_output_line(line)
    }
}

impl OutputWriter for NullOutput {
    fn write_line(&mut self, _line: &str) -> Result<(), VmError> {
        Ok(())
    }
}
