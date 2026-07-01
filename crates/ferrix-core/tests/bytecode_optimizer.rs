//! Tests for verifier-safe bytecode optimization rewrites.

use ferrix_core::{
    Value,
    bytecode::{
        Chunk, Instruction, JumpTarget, Register, StructuralVerifier, optimize_chunk,
        optimize_chunk_with_report,
    },
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
fn optimization_report_records_pass_metrics() {
    let mut chunk = Chunk::new("report", 3);
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

    let optimized = optimize_chunk_with_report(chunk);

    assert_eq!(optimized.report.chunk_name, "report");
    assert_eq!(optimized.report.instructions_before, 4);
    assert_eq!(optimized.report.instructions_after, 4);
    assert_eq!(optimized.report.passes.len(), 6);
    assert!(optimized.report.total_transformations() >= 1);
    assert!(
        optimized
            .report
            .passes
            .iter()
            .any(|pass| pass.name == "constant-folding" && pass.changed)
    );
    StructuralVerifier::verify(optimized.chunk).unwrap();
}

#[test]
fn folds_specialized_constant_integer_arithmetic() {
    let mut chunk = Chunk::new("fold-specialized", 3);
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
    chunk.push_instruction(Instruction::AddInt {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });

    let optimized = optimize_chunk(chunk);

    let Instruction::LoadConst { constant, .. } = optimized.instructions[2] else {
        panic!("specialized arithmetic should fold to a constant load");
    };
    assert_eq!(optimized.constants[usize::from(constant.0)], Value::Int(42));
    StructuralVerifier::verify(optimized).unwrap();
}

#[test]
fn specializes_unfolded_integer_dispatch_opcodes() {
    let mut chunk = Chunk::new("specialize", 3);
    chunk.push_instruction(Instruction::Add {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Less {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });

    let optimized = optimize_chunk(chunk);

    assert!(matches!(
        optimized.instructions[0],
        Instruction::AddInt {
            dst: Register(2),
            lhs: Register(0),
            rhs: Register(1),
        }
    ));
    assert!(matches!(
        optimized.instructions[1],
        Instruction::LessInt {
            dst: Register(2),
            lhs: Register(0),
            rhs: Register(1),
        }
    ));
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
