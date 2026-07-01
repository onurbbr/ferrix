//! Tests for verifier-safe bytecode optimization rewrites.

use ferrix_core::{
    Value,
    bytecode::{Chunk, Instruction, JumpTarget, Register, StructuralVerifier, optimize_chunk},
};

#[test]
fn folds_constant_integer_arithmetic() {
    let mut chunk = Chunk::new("fold", 3);
    let forty = chunk.add_constant(Value::Int(40)).unwrap();
    let two = chunk.add_constant(Value::Int(2)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: forty,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: two,
    });
    chunk.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });

    let optimized = optimize_chunk(chunk);

    assert!(matches!(
        optimized.instructions[2],
        Instruction::LoadConst {
            dst: Register(2),
            ..
        }
    ));
    let Instruction::LoadConst { constant, .. } = optimized.instructions[2] else {
        unreachable!("asserted load const");
    };
    assert_eq!(optimized.constants[usize::from(constant.0)], Value::Int(42));
    StructuralVerifier::verify(optimized).unwrap();
}

#[test]
fn removes_redundant_moves_and_unreachable_instructions() {
    let mut chunk = Chunk::new("compact", 1);
    let value = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::Move {
        dst: Register(0),
        src: Register(0),
    });
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(5),
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let optimized = optimize_chunk(chunk);

    assert_eq!(
        optimized.instructions,
        vec![
            Instruction::LoadConst {
                dst: Register(0),
                constant: value,
            },
            Instruction::Jump {
                target: JumpTarget(2),
            },
            Instruction::Return { src: Register(0) },
        ]
    );
    StructuralVerifier::verify(optimized).unwrap();
}

#[test]
fn collapses_jump_to_jump_chains() {
    let mut chunk = Chunk::new("jumps", 1);
    let value = chunk.add_constant(Value::Int(42)).unwrap();
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(1),
    });
    chunk.push_instruction(Instruction::Jump {
        target: JumpTarget(2),
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: value,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let optimized = optimize_chunk(chunk);

    assert!(matches!(
        optimized.instructions[0],
        Instruction::Jump {
            target: JumpTarget(1)
        }
    ));
    StructuralVerifier::verify(optimized).unwrap();
}
