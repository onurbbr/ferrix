//! Native standard-library bindings for Ferrix.
//!
//! The CLI installs these native functions into a VM after compilation. The
//! implementations use `NativeContext` to access heap objects and output.

use ferrix_core::{
    Obj, Value,
    bytecode::{FunctionId, Program},
};
use ferrix_vm::{HostCapability, NativeContext, Vm, VmError, VmErrorKind};

/// Signature implemented by every Ferrix native standard-library function.
pub type NativeCall = for<'vm> fn(&mut NativeContext<'vm>, &[Value]) -> Result<Value, VmError>;

/// Value shape declared by a native host function contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostValueType {
    /// Any Ferrix value is accepted.
    Any,
    /// Native returns no meaningful value.
    Nil,
    /// Native returns an integer.
    Int,
    /// Native returns a heap string.
    String,
}

/// Native host function scheduling class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCallKind {
    /// Synchronous host call executed on the VM thread.
    Sync,
}

/// Native function resource cost class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCostClass {
    /// Cheap pure or near-pure helper.
    Cheap,
    /// Normal local host interaction.
    Normal,
    /// Potentially external host effect.
    External,
}

/// Metadata and function pointer for one installable native binding.
#[derive(Clone, Copy)]
pub struct NativeBinding {
    /// Source-level function name, such as `print`.
    pub name: &'static str,
    /// Stable namespaced registry name, such as `io.print`.
    pub qualified_name: &'static str,
    /// Number of arguments expected by the native function.
    pub arity: u8,
    /// Parameter contract used by runtime/tooling validation.
    pub parameters: &'static [HostValueType],
    /// Return value contract used by runtime/tooling validation.
    pub returns: HostValueType,
    /// Capabilities required before this binding may run.
    pub required_capabilities: &'static [HostCapability],
    /// Profiles where this binding is available by default.
    pub available_profiles: &'static [&'static str],
    /// Sync/async classification for runtime scheduling.
    pub call_kind: HostCallKind,
    /// Estimated host resource cost.
    pub cost: HostCostClass,
    /// Short documentation string for CLI/runtime inspection.
    pub docs: &'static str,
    /// Host implementation called by the VM.
    pub call: NativeCall,
}

/// Summary returned after installing native bindings into a VM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstallReport {
    /// Number of program native declarations matched and registered.
    pub registered: usize,
}

const BINDINGS: &[NativeBinding] = &[
    NativeBinding {
        name: "print",
        qualified_name: "io.print",
        arity: 1,
        parameters: &[HostValueType::Any],
        returns: HostValueType::Nil,
        required_capabilities: &[HostCapability::NativeCall, HostCapability::IoOutput],
        available_profiles: &["development", "cli", "server", "trusted"],
        call_kind: HostCallKind::Sync,
        cost: HostCostClass::External,
        docs: "Writes one formatted value to the configured output sink.",
        call: print,
    },
    NativeBinding {
        name: "len",
        qualified_name: "core.len",
        arity: 1,
        parameters: &[HostValueType::Any],
        returns: HostValueType::Int,
        required_capabilities: &[HostCapability::NativeCall],
        available_profiles: &["development", "cli", "server", "trusted"],
        call_kind: HostCallKind::Sync,
        cost: HostCostClass::Cheap,
        docs: "Returns the length of a string, array, map, or record.",
        call: len,
    },
    NativeBinding {
        name: "type_of",
        qualified_name: "core.type_of",
        arity: 1,
        parameters: &[HostValueType::Any],
        returns: HostValueType::String,
        required_capabilities: &[HostCapability::NativeCall],
        available_profiles: &["development", "cli", "server", "trusted"],
        call_kind: HostCallKind::Sync,
        cost: HostCostClass::Cheap,
        docs: "Returns the runtime type name for one value.",
        call: type_of,
    },
];

/// Returns all built-in native bindings known to the standard library.
pub fn bindings() -> &'static [NativeBinding] {
    BINDINGS
}

/// Registers native implementations required by the given program.
pub fn install(vm: &mut Vm, program: &Program) -> InstallReport {
    install_with_filter(vm, program, |_| true)
}

/// Registers native implementations that pass the supplied binding filter.
pub fn install_with_filter(
    vm: &mut Vm,
    program: &Program,
    mut allow: impl FnMut(&NativeBinding) -> bool,
) -> InstallReport {
    let mut registered = 0;

    for (index, function) in program.functions.iter().enumerate() {
        let Some(native_name) = function.native_name() else {
            continue;
        };
        let Some(binding) = find_binding(native_name, function.arity) else {
            continue;
        };
        if !allow(binding) {
            continue;
        }

        vm.register_native_context_fn(FunctionId(index as u16), binding.call);
        registered += 1;
    }

    InstallReport { registered }
}

