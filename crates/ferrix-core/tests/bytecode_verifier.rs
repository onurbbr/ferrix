//! Tests for structural and program-level bytecode verification.

use ferrix_core::{
    Value,
    bytecode::{
        BytecodeFormat, CaptureId, Chunk, ConstId, Function, FunctionId, FunctionKind, Instruction,
        JumpTarget, Program, Register, StringId, StructuralVerifier, VerificationErrorKind,
        VerifiedProgram,
    },
};

#[test]
fn verifies_valid_chunk() {
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

    let verified = StructuralVerifier::verify(chunk).unwrap();

    assert_eq!(verified.as_chunk().register_count, 3);
}

#[test]
fn rejects_invalid_register() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::Return { src: Register(1) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VerificationErrorKind::InvalidRegister {
            register: Register(1),
            register_count: 1,
        }
    );
}

#[test]
fn rejects_invalid_constant() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: ConstId(0),
    });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VerificationErrorKind::InvalidConstant {
            constant: ConstId(0),
            constant_count: 0,
        }
    );
}

#[test]
fn rejects_invalid_string() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::LoadString {
        dst: Register(0),
        string: StringId(0),
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VerificationErrorKind::InvalidString {
            string: StringId(0),
            string_count: 0,
        }
    );
}

#[test]
fn rejects_arity_that_does_not_fit_register_file() {
    let chunk = Chunk::new("main", 1).with_arity(2);

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::ArityExceedsRegisterCount {
            arity: 2,
            register_count: 1,
        }
    );
}

#[test]
fn rejects_chunk_without_return() {
    let chunk = Chunk::new("main", 1);

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(err.instruction_ip, None);
    assert_eq!(err.kind, VerificationErrorKind::MissingReturn);
}

#[test]
fn rejects_invalid_jump_target() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(2),
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VerificationErrorKind::InvalidJumpTarget {
            target: JumpTarget(2),
            instruction_count: 2,
        }
    );
}

#[test]
fn rejects_debug_local_names_outside_register_file() {
    let mut chunk = Chunk::new("main", 1);
    chunk.set_debug_local_name(Register(1), "too_far");
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::DebugLocalNamesOutOfRange {
            debug_local_names: 2,
            register_count: 1,
        }
    );
}

#[test]
fn verifies_program_with_direct_call() {
    let mut callee = Chunk::new("id", 1).with_arity(1);
    callee.push_instruction(Instruction::Return { src: Register(0) });

    let mut main = Chunk::new("main", 2);
    let value = main.add_constant(Value::Int(42)).unwrap();
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
    program.add_function(Function::bytecode(callee)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();

    let verified = VerifiedProgram::new(program).unwrap();

    assert_eq!(verified.as_program().entry, FunctionId(1));
}

#[test]
fn verifies_program_with_closure_call() {
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

    let verified = VerifiedProgram::new(program).unwrap();

    assert_eq!(verified.as_program().entry, FunctionId(1));
}

#[test]
fn rejects_invalid_capture_operand() {
    let mut chunk = Chunk::new("closure#0", 2)
        .with_arity(1)
        .with_capture_count(1);
    chunk.push_instruction(Instruction::LoadCapture {
        dst: Register(1),
        capture: CaptureId(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(1) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VerificationErrorKind::InvalidCapture {
            capture: CaptureId(1),
            capture_count: 1,
        }
    );
}

#[test]
fn rejects_closure_captures_that_exceed_register_file() {
    let mut chunk = Chunk::new("main", 2);
    chunk.push_instruction(Instruction::MakeClosure {
        dst: Register(0),
        function: FunctionId(0),
        captures_start: Register(1),
        capture_count: 2,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VerificationErrorKind::ClosureCapturesOutOfRange {
            captures_start: Register(1),
            capture_count: 2,
            register_count: 2,
        }
    );
}

#[test]
fn rejects_unsupported_bytecode_format() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let mut program = Program::new(FunctionId(0)).with_format(BytecodeFormat {
        magic: "NOPE",
        version: 99,
        feature_flags: 0,
    });
    program.add_function(Function::bytecode(chunk)).unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::UnsupportedBytecodeFormat {
            magic: "NOPE".to_string(),
            version: 99,
        }
    );
}

#[test]
fn rejects_function_chunk_metadata_mismatch() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let function = Function {
        name: "renamed".to_string(),
        arity: 0,
        register_count: 1,
        capture_count: 0,
        kind: FunctionKind::Bytecode(chunk),
    };
    let mut program = Program::new(FunctionId(0));
    program.add_function(function).unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(err.function_id, Some(FunctionId(0)));
    assert_eq!(
        err.kind,
        VerificationErrorKind::FunctionMetadataMismatch { field: "name" }
    );
}

#[test]
fn rejects_native_function_metadata_mismatch() {
    let function = Function {
        name: "display_name".to_string(),
        arity: 1,
        register_count: 1,
        capture_count: 0,
        kind: FunctionKind::Native {
            name: "native_name".to_string(),
        },
    };
    let mut program = Program::new(FunctionId(0));
    program.add_function(function).unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(err.function_id, Some(FunctionId(0)));
    assert_eq!(
        err.kind,
        VerificationErrorKind::FunctionMetadataMismatch { field: "name" }
    );
}

#[test]
fn rejects_native_register_count_that_does_not_match_arity() {
    let function = Function {
        name: "clock".to_string(),
        arity: 0,
        register_count: 1,
        capture_count: 0,
        kind: FunctionKind::Native {
            name: "clock".to_string(),
        },
    };
    let mut program = Program::new(FunctionId(0));
    program.add_function(function).unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(err.function_id, Some(FunctionId(0)));
    assert_eq!(
        err.kind,
        VerificationErrorKind::FunctionMetadataMismatch {
            field: "register_count",
        }
    );
}

#[test]
fn rejects_invalid_function_reference() {
    let mut main = Chunk::new("main", 1);
    main.push_instruction(Instruction::CallFunction {
        dst: Register(0),
        function: FunctionId(9),
        args_start: Register(0),
        arg_count: 0,
    });
    main.push_instruction(Instruction::Return { src: Register(0) });

    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(err.function_id, Some(FunctionId(0)));
    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VerificationErrorKind::InvalidFunction {
            function: FunctionId(9),
            function_count: 1,
        }
    );
}

