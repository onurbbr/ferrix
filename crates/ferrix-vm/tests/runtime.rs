//! Integration-style tests for VM instruction execution and runtime services.

use ferrix_core::{
    Obj, ObjRef, Value,
    bytecode::{
        CaptureId, Chunk, ConstId, Function, FunctionId, Instruction, JumpTarget, Program,
        Register, VerifiedChunk, VerifiedProgram,
    },
    diagnostics::{FileId, SourceManager, SourceSpan},
};
use ferrix_vm::{
    DebugAction, DebugEvent, DebugOutcome, Debugger, IncrementalGcPhase, RuntimeLimits, Vm,
    VmError, VmErrorKind, VmStackFrame,
};

#[test]
fn vm_stores_runtime_limits() {
    let limits = RuntimeLimits {
        max_instruction_count: 10,
        max_call_depth: 2,
        max_heap_objects: 16,
        gc_allocation_threshold: 8,
        gc_incremental_step_budget: 64,
    };

    let vm = Vm::with_limits(limits);

    assert_eq!(vm.limits(), limits);
}

#[test]
fn vm_allocates_heap_objects() {
    let mut vm = Vm::new();

    let reference = vm
        .allocate_object(Obj::String("hello".to_string()))
        .unwrap();

    assert_eq!(reference, ObjRef::new(0, 0));
    assert_eq!(vm.heap().len(), 1);
    assert_eq!(
        vm.heap_object(reference).unwrap(),
        &Obj::String("hello".to_string())
    );
}

#[test]
fn vm_allocation_respects_heap_limit() {
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 10,
        max_call_depth: 2,
        max_heap_objects: 0,
        gc_allocation_threshold: 0,
        gc_incremental_step_budget: 64,
    });

    let err = vm
        .allocate_object(Obj::String("blocked".to_string()))
        .unwrap_err();

    assert_eq!(
        err.kind,
        VmErrorKind::HeapObjectLimitExceeded {
            max_heap_objects: 0,
        }
    );
}

#[test]
fn vm_roots_include_registers_and_program_constants() {
    let mut vm = Vm::new();
    let register_ref = vm
        .allocate_object(Obj::String("register".to_string()))
        .unwrap();
    let constant_ref = vm.allocate_object(Obj::Array(vec![Value::Int(1)])).unwrap();

    let mut main = Chunk::new("main", 1);
    let returned = main.add_constant(Value::Obj(register_ref)).unwrap();
    main.add_constant(Value::Obj(constant_ref)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: returned,
    });
    main.push_instruction(Instruction::Return { src: Register(0) });

    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Obj(register_ref));
    assert_eq!(
        vm.root_objects_with_program(program.as_program()),
        vec![register_ref, constant_ref]
    );
}

#[test]
fn debugger_observes_program_instructions_before_execution() {
    let mut chunk = Chunk::new("main", 1);
    let value = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(chunk)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();
    let mut debugger = RecordingDebugger {
        events: Vec::new(),
        quit_after: None,
    };

    let outcome = Vm::new()
        .run_program_with_debugger(&program, &mut debugger)
        .unwrap();

    assert_eq!(outcome, DebugOutcome::Completed(Value::Int(42)));
    assert_eq!(
        debugger.events,
        vec![
            "fn#0 main ip=0 r0=Nil".to_string(),
            "fn#0 main ip=1 r0=Int(42)".to_string()
        ]
    );
}

#[test]
fn debugger_can_quit_before_program_finishes() {
    let mut chunk = Chunk::new("main", 1);
    let value = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(chunk)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();
    let mut debugger = RecordingDebugger {
        events: Vec::new(),
        quit_after: Some(1),
    };

    let outcome = Vm::new()
        .run_program_with_debugger(&program, &mut debugger)
        .unwrap();

    assert_eq!(outcome, DebugOutcome::Quit);
    assert_eq!(debugger.events.len(), 1);
}

struct RecordingDebugger {
    events: Vec<String>,
    quit_after: Option<usize>,
}

impl Debugger for RecordingDebugger {
    fn before_instruction(&mut self, event: DebugEvent<'_>) -> DebugAction {
        self.events.push(format!(
            "{} {} ip={} r0={:?}",
            event.function,
            event.function_name,
            event.instruction_ip,
            event.registers.first().copied().unwrap_or(Value::Nil)
        ));

        if self.quit_after == Some(self.events.len()) {
            DebugAction::Quit
        } else {
            DebugAction::Step
        }
    }
}

