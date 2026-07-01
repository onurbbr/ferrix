//! Tests for native standard-library installation and behavior.

use std::{cell::RefCell, rc::Rc};

use ferrix_core::{
    Obj, Value,
    bytecode::{Chunk, Function, FunctionId, Instruction, Program, Register, VerifiedProgram},
};
use ferrix_stdlib::install;
use ferrix_vm::{OutputWriter, Vm, VmError, VmErrorKind};

#[derive(Clone, Debug, Default)]
struct SharedOutput(Rc<RefCell<Vec<String>>>);

impl SharedOutput {
    fn lines(&self) -> Vec<String> {
        self.0.borrow().clone()
    }
}

impl OutputWriter for SharedOutput {
    fn write_line(&mut self, line: &str) -> Result<(), VmError> {
        self.0.borrow_mut().push(line.to_string());
        Ok(())
    }
}

#[test]
fn install_registers_matching_native_functions() {
    let program = program_calling_native("print", Value::Int(42));
    let mut vm = Vm::new();

    let report = install(&mut vm, program.as_program());

    assert_eq!(report.registered, 1);
}

#[test]
fn print_writes_to_configured_output() {
    let program = program_calling_native("print", Value::Int(42));
    let output = SharedOutput::default();
    let mut vm = Vm::new();
    vm.set_output_writer(output.clone());
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Nil);
    assert_eq!(output.lines(), vec!["42".to_string()]);
}

#[test]
fn print_formats_heap_arrays() {
    let output = SharedOutput::default();
    let mut vm = Vm::new();
    let array = vm
        .allocate_object(Obj::Array(vec![Value::Int(1), Value::Bool(true)]))
        .unwrap();
    let program = program_calling_native("print", Value::Obj(array));
    vm.set_output_writer(output.clone());
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Nil);
    assert_eq!(output.lines(), vec!["[1, true]".to_string()]);
}

#[test]
fn print_formats_heap_maps() {
    let output = SharedOutput::default();
    let mut vm = Vm::new();
    let key = vm.allocate_object(Obj::String("name".to_string())).unwrap();
    let value = vm.allocate_object(Obj::String("Onur".to_string())).unwrap();
    let map = vm
        .allocate_object(Obj::Map(vec![(Value::Obj(key), Value::Obj(value))]))
        .unwrap();
    let program = program_calling_native("print", Value::Obj(map));
    vm.set_output_writer(output.clone());
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Nil);
    assert_eq!(output.lines(), vec!["{name: Onur}".to_string()]);
}

#[test]
fn print_formats_heap_records() {
    let output = SharedOutput::default();
    let mut vm = Vm::new();
    let record = vm
        .allocate_object(Obj::Record(vec![("name".to_string(), Value::Int(42))]))
        .unwrap();
    let program = program_calling_native("print", Value::Obj(record));
    vm.set_output_writer(output.clone());
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Nil);
    assert_eq!(output.lines(), vec!["{name: 42}".to_string()]);
}

#[test]
fn len_counts_heap_strings() {
    let mut vm = Vm::new();
    let string = vm
        .allocate_object(Obj::String("ferrix".to_string()))
        .unwrap();
    let program = program_calling_native("len", Value::Obj(string));
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Int(6));
}

#[test]
fn len_counts_heap_arrays() {
    let mut vm = Vm::new();
    let array = vm
        .allocate_object(Obj::Array(vec![Value::Int(1), Value::Int(2)]))
        .unwrap();
    let program = program_calling_native("len", Value::Obj(array));
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Int(2));
}

#[test]
fn len_counts_heap_maps() {
    let mut vm = Vm::new();
    let map = vm
        .allocate_object(Obj::Map(vec![(Value::Int(1), Value::Int(2))]))
        .unwrap();
    let program = program_calling_native("len", Value::Obj(map));
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Int(1));
}

#[test]
fn len_counts_heap_records() {
    let mut vm = Vm::new();
    let record = vm
        .allocate_object(Obj::Record(vec![("answer".to_string(), Value::Int(42))]))
        .unwrap();
    let program = program_calling_native("len", Value::Obj(record));
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Int(1));
}

#[test]
fn len_reports_type_errors() {
    let program = program_calling_native("len", Value::Int(42));
    let mut vm = Vm::new();
    install(&mut vm, program.as_program());

    let err = vm.run_program(&program).unwrap_err();

    assert_eq!(err.instruction_ip, Some(1));
    assert_eq!(
        err.kind,
        VmErrorKind::TypeError {
            expected: "string, array, map, or record",
            found: Value::Int(42),
        }
    );
}

#[test]
fn type_of_returns_heap_string() {
    let program = program_calling_native("type_of", Value::Bool(false));
    let mut vm = Vm::new();
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();
    let reference = result.as_obj_ref().unwrap();

    assert_eq!(
        vm.heap_object(reference).unwrap(),
        &Obj::String("bool".to_string())
    );
}

#[test]
fn type_of_reports_records() {
    let mut vm = Vm::new();
    let record = vm.allocate_object(Obj::Record(Vec::new())).unwrap();
    let program = program_calling_native("type_of", Value::Obj(record));
    install(&mut vm, program.as_program());

    let result = vm.run_program(&program).unwrap();
    let reference = result.as_obj_ref().unwrap();

    assert_eq!(
        vm.heap_object(reference).unwrap(),
        &Obj::String("record".to_string())
    );
}

fn program_calling_native(name: &str, value: Value) -> VerifiedProgram {
    let mut main = Chunk::new("main", 2);
    let constant = main.add_constant(value).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant,
    });
    main.push_instruction(Instruction::CallFunction {
        dst: Register(1),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 1,
    });
    main.push_instruction(Instruction::Return { src: Register(1) });

    let mut program = Program::new(FunctionId(1));
    program.add_function(Function::native(name, 1)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();
    VerifiedProgram::new(program).unwrap()
}
