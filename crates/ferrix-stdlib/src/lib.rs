//! Native standard-library bindings for Ferrix.
//!
//! The CLI installs these native functions into a VM after compilation. The
//! implementations use `NativeContext` to access heap objects and output.

use ferrix_core::{
    Obj, Value,
    bytecode::{FunctionId, Program},
};
use ferrix_vm::{NativeContext, Vm, VmError, VmErrorKind};

/// Signature implemented by every Ferrix native standard-library function.
pub type NativeCall = for<'vm> fn(&mut NativeContext<'vm>, &[Value]) -> Result<Value, VmError>;

/// Metadata and function pointer for one installable native binding.
#[derive(Clone, Copy)]
pub struct NativeBinding {
    /// Source-level function name, such as `print`.
    pub name: &'static str,
    /// Number of arguments expected by the native function.
    pub arity: u8,
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
        arity: 1,
        call: print,
    },
    NativeBinding {
        name: "len",
        arity: 1,
        call: len,
    },
    NativeBinding {
        name: "type_of",
        arity: 1,
        call: type_of,
    },
];

/// Returns all built-in native bindings known to the standard library.
pub fn bindings() -> &'static [NativeBinding] {
    BINDINGS
}

/// Registers native implementations required by the given program.
pub fn install(vm: &mut Vm, program: &Program) -> InstallReport {
    let mut registered = 0;

    for (index, function) in program.functions.iter().enumerate() {
        let Some(native_name) = function.native_name() else {
            continue;
        };
        let Some(binding) = find_binding(native_name, function.arity) else {
            continue;
        };

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
        return Err(type_error("string, array, or map", *value));
    };

    let length = match ctx.heap_object(*reference)? {
        Obj::String(value) => value.chars().count(),
        Obj::Array(values) => values.len(),
        Obj::Map(entries) => entries.len(),
        _ => return Err(type_error("string, array, or map", *value)),
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
            _ => Ok(value.to_string()),
        },
        _ => Ok(value.to_string()),
    }
}