#[test]
fn manual_gc_preserves_register_roots() {
    let mut vm = Vm::new();
    let kept = vm.allocate_object(Obj::String("kept".to_string())).unwrap();
    let dropped = vm
        .allocate_object(Obj::String("dropped".to_string()))
        .unwrap();

    let mut chunk = Chunk::new("main", 1);
    let constant = chunk.add_constant(Value::Obj(kept)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    assert_eq!(vm.run(&chunk).unwrap(), Value::Obj(kept));

    let stats = vm.collect_garbage();

    assert_eq!(stats.marked, 1);
    assert_eq!(stats.swept, 1);
    assert_eq!(stats.live, 1);
    assert!(vm.heap_object(kept).is_ok());
    assert_eq!(
        vm.heap_object(dropped).unwrap_err().kind,
        VmErrorKind::InvalidObjectRef { reference: dropped }
    );
}

#[test]
fn allocation_runs_gc_before_heap_limit_error() {
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 10,
        max_call_depth: 2,
        max_heap_objects: 1,
        gc_allocation_threshold: 0,
        gc_incremental_step_budget: 64,
    });
    let old = vm.allocate_object(Obj::String("old".to_string())).unwrap();

    let new = vm.allocate_object(Obj::String("new".to_string())).unwrap();

    assert_eq!(new.index, old.index);
    assert_ne!(new.generation, old.generation);
    assert!(vm.heap_object(old).is_err());
    assert_eq!(
        vm.heap_object(new).unwrap(),
        &Obj::String("new".to_string())
    );
}

#[test]
fn allocation_threshold_runs_gc_before_heap_limit() {
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 10,
        max_call_depth: 2,
        max_heap_objects: 10,
        gc_allocation_threshold: 2,
        gc_incremental_step_budget: 64,
    });
    let first = vm
        .allocate_object(Obj::String("first".to_string()))
        .unwrap();
    let second = vm
        .allocate_object(Obj::String("second".to_string()))
        .unwrap();

    let third = vm
        .allocate_object(Obj::String("third".to_string()))
        .unwrap();

    assert!(vm.heap_object(first).is_err());
    assert!(vm.heap_object(second).is_err());
    assert_eq!(
        vm.heap_object(third).unwrap(),
        &Obj::String("third".to_string())
    );
    assert_eq!(vm.heap().len(), 1);
    assert_eq!(vm.gc_stats().allocations, 3);
    assert_eq!(vm.gc_stats().allocation_pressure, 1);
    assert_eq!(vm.gc_stats().collections, 1);
    assert_eq!(vm.gc_stats().last_collection.marked, 0);
    assert_eq!(vm.gc_stats().last_collection.swept, 2);
    assert_eq!(vm.gc_stats().total_swept, 2);
}

#[test]
fn allocation_threshold_can_finish_incrementally_at_safepoints() {
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 32,
        max_call_depth: 2,
        max_heap_objects: 16,
        gc_allocation_threshold: 3,
        gc_incremental_step_budget: 1,
    });
    let first = vm
        .allocate_object(Obj::String("first".to_string()))
        .unwrap();
    let second = vm
        .allocate_object(Obj::String("second".to_string()))
        .unwrap();
    let third = vm
        .allocate_object(Obj::String("third".to_string()))
        .unwrap();

    let allocated_during_gc = vm
        .allocate_object(Obj::String("during".to_string()))
        .unwrap();

    assert_ne!(vm.incremental_gc_phase(), IncrementalGcPhase::Idle);
    assert_eq!(vm.gc_stats().collections, 0);

    let mut chunk = Chunk::new("safepoints", 1);
    let value = chunk.add_constant(Value::Int(7)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    for _ in 0..6 {
        chunk.push_instruction(Instruction::Move {
            dst: Register(0),
            src: Register(0),
        });
    }
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    assert_eq!(vm.run(&chunk).unwrap(), Value::Int(7));

    assert_eq!(vm.incremental_gc_phase(), IncrementalGcPhase::Idle);
    assert_eq!(vm.gc_stats().collections, 1);
    assert!(vm.gc_stats().incremental_steps > 1);
    assert!(vm.heap_object(first).is_err());
    assert!(vm.heap_object(second).is_err());
    assert!(vm.heap_object(third).is_err());
    assert!(vm.heap_object(allocated_during_gc).is_ok());
}

