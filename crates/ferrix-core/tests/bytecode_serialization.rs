//! Tests for Ferrix binary bytecode encoding and decoding.

use ferrix_core::{
    Value,
    bytecode::{
        BytecodeDecodeError, CaptureId, Chunk, Function, FunctionId, Instruction, Program,
        Register, VerifiedProgram, decode_program, encode_program,
    },
};

#[test]
fn bytecode_program_roundtrips_through_binary_format() {
    let mut main = Chunk::new("main", 3);
    let forty = main.add_constant(Value::Int(40)).unwrap();
    let two = main.add_constant(Value::Int(2)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: forty,
    });
    main.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: two,
    });
    main.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    main.push_instruction(Instruction::Return { src: Register(2) });
    main.set_debug_local_name(Register(0), "forty");

    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let bytes = encode_program(program.as_program()).unwrap();
    let decoded = decode_program(&bytes).unwrap();

    assert_eq!(decoded.as_program(), program.as_program());
}

#[test]
fn bytecode_program_roundtrips_closure_instructions() {
    let mut closure = Chunk::new("closure#0", 3)
        .with_arity(1)
        .with_capture_count(1);
    closure.push_instruction(Instruction::LoadCapture {
        dst: Register(1),
        capture: CaptureId(0),
    });
    closure.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(1),
        rhs: Register(0),
    });
    closure.push_instruction(Instruction::Return { src: Register(2) });

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
    program.add_function(Function::bytecode(closure)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let bytes = encode_program(program.as_program()).unwrap();
    let decoded = decode_program(&bytes).unwrap();

    assert_eq!(decoded.as_program(), program.as_program());
}

#[test]
fn bytecode_decode_rejects_invalid_magic() {
    let err = decode_program(b"not ferrix").unwrap_err();

    assert_eq!(err, BytecodeDecodeError::InvalidMagic);
}