#[test]
fn rejects_wrong_direct_call_arity() {
    let mut callee = Chunk::new("pair", 2).with_arity(2);
    callee.push_instruction(Instruction::Return { src: Register(0) });

    let mut main = Chunk::new("main", 1);
    main.push_instruction(Instruction::CallFunction {
        dst: Register(0),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 1,
    });
    main.push_instruction(Instruction::Return { src: Register(0) });

    let mut program = Program::new(FunctionId(1));
    program.add_function(Function::bytecode(callee)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(err.function_id, Some(FunctionId(1)));
    assert_eq!(err.instruction_ip, Some(0));
    assert_eq!(
        err.kind,
        VerificationErrorKind::WrongCallArity {
            function: FunctionId(0),
            expected: 2,
            actual: 1,
        }
    );
}

#[test]
fn rejects_call_arguments_that_exceed_register_file() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::CallFunction {
        dst: Register(0),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 2,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::CallArgumentsOutOfRange {
            args_start: Register(0),
            arg_count: 2,
            register_count: 1,
        }
    );
}

#[test]
fn verifies_array_instructions() {
    let mut chunk = Chunk::new("array", 5);
    let zero = chunk.add_constant(Value::Int(0)).unwrap();
    let value = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: zero,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: value,
    });
    chunk.push_instruction(Instruction::ArrayNew {
        dst: Register(2),
        elements_start: Register(1),
        element_count: 1,
    });
    chunk.push_instruction(Instruction::ArrayGet {
        dst: Register(3),
        array: Register(2),
        index: Register(0),
    });
    chunk.push_instruction(Instruction::ArraySet {
        array: Register(2),
        index: Register(0),
        value: Register(3),
    });
    chunk.push_instruction(Instruction::Return { src: Register(3) });

    StructuralVerifier::verify(chunk).unwrap();
}

#[test]
fn rejects_array_elements_that_exceed_register_file() {
    let mut chunk = Chunk::new("array", 2);
    chunk.push_instruction(Instruction::ArrayNew {
        dst: Register(0),
        elements_start: Register(1),
        element_count: 2,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::ArrayElementsOutOfRange {
            elements_start: Register(1),
            element_count: 2,
            register_count: 2,
        }
    );
}

#[test]
fn verifies_map_and_index_instructions() {
    let mut chunk = Chunk::new("map", 5);
    let key = chunk.add_constant(Value::Int(7)).unwrap();
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
    chunk.push_instruction(Instruction::IndexGet {
        dst: Register(3),
        target: Register(2),
        index: Register(0),
    });
    chunk.push_instruction(Instruction::IndexSet {
        target: Register(2),
        index: Register(0),
        value: Register(3),
    });
    chunk.push_instruction(Instruction::Return { src: Register(3) });

    StructuralVerifier::verify(chunk).unwrap();
}

#[test]
fn rejects_map_entries_that_exceed_register_file() {
    let mut chunk = Chunk::new("map", 2);
    chunk.push_instruction(Instruction::MapNew {
        dst: Register(0),
        entries_start: Register(1),
        entry_count: 1,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::MapEntriesOutOfRange {
            entries_start: Register(1),
            entry_count: 1,
            register_count: 2,
        }
    );
}