#[test]
fn threshold_gc_preserves_register_roots_during_execution() {
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 10,
        max_call_depth: 2,
        max_heap_objects: 10,
        gc_allocation_threshold: 1,
        gc_incremental_step_budget: 64,
    });
    let mut chunk = Chunk::new("main", 2);
    let kept = chunk.add_string("kept").unwrap();
    let pressure = chunk.add_string("pressure").unwrap();
    chunk.push_instruction(Instruction::LoadString {
        dst: Register(0),
        string: kept,
    });
    chunk.push_instruction(Instruction::LoadString {
        dst: Register(1),
        string: pressure,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = vm.run(&chunk).unwrap();

    let Value::Obj(reference) = result else {
        panic!("expected heap object result");
    };
    assert_eq!(
        vm.heap_object(reference).unwrap(),
        &Obj::String("kept".to_string())
    );
    assert_eq!(vm.gc_stats().allocations, 2);
    assert_eq!(vm.gc_stats().collections, 1);
    assert_eq!(vm.gc_stats().last_collection.marked, 1);
    assert_eq!(vm.gc_stats().last_collection.swept, 0);
}

#[test]
fn gc_preserves_program_constant_roots_when_requested() {
    let mut vm = Vm::new();
    let kept = vm
        .allocate_object(Obj::String("constant".to_string()))
        .unwrap();
    let dropped = vm
        .allocate_object(Obj::String("dropped".to_string()))
        .unwrap();

    let mut main = Chunk::new("main", 1);
    main.add_constant(Value::Obj(kept)).unwrap();
    main.push_instruction(Instruction::Return { src: Register(0) });
    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let stats = vm.collect_garbage_with_program(program.as_program());

    assert_eq!(stats.marked, 1);
    assert_eq!(stats.swept, 1);
    assert_eq!(stats.live, 1);
    assert!(vm.heap_object(kept).is_ok());
    assert!(vm.heap_object(dropped).is_err());
}

#[test]
fn chunk_constants_are_roots_during_execution_gc() {
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 10,
        max_call_depth: 2,
        max_heap_objects: 2,
        gc_allocation_threshold: 0,
        gc_incremental_step_budget: 64,
    });
    let kept = vm
        .allocate_object(Obj::String("constant".to_string()))
        .unwrap();
    let dropped = vm
        .allocate_object(Obj::String("dropped".to_string()))
        .unwrap();

    let mut chunk = Chunk::new("main", 2);
    let constant = chunk.add_constant(Value::Obj(kept)).unwrap();
    let allocated = chunk.add_string("allocated").unwrap();
    chunk.push_instruction(Instruction::LoadString {
        dst: Register(0),
        string: allocated,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant,
    });
    chunk.push_instruction(Instruction::Return { src: Register(1) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    assert_eq!(vm.run(&chunk).unwrap(), Value::Obj(kept));
    assert!(vm.heap_object(kept).is_ok());
    assert!(vm.heap_object(dropped).is_err());
}

#[test]
fn chunk_constants_are_roots_during_traced_execution_gc() {
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 10,
        max_call_depth: 2,
        max_heap_objects: 2,
        gc_allocation_threshold: 0,
        gc_incremental_step_budget: 64,
    });
    let kept = vm
        .allocate_object(Obj::String("constant".to_string()))
        .unwrap();
    let dropped = vm
        .allocate_object(Obj::String("dropped".to_string()))
        .unwrap();

    let mut chunk = Chunk::new("main", 2);
    let constant = chunk.add_constant(Value::Obj(kept)).unwrap();
    let allocated = chunk.add_string("allocated").unwrap();
    chunk.push_instruction(Instruction::LoadString {
        dst: Register(0),
        string: allocated,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant,
    });
    chunk.push_instruction(Instruction::Return { src: Register(1) });
    let chunk = VerifiedChunk::new(chunk).unwrap();
    let mut trace = String::new();

    assert_eq!(
        vm.run_with_trace(&chunk, &mut trace).unwrap(),
        Value::Obj(kept)
    );
    assert!(vm.heap_object(kept).is_ok());
    assert!(vm.heap_object(dropped).is_err());
}

#[test]
fn chunk_execution_enforces_instruction_budget() {
    let mut chunk = Chunk::new("spin", 1);
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(0),
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let chunk = VerifiedChunk::new(chunk).unwrap();
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 3,
        max_call_depth: 16,
        max_heap_objects: 16,
        gc_allocation_threshold: 0,
        gc_incremental_step_budget: 64,
    });

    let err = vm.run(&chunk).unwrap_err();

    assert_eq!(vm.executed_instruction_count(), 3);
    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VmErrorKind::InstructionLimitExceeded {
            max_instruction_count: 3,
        }
    );
}