/// Looks up a native binding by name and arity.
pub fn find_binding(name: &str, arity: u8) -> Option<&'static NativeBinding> {
    BINDINGS
        .iter()
        .find(|binding| binding.name == name && binding.arity == arity)
}

/// Validates static registry metadata before runtime execution.
pub fn validate_registry() -> Result<(), String> {
    for binding in BINDINGS {
        if binding.parameters.len() != usize::from(binding.arity) {
            return Err(format!(
                "native binding `{}` declares arity {} but {} parameters",
                binding.qualified_name,
                binding.arity,
                binding.parameters.len()
            ));
        }
        if binding.required_capabilities.is_empty() {
            return Err(format!(
                "native binding `{}` declares no required capabilities",
                binding.qualified_name
            ));
        }
        if binding.available_profiles.is_empty() {
            return Err(format!(
                "native binding `{}` declares no profile availability",
                binding.qualified_name
            ));
        }
    }
    Ok(())
}

fn print(ctx: &mut NativeContext<'_>, args: &[Value]) -> Result<Value, VmError> {
    let [value] = args else {
        unreachable!("program verifier enforces native arity");
    };

    ctx.write_output_line(&display_value(ctx, *value, 0)?)?;
    Ok(Value::Nil)
}

fn len(ctx: &mut NativeContext<'_>, args: &[Value]) -> Result<Value, VmError> {
    let [value] = args else {
        unreachable!("program verifier enforces native arity");
    };

    let Value::Obj(reference) = value else {
        return Err(type_error("string, array, map, or record", *value));
    };

    let length = match ctx.heap_object(*reference)? {
        Obj::String(value) => value.chars().count(),
        Obj::Array(values) => values.len(),
        Obj::Map(entries) => entries.len(),
        Obj::Record(fields) => fields.len(),
        _ => return Err(type_error("string, array, map, or record", *value)),
    };

    Ok(Value::Int(length.try_into().unwrap_or(i64::MAX)))
}

fn type_of(ctx: &mut NativeContext<'_>, args: &[Value]) -> Result<Value, VmError> {
    let [value] = args else {
        unreachable!("program verifier enforces native arity");
    };

    let name = match value {
        Value::Nil => "nil",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Obj(reference) => match ctx.heap_object(*reference)? {
            Obj::String(_) => "string",
            Obj::Array(_) => "array",
            Obj::Map(_) => "map",
            Obj::Record(_) => "record",
            Obj::Upvalue(_) => "upvalue",
            Obj::Function(_) => "function",
            Obj::Closure { .. } => "function",
            Obj::NativeFunction(_) => "native_function",
            Obj::Module(_) => "module",
        },
    };

    let reference = ctx.allocate_object(Obj::String(name.to_string()))?;
    Ok(Value::Obj(reference))
}

fn type_error(expected: &'static str, found: Value) -> VmError {
    VmError::new(None, VmErrorKind::TypeError { expected, found })
}

fn display_value(ctx: &NativeContext<'_>, value: Value, depth: usize) -> Result<String, VmError> {
    if depth >= 8 {
        return Ok("...".to_string());
    }

    match value {
        Value::Obj(reference) => match ctx.heap_object(reference)? {
            Obj::String(value) => Ok(value.clone()),
            Obj::Array(values) => {
                let values = values
                    .iter()
                    .copied()
                    .map(|value| display_value(ctx, value, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("[{}]", values.join(", ")))
            }
            Obj::Map(entries) => {
                let entries = entries
                    .iter()
                    .map(|(key, value)| {
                        Ok(format!(
                            "{}: {}",
                            display_value(ctx, *key, depth + 1)?,
                            display_value(ctx, *value, depth + 1)?
                        ))
                    })
                    .collect::<Result<Vec<_>, VmError>>()?;
                Ok(format!("{{{}}}", entries.join(", ")))
            }
            Obj::Record(fields) => {
                let fields = fields
                    .iter()
                    .map(|(field, value)| {
                        Ok(format!(
                            "{}: {}",
                            field,
                            display_value(ctx, *value, depth + 1)?
                        ))
                    })
                    .collect::<Result<Vec<_>, VmError>>()?;
                Ok(format!("{{{}}}", fields.join(", ")))
            }
            _ => Ok(value.to_string()),
        },
        _ => Ok(value.to_string()),
    }
}
