//! Tests for Ferrix binary bytecode encoding and decoding.

use ferrix_core::{
    Value,
    bytecode::{
        BytecodeContainerMetadata, BytecodeDecodeError, BytecodeSectionKind, CaptureId, Chunk,
        FEATURE_CUSTOM_EXTENSIONS, FEATURE_NATIVE_CALLS, Function, FunctionId, Instruction,
        JumpTarget, Program, Register, StringId, VerifiedProgram, decode_bytecode,
        decode_container, decode_program, encode_container, encode_program, inspect_container,
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
fn bytecode_container_wraps_program_with_metadata_and_sections() {
    let mut main = Chunk::new("main", 1);
    main.push_instruction(Instruction::Return { src: Register(0) });
    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();
    let metadata = BytecodeContainerMetadata::for_program(program.as_program())
        .with_module_name("demo")
        .with_required_capability("native.call");

    let bytes = encode_container(program.as_program(), Some(metadata)).unwrap();
    let container = decode_container(&bytes).unwrap();
    let inspected = inspect_container(&bytes).unwrap();

    assert_eq!(container.program.as_program(), program.as_program());
    assert_eq!(container.metadata.module_name.as_deref(), Some("demo"));
    assert_eq!(container.metadata.required_capabilities, ["native.call"]);
    assert_eq!(
        container.sections[0].kind,
        BytecodeSectionKind::ProgramPayload
    );
    assert_eq!(inspected.checksum, container.metadata.checksum);
}

#[test]
fn bytecode_container_decodes_through_compat_loader() {
    let mut main = Chunk::new("main", 1);
    main.push_instruction(Instruction::Return { src: Register(0) });
    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let bytes = encode_container(program.as_program(), None).unwrap();
    let decoded = decode_bytecode(&bytes).unwrap();

    assert_eq!(decoded.as_program(), program.as_program());
}

#[test]
fn bytecode_container_rejects_checksum_mismatch() {
    let mut main = Chunk::new("main", 1);
    main.push_instruction(Instruction::Return { src: Register(0) });
    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let mut bytes = encode_container(program.as_program(), None).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 0x01;

    assert!(matches!(
        decode_container(&bytes).unwrap_err(),
        BytecodeDecodeError::ChecksumMismatch { .. }
            | BytecodeDecodeError::InvalidBytecode(_)
            | BytecodeDecodeError::InvalidInstructionOpcode { .. }
    ));
}

#[test]
fn bytecode_container_preserves_declared_feature_flags() {
    let mut program = Program::new(FunctionId(0));
    let mut main = Chunk::new("main", 1);
    main.push_instruction(Instruction::Return { src: Register(0) });
    program.add_function(Function::bytecode(main)).unwrap();
    program.add_function(Function::native("clock", 0)).unwrap();
    program.format.feature_flags = FEATURE_CUSTOM_EXTENSIONS;
    let program = VerifiedProgram::new(program).unwrap();

    let bytes = encode_container(program.as_program(), None).unwrap();
    let metadata = inspect_container(&bytes).unwrap();

    assert!(metadata.feature_flags & FEATURE_CUSTOM_EXTENSIONS != 0);
    assert!(metadata.feature_flags & FEATURE_NATIVE_CALLS != 0);
}

#[test]
fn bytecode_program_roundtrips_custom_extension_instruction() {
    let mut main = Chunk::new("main", 2);
    let extension = main.add_string("math.double").unwrap();
    let value = main.add_constant(Value::Int(21)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    main.push_instruction(Instruction::CallExtension {
        dst: Register(1),
        extension,
        args_start: Register(0),
        arg_count: 1,
    });
    main.push_instruction(Instruction::Return { src: Register(1) });
    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let bytes = encode_container(program.as_program(), None).unwrap();
    let decoded = decode_container(&bytes).unwrap();
    let metadata = inspect_container(&bytes).unwrap();

    assert_eq!(decoded.program.as_program(), program.as_program());
    assert!(metadata.feature_flags & FEATURE_CUSTOM_EXTENSIONS != 0);
    assert!(matches!(
        decoded.program.as_program().functions[0]
            .chunk()
            .unwrap()
            .instructions[1],
        Instruction::CallExtension {
            extension: StringId(0),
            arg_count: 1,
            ..
        }
    ));
}

#[test]
fn bytecode_program_roundtrips_integer_specialized_instructions() {
    let mut main = Chunk::new("main", 6);
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
    main.push_instruction(Instruction::AddInt {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    main.push_instruction(Instruction::SubInt {
        dst: Register(3),
        lhs: Register(2),
        rhs: Register(1),
    });
    main.push_instruction(Instruction::MulInt {
        dst: Register(4),
        lhs: Register(3),
        rhs: Register(1),
    });
    main.push_instruction(Instruction::DivInt {
        dst: Register(5),
        lhs: Register(4),
        rhs: Register(1),
    });
    main.push_instruction(Instruction::LessEqualInt {
        dst: Register(5),
        lhs: Register(3),
        rhs: Register(5),
    });
    main.push_instruction(Instruction::Return { src: Register(5) });

    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let bytes = encode_program(program.as_program()).unwrap();
    let decoded = decode_program(&bytes).unwrap();

    assert_eq!(decoded.as_program(), program.as_program());
}

#[test]
fn bytecode_program_roundtrips_closure_instructions() {
    let mut closure = Chunk::new("closure#0", 4)
        .with_arity(1)
        .with_capture_count(1);
    closure.push_instruction(Instruction::LoadCaptureCell {
        dst: Register(1),
        capture: CaptureId(0),
    });
    closure.push_instruction(Instruction::LoadCapture {
        dst: Register(2),
        capture: CaptureId(0),
    });
    closure.push_instruction(Instruction::StoreCapture {
        capture: CaptureId(0),
        src: Register(2),
    });
    closure.push_instruction(Instruction::Add {
        dst: Register(3),
        lhs: Register(2),
        rhs: Register(0),
    });
    closure.push_instruction(Instruction::Return { src: Register(3) });

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
    main.push_instruction(Instruction::LoadUpvalue {
        dst: Register(3),
        upvalue: Register(0),
    });
    main.push_instruction(Instruction::StoreUpvalue {
        upvalue: Register(0),
        src: Register(3),
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
fn bytecode_program_roundtrips_error_handling_instructions() {
    let mut main = Chunk::new("main", 2);
    let value = main.add_constant(Value::Int(42)).unwrap();
    main.push_instruction(Instruction::PushHandler {
        error: Register(1),
        target: JumpTarget(4),
    });
    main.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    main.push_instruction(Instruction::Throw { src: Register(0) });
    main.push_instruction(Instruction::PopHandler);
    main.push_instruction(Instruction::Return { src: Register(1) });

    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(main)).unwrap();
    let program = VerifiedProgram::new(program).unwrap();

    let bytes = encode_program(program.as_program()).unwrap();
    let decoded = decode_program(&bytes).unwrap();

    assert_eq!(decoded.as_program(), program.as_program());
}

#[test]
fn bytecode_program_roundtrips_record_instructions() {
    let mut main = Chunk::new("main", 4);
    let name = main.add_string("name").unwrap();
    let value = main.add_constant(Value::Int(42)).unwrap();
    main.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: value,
    });
    main.push_instruction(Instruction::RecordNew {
        dst: Register(0),
        fields_start: Register(1),
        fields: vec![name],
    });
    main.push_instruction(Instruction::FieldGet {
        dst: Register(2),
        target: Register(0),
        field: name,
    });
    main.push_instruction(Instruction::FieldSet {
        target: Register(0),
        field: name,
        value: Register(2),
    });
    main.push_instruction(Instruction::Return { src: Register(2) });

    let mut program = Program::new(FunctionId(0));
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