#[test]
fn program_execution_enforces_instruction_budget() {
    let mut main = Chunk::new("main", 1);
    main.push_instruction(Instruction::Jump {
        target: JumpTarget(0),
    });
    main.push_instruction(Instruction::Return { src: Register(0) });

    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 2,
        max_call_depth: 16,
        max_heap_objects: 16,
        gc_allocation_threshold: 0,
        gc_incremental_step_budget: 64,
    });

    let err = vm.run_program(&program).unwrap_err();

    assert_eq!(vm.executed_instruction_count(), 2);
    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VmErrorKind::InstructionLimitExceeded {
            max_instruction_count: 2,
        }
    );
}

#[test]
fn runs_arithmetic_program() {
    let mut chunk = Chunk::new("main", 3);
    let ten = chunk.add_constant(Value::Int(10)).unwrap();
    let thirty_two = chunk.add_constant(Value::Int(32)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: ten,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: thirty_two,
    });
    chunk.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn runs_specialized_integer_arithmetic_program() {
    let mut chunk = Chunk::new("main", 6);
    let ten = chunk.add_constant(Value::Int(10)).unwrap();
    let thirty_two = chunk.add_constant(Value::Int(32)).unwrap();
    let two = chunk.add_constant(Value::Int(2)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: ten,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: thirty_two,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(2),
        constant: two,
    });
    chunk.push_instruction(Instruction::AddInt {
        dst: Register(3),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::SubInt {
        dst: Register(3),
        lhs: Register(3),
        rhs: Register(2),
    });
    chunk.push_instruction(Instruction::MulInt {
        dst: Register(4),
        lhs: Register(3),
        rhs: Register(2),
    });
    chunk.push_instruction(Instruction::DivInt {
        dst: Register(5),
        lhs: Register(4),
        rhs: Register(2),
    });
    chunk.push_instruction(Instruction::Return { src: Register(5) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(40));
}

#[test]
fn runs_specialized_integer_comparison_program() {
    let mut chunk = Chunk::new("main", 3);
    let forty = chunk.add_constant(Value::Int(40)).unwrap();
    let forty_two = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: forty,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: forty_two,
    });
    chunk.push_instruction(Instruction::LessEqualInt {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Bool(true));
}

