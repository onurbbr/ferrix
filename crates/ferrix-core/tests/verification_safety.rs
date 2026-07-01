//! Tests for malformed bytecode/program safety boundaries.

use ferrix_core::{
    Value,
    bytecode::{
        BytecodeFormat, Chunk, ConstId, Function, FunctionId, FunctionKind, Instruction,
        JumpTarget, Program, Register, StructuralVerifier, VerificationErrorKind, VerifiedProgram,
    },
    diagnostics::{FileId, SourceSpan},
};

fn assert_chunk_rejected(chunk: Chunk, expected: VerificationErrorKind) {
    let err = StructuralVerifier::verify(chunk).unwrap_err();

    assert_eq!(err.kind, expected);
}

#[test]
fn structural_verifier_rejects_invalid_register_operands() {
    let mut chunk = Chunk::new("bad_register", 1);
    chunk.push_instruction(Instruction::Return { src: Register(1) });

    assert_chunk_rejected(
        chunk,
        VerificationErrorKind::InvalidRegister {
            register: Register(1),
            register_count: 1,
        },
    );
}

#[test]
fn structural_verifier_rejects_invalid_constant_operands() {
    let mut chunk = Chunk::new("bad_constant", 1);
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: ConstId(0),
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    assert_chunk_rejected(
        chunk,
        VerificationErrorKind::InvalidConstant {
            constant: ConstId(0),
            constant_count: 0,
        },
    );
}

#[test]
fn structural_verifier_rejects_invalid_jump_targets() {
    let mut chunk = Chunk::new("bad_jump", 1);
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(99),
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    assert_chunk_rejected(
        chunk,
        VerificationErrorKind::InvalidJumpTarget {
            target: JumpTarget(99),
            instruction_count: 2,
        },
    );
}

#[test]
fn structural_verifier_rejects_source_map_mismatches() {
    let mut chunk = Chunk::new("bad_source_map", 1);
    chunk
        .instructions
        .push(Instruction::Return { src: Register(0) });

    assert_chunk_rejected(
        chunk,
        VerificationErrorKind::SourceMapLengthMismatch {
            instructions: 1,
            source_map: 0,
        },
    );
}

#[test]
fn structural_verifier_accepts_source_map_entries_that_match_instructions() {
    let mut chunk = Chunk::new("mapped", 1);
    chunk.push_instruction_with_span(
        Instruction::Return { src: Register(0) },
        Some(SourceSpan::new(FileId(0), 0, 6)),
    );

    assert!(StructuralVerifier::verify(chunk).is_ok());
}

#[test]
fn program_verifier_rejects_invalid_entrypoint() {
    let mut program = Program::new(FunctionId(1));
    program
        .add_function(Function::bytecode(returning_chunk("main")))
        .unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::InvalidEntrypoint {
            entry: FunctionId(1),
            function_count: 1,
        }
    );
}

#[test]
fn program_verifier_rejects_invalid_bytecode_format_before_execution() {
    let mut program = Program::new(FunctionId(0)).with_format(BytecodeFormat {
        magic: "NOPE",
        version: 99,
        feature_flags: 0,
    });
    program
        .add_function(Function::bytecode(returning_chunk("main")))
        .unwrap();

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
fn program_verifier_rejects_invalid_direct_call_function_id() {
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

    assert_eq!(
        err.kind,
        VerificationErrorKind::InvalidFunction {
            function: FunctionId(9),
            function_count: 1,
        }
    );
}

#[test]
fn program_verifier_rejects_direct_call_arity_mismatch() {
    let mut callee = Chunk::new("callee", 1).with_arity(1);
    callee.push_instruction(Instruction::Return { src: Register(0) });

    let mut main = Chunk::new("main", 1);
    main.push_instruction(Instruction::CallFunction {
        dst: Register(0),
        function: FunctionId(0),
        args_start: Register(0),
        arg_count: 0,
    });
    main.push_instruction(Instruction::Return { src: Register(0) });

    let mut program = Program::new(FunctionId(1));
    program.add_function(Function::bytecode(callee)).unwrap();
    program.add_function(Function::bytecode(main)).unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::WrongCallArity {
            function: FunctionId(0),
            expected: 1,
            actual: 0,
        }
    );
}

#[test]
fn program_verifier_rejects_native_metadata_mismatches() {
    let function = Function {
        name: "public".to_string(),
        arity: 1,
        register_count: 1,
        kind: FunctionKind::Native {
            name: "internal".to_string(),
        },
    };
    let mut program = Program::new(FunctionId(0));
    program.add_function(function).unwrap();

    let err = VerifiedProgram::new(program).unwrap_err();

    assert_eq!(
        err.kind,
        VerificationErrorKind::FunctionMetadataMismatch { field: "name" }
    );
}

fn returning_chunk(name: &str) -> Chunk {
    let mut chunk = Chunk::new(name, 1);
    let value = chunk.add_constant(Value::Int(0)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    chunk
}