#[test]
fn move_copies_register_value() {
    let mut chunk = Chunk::new("main", 2);
    let value = chunk.add_constant(Value::Int(7)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::Move {
        dst: Register(1),
        src: Register(0),
    });
    chunk.push_instruction(Instruction::Return { src: Register(1) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(7));
}

#[test]
fn load_string_allocates_heap_string() {
    let mut chunk = Chunk::new("main", 1);
    let string = chunk.add_string("ferrix").unwrap();
    chunk.push_instruction(Instruction::LoadString {
        dst: Register(0),
        string,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let chunk = VerifiedChunk::new(chunk).unwrap();
    let mut vm = Vm::new();

    let result = vm.run(&chunk).unwrap();
    let reference = result.as_obj_ref().unwrap();

    assert_eq!(
        vm.heap_object(reference).unwrap(),
        &Obj::String("ferrix".to_string())
    );
}

#[test]
fn array_new_get_and_set_are_supported() {
    let mut chunk = Chunk::new("array", 5);
    let zero = chunk.add_constant(Value::Int(0)).unwrap();
    let one = chunk.add_constant(Value::Int(1)).unwrap();
    let replacement = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: zero,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: one,
    });
    chunk.push_instruction(Instruction::ArrayNew {
        dst: Register(2),
        elements_start: Register(1),
        element_count: 1,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(3),
        constant: replacement,
    });
    chunk.push_instruction(Instruction::ArraySet {
        array: Register(2),
        index: Register(0),
        value: Register(3),
    });
    chunk.push_instruction(Instruction::ArrayGet {
        dst: Register(4),
        array: Register(2),
        index: Register(0),
    });
    chunk.push_instruction(Instruction::Return { src: Register(4) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn array_get_reports_out_of_bounds() {
    let mut chunk = Chunk::new("array", 3);
    let index = chunk.add_constant(Value::Int(1)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: index,
    });
    chunk.push_instruction(Instruction::ArrayNew {
        dst: Register(1),
        elements_start: Register(0),
        element_count: 0,
    });
    chunk.push_instruction(Instruction::ArrayGet {
        dst: Register(2),
        array: Register(1),
        index: Register(0),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let err = Vm::new().run(&chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(2));
    assert_eq!(err.kind, VmErrorKind::IndexOutOfBounds { index: 1, len: 0 });
}

#[test]
fn map_new_get_and_set_are_supported() {
    let mut chunk = Chunk::new("map", 6);
    let key = chunk.add_constant(Value::Int(7)).unwrap();
    let initial = chunk.add_constant(Value::Int(1)).unwrap();
    let replacement = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: key,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: initial,
    });
    chunk.push_instruction(Instruction::MapNew {
        dst: Register(2),
        entries_start: Register(0),
        entry_count: 1,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(3),
        constant: replacement,
    });
    chunk.push_instruction(Instruction::IndexSet {
        target: Register(2),
        index: Register(0),
        value: Register(3),
    });
    chunk.push_instruction(Instruction::IndexGet {
        dst: Register(4),
        target: Register(2),
        index: Register(0),
    });
    chunk.push_instruction(Instruction::Return { src: Register(4) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn map_get_missing_key_returns_nil() {
    let mut chunk = Chunk::new("map", 5);
    let key = chunk.add_constant(Value::Int(7)).unwrap();
    let missing = chunk.add_constant(Value::Int(99)).unwrap();
    let value = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: key,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: value,
    });
    chunk.push_instruction(Instruction::MapNew {
        dst: Register(2),
        entries_start: Register(0),
        entry_count: 1,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(3),
        constant: missing,
    });
    chunk.push_instruction(Instruction::IndexGet {
        dst: Register(4),
        target: Register(2),
        index: Register(3),
    });
    chunk.push_instruction(Instruction::Return { src: Register(4) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Nil);
}

#[test]
fn record_new_get_and_set_are_supported() {
    let mut chunk = Chunk::new("record", 5);
    let field = chunk.add_string("answer").unwrap();
    let initial = chunk.add_constant(Value::Int(1)).unwrap();
    let replacement = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: initial,
    });
    chunk.push_instruction(Instruction::RecordNew {
        dst: Register(1),
        fields_start: Register(0),
        fields: vec![field],
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(2),
        constant: replacement,
    });
    chunk.push_instruction(Instruction::FieldSet {
        target: Register(1),
        field,
        value: Register(2),
    });
    chunk.push_instruction(Instruction::FieldGet {
        dst: Register(3),
        target: Register(1),
        field,
    });
    chunk.push_instruction(Instruction::Return { src: Register(3) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn record_get_missing_field_returns_nil() {
    let mut chunk = Chunk::new("record", 4);
    let present = chunk.add_string("present").unwrap();
    let missing = chunk.add_string("missing").unwrap();
    let value = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::RecordNew {
        dst: Register(1),
        fields_start: Register(0),
        fields: vec![present],
    });
    chunk.push_instruction(Instruction::FieldGet {
        dst: Register(2),
        target: Register(1),
        field: missing,
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Nil);
}

#[test]
fn arithmetic_instructions_operate_on_ints() {
    assert_eq!(run_binary(8, 3, InstructionKind::Sub), Value::Int(5));
    assert_eq!(run_binary(8, 3, InstructionKind::Mul), Value::Int(24));
    assert_eq!(run_binary(8, 2, InstructionKind::Div), Value::Int(4));
}

#[test]
fn type_error_reports_instruction_ip() {
    let mut chunk = Chunk::new("main", 3);
    let bool_const = chunk.add_constant(Value::Bool(true)).unwrap();
    let int_const = chunk.add_constant(Value::Int(32)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: bool_const,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: int_const,
    });
    chunk.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let err = Vm::new().run(&chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(2));
    assert_eq!(
        err.kind,
        VmErrorKind::TypeError {
            expected: "int",
            found: Value::Bool(true),
        }
    );
}

#[test]
fn division_by_zero_is_typed_error() {
    let err = run_binary_error(10, 0, InstructionKind::Div);

    assert_eq!(err.instruction_ip, Some(2));
    assert_eq!(err.kind, VmErrorKind::DivisionByZero);
}

#[test]
fn missing_return_is_typed_error() {
    let mut chunk = Chunk::new("main", 1);
    let value = chunk.add_constant(Value::Int(1)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });

    let err = Vm::new().run_unchecked(&chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(1));
    assert_eq!(err.kind, VmErrorKind::MissingReturn);
}

#[test]
fn unchecked_execution_returns_invalid_constant_error() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: ConstId(0),
    });

    let err = Vm::new().run_unchecked(&chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VmErrorKind::InvalidConstant {
            constant: ConstId(0),
            constant_count: 0,
        }
    );
}

#[test]
fn trace_output_records_instruction_and_registers_before_execution() {
    let mut chunk = Chunk::new("main", 3);
    let ten = chunk.add_constant(Value::Int(10)).unwrap();
    let thirty_two = chunk.add_constant(Value::Int(32)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: ten,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: thirty_two,
    });
    chunk.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });
    let chunk = VerifiedChunk::new(chunk).unwrap();
    let mut trace = String::new();

    let result = Vm::new().run_with_trace(&chunk, &mut trace).unwrap();

    assert_eq!(result, Value::Int(42));
    assert_eq!(
        trace,
        "\
ip=0000 LoadConst   r0, #0
registers: r0=Nil, r1=Nil, r2=Nil
ip=0001 LoadConst   r1, #1
registers: r0=Int(10), r1=Nil, r2=Nil
ip=0002 Add         r2, r0, r1
registers: r0=Int(10), r1=Int(32), r2=Nil
ip=0003 Return      r2
registers: r0=Int(10), r1=Int(32), r2=Int(42)
"
    );
}

#[test]
fn runs_if_else_with_jump_if_false() {
    let mut chunk = Chunk::new("if_else", 2);
    let true_const = chunk.add_constant(Value::Bool(true)).unwrap();
    let then_value = chunk.add_constant(Value::Int(10)).unwrap();
    let else_value = chunk.add_constant(Value::Int(20)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: true_const,
    });
    chunk.push_instruction(Instruction::JumpIfFalse {
        condition: Register(0),
        target: JumpTarget(4),
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: then_value,
    });
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(5),
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: else_value,
    });
    chunk.push_instruction(Instruction::Return { src: Register(1) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(10));
}

#[test]
fn runs_counter_loop() {
    let mut chunk = Chunk::new("loop", 6);
    let zero = chunk.add_constant(Value::Int(0)).unwrap();
    let one = chunk.add_constant(Value::Int(1)).unwrap();
    let five = chunk.add_constant(Value::Int(5)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: zero,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: zero,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(2),
        constant: one,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(3),
        constant: five,
    });
    chunk.push_instruction(Instruction::Less {
        dst: Register(4),
        lhs: Register(0),
        rhs: Register(3),
    });
    chunk.push_instruction(Instruction::JumpIfFalse {
        condition: Register(4),
        target: JumpTarget(9),
    });
    chunk.push_instruction(Instruction::Add {
        dst: Register(1),
        lhs: Register(1),
        rhs: Register(0),
    });
    chunk.push_instruction(Instruction::Add {
        dst: Register(0),
        lhs: Register(0),
        rhs: Register(2),
    });
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(4),
    });
    chunk.push_instruction(Instruction::Return { src: Register(1) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(10));
}

#[test]
fn comparisons_and_not_produce_bool_values() {
    let mut chunk = Chunk::new("compare", 5);
    let two = chunk.add_constant(Value::Int(2)).unwrap();
    let three = chunk.add_constant(Value::Int(3)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: two,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: three,
    });
    chunk.push_instruction(Instruction::Less {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Not {
        dst: Register(3),
        src: Register(2),
    });
    chunk.push_instruction(Instruction::Equal {
        dst: Register(4),
        lhs: Register(2),
        rhs: Register(3),
    });
    chunk.push_instruction(Instruction::Return { src: Register(4) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Bool(false));
}

#[test]
fn non_bool_condition_is_typed_error() {
    let mut chunk = Chunk::new("bad_condition", 1);
    let value = chunk.add_constant(Value::Int(1)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::JumpIfTrue {
        condition: Register(0),
        target: JumpTarget(2),
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let err = Vm::new().run(&chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(1));
    assert_eq!(
        err.kind,
        VmErrorKind::TypeError {
            expected: "bool",
            found: Value::Int(1),
        }
    );
}

#[test]
fn unchecked_execution_returns_invalid_jump_target_error() {
    let mut chunk = Chunk::new("bad_jump", 1);
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(10),
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let err = Vm::new().run_unchecked(&chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VmErrorKind::InvalidJumpTarget {
            target: JumpTarget(10),
            instruction_count: 2,
        }
    );
}

#[test]
fn runs_direct_function_call() {
    let mut add = Chunk::new("add", 3).with_arity(2);
    add.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    add.push_instruction(Instruction::Return { src: Register(2) });

    let mut main = Chunk::new("main", 4);
    let ten = main.add_constant(Value::Int(10)).unwrap();
    let thirty_two = main.add_constant(Value::Int(32)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: ten,
    });
    main.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: thirty_two,
    });
    main.push_instruction(Instruction::CallFunction {
        dst: Register(2),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 2,
    });
    main.push_instruction(Instruction::Return { src: Register(2) });

    let mut program = Program::new(FunctionId(1));
    program.add_function(Function::bytecode(add)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn runs_closure_call_with_captured_value() {
    let mut add_capture = Chunk::new("closure#0", 3)
        .with_arity(1)
        .with_capture_count(1);
    add_capture.push_instruction(Instruction::LoadCapture {
        dst: Register(1),
        capture: CaptureId(0),
    });
    add_capture.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(1),
        rhs: Register(0),
    });
    add_capture.push_instruction(Instruction::Return { src: Register(2) });

    let mut main = Chunk::new("main", 4);
    let forty = main.add_constant(Value::Int(40)).unwrap();
    let two = main.add_constant(Value::Int(2)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: forty,
    });
    main.push_instruction(Instruction::MakeUpvalue {
        dst: Register(0),
        src: Register(0),
    });
    main.push_instruction(Instruction::MakeClosure {
        dst: Register(1),
        function: FunctionId(0),
        captures_start: Register(0),
        capture_count: 1,
    });
    main.push_instruction(Instruction::LoadConst {
        dst: Register(2),
        constant: two,
    });
    main.push_instruction(Instruction::CallValue {
        dst: Register(3),
        callee: Register(1),
        args_start: Register(2),
        arg_count: 1,
    });
    main.push_instruction(Instruction::Return { src: Register(3) });

    let mut program = Program::new(FunctionId(1));
    program
        .add_function(Function::bytecode(add_capture))
        .unwrap();
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn upvalue_cells_load_and_store_values() {
    let mut chunk = Chunk::new("upvalue", 4);
    let one = chunk.add_constant(Value::Int(1)).unwrap();
    let forty_two = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: one,
    });
    chunk.push_instruction(Instruction::MakeUpvalue {
        dst: Register(1),
        src: Register(0),
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(2),
        constant: forty_two,
    });
    chunk.push_instruction(Instruction::StoreUpvalue {
        upvalue: Register(1),
        src: Register(2),
    });
    chunk.push_instruction(Instruction::LoadUpvalue {
        dst: Register(3),
        upvalue: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(3) });
    let chunk = VerifiedChunk::new(chunk).unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn runtime_errors_capture_stack_trace() {
    let file = FileId(0);
    let mut fail = Chunk::new("fail", 3);
    let one = fail.add_constant(Value::Int(1)).unwrap();
    let zero = fail.add_constant(Value::Int(0)).unwrap();
    fail.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: one,
    });
    fail.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: zero,
    });
    fail.push_instruction_with_span(
        Instruction::Div {
            dst: Register(2),
            lhs: Register(0),
            rhs: Register(1),
        },
        Some(SourceSpan::new(file, 23, 28)),
    );
    fail.push_instruction(Instruction::Return { src: Register(2) });

    let mut main = Chunk::new("main", 1);
    main.push_instruction_with_span(
        Instruction::CallFunction {
            dst: Register(0),
            function: FunctionId(0),
            args_start: Register(0),
            arg_count: 0,
        },
        Some(SourceSpan::new(file, 39, 45)),
    );
    main.push_instruction(Instruction::Return { src: Register(0) });

    let mut program = Program::new(FunctionId(1));
    program.add_function(Function::bytecode(fail)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let err = Vm::new().run_program(&program).unwrap_err();

    assert_eq!(
        err.stack_trace,
        vec![
            VmStackFrame {
                function: FunctionId(0),
                name: "fail".to_string(),
                instruction_ip: Some(2),
            },
            VmStackFrame {
                function: FunctionId(1),
                name: "main".to_string(),
                instruction_ip: Some(0),
            },
        ]
    );

    let mut sources = SourceManager::new();
    sources.add_file(
        "stack.fx",
        "fn fail() {\n    return 1 / 0;\n}\nreturn fail();\n",
    );

    assert_eq!(
        sources.render_diagnostic(&err.to_diagnostic_with_program(program.as_program())),
        "\
error: division by zero at instruction 2
 --> stack.fx:2:12
  |
2 |     return 1 / 0;
  |            ^^^^^
stack trace:
  at fail (fn#0, instruction 2)
  at main (fn#1, instruction 0)
"
    );
}

#[test]
fn runs_recursive_factorial() {
    let mut fact = Chunk::new("fact", 6).with_arity(1);
    let zero = fact.add_constant(Value::Int(0)).unwrap();
    let one = fact.add_constant(Value::Int(1)).unwrap();
    fact.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: zero,
    });
    fact.push_instruction(Instruction::Equal {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    fact.push_instruction(Instruction::JumpIfFalse {
        condition: Register(2),
        target: JumpTarget(5),
    });
    fact.push_instruction(Instruction::LoadConst {
        dst: Register(3),
        constant: one,
    });
    fact.push_instruction(Instruction::Return { src: Register(3) });
    fact.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: one,
    });
    fact.push_instruction(Instruction::Sub {
        dst: Register(4),
        lhs: Register(0),
        rhs: Register(1),
    });
    fact.push_instruction(Instruction::CallFunction {
        dst: Register(5),
        function: FunctionId(0),
        args_start: Register(4),
        arg_count: 1,
    });
    fact.push_instruction(Instruction::Mul {
        dst: Register(5),
        lhs: Register(0),
        rhs: Register(5),
    });
    fact.push_instruction(Instruction::Return { src: Register(5) });

    let mut main = Chunk::new("main", 2);
    let five = main.add_constant(Value::Int(5)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: five,
    });
    main.push_instruction(Instruction::CallFunction {
        dst: Register(1),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 1,
    });
    main.push_instruction(Instruction::Return { src: Register(1) });

    let mut program = Program::new(FunctionId(1));
    program.add_function(Function::bytecode(fact)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let result = Vm::new().run_program(&program).unwrap();

    assert_eq!(result, Value::Int(120));
}

#[test]
fn call_depth_limit_is_enforced() {
    let mut recursive = Chunk::new("forever", 1);
    recursive.push_instruction(Instruction::CallFunction {
        dst: Register(0),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 0,
    });
    recursive.push_instruction(Instruction::Return { src: Register(0) });

    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(recursive)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();
    let mut vm = Vm::with_limits(RuntimeLimits {
        max_instruction_count: 1_000,
        max_call_depth: 4,
        max_heap_objects: 16,
        gc_allocation_threshold: 0,
        gc_incremental_step_budget: 64,
    });

    let err = vm.run_program(&program).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VmErrorKind::CallDepthExceeded { max_call_depth: 4 }
    );
}

#[test]
fn native_function_calls_are_supported() {
    let mut main = Chunk::new("main", 2);
    let twenty_one = main.add_constant(Value::Int(21)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: twenty_one,
    });
    main.push_instruction(Instruction::CallFunction {
        dst: Register(1),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 1,
    });
    main.push_instruction(Instruction::Return { src: Register(1) });

    let mut program = Program::new(FunctionId(1));
    program.add_function(Function::native("double", 1)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();
    let mut vm = Vm::new();
    vm.register_native_fn(FunctionId(0), |args| match args {
        [Value::Int(value)] => Ok(Value::Int(value * 2)),
        [found] => Err(VmError::new(
            None,
            VmErrorKind::TypeError {
                expected: "int",
                found: *found,
            },
        )),
        _ => unreachable!("program verifier enforces native arity"),
    });

    let result = vm.run_program(&program).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn missing_native_implementation_is_typed_error() {
    let mut main = Chunk::new("main", 2);
    let value = main.add_constant(Value::Int(21)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    main.push_instruction(Instruction::CallFunction {
        dst: Register(1),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 1,
    });
    main.push_instruction(Instruction::Return { src: Register(1) });

    let mut program = Program::new(FunctionId(1));
    program.add_function(Function::native("double", 1)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let err = Vm::new().run_program(&program).unwrap_err();

    assert_eq!(err.instruction_ip, Some(1));
    assert_eq!(
        err.kind,
        VmErrorKind::MissingNativeFunction {
            function: FunctionId(0),
        }
    );
}

#[derive(Clone, Copy)]
enum InstructionKind {
    Sub,
    Mul,
    Div,
}

fn run_binary(lhs: i64, rhs: i64, kind: InstructionKind) -> Value {
    let chunk = binary_chunk(lhs, rhs, kind);
    Vm::new().run(&chunk).unwrap()
}

fn run_binary_error(lhs: i64, rhs: i64, kind: InstructionKind) -> VmError {
    let chunk = binary_chunk(lhs, rhs, kind);
    Vm::new().run(&chunk).unwrap_err()
}

fn binary_chunk(lhs: i64, rhs: i64, kind: InstructionKind) -> VerifiedChunk {
    let mut chunk = Chunk::new("main", 3);
    let lhs_const = chunk.add_constant(Value::Int(lhs)).unwrap();
    let rhs_const = chunk.add_constant(Value::Int(rhs)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: lhs_const,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: rhs_const,
    });
    chunk.push_instruction(match kind {
        InstructionKind::Sub => Instruction::Sub {
            dst: Register(2),
            lhs: Register(0),
            rhs: Register(1),
        },
        InstructionKind::Mul => Instruction::Mul {
            dst: Register(2),
            lhs: Register(0),
            rhs: Register(1),
        },
        InstructionKind::Div => Instruction::Div {
            dst: Register(2),
            lhs: Register(0),
            rhs: Register(1),
        },
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });
    VerifiedChunk::new(chunk).unwrap()
}
