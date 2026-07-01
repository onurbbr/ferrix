//! Tests for assembler builder ergonomics and label patching.

use ferrix_assembler::{Assembler, AssemblerError};
use ferrix_core::{
    Value,
    bytecode::{ConstId, FunctionId, Instruction, JumpTarget, Register, VerificationErrorKind},
};
use ferrix_vm::Vm;

#[test]
fn builder_generates_verified_chunk() {
    let chunk = Assembler::new("main")
        .registers(3)
        .int(10)
        .int(32)
        .load_const(0, 0)
        .load_const(1, 1)
        .add(2, 0, 1)
        .ret(2)
        .finish()
        .unwrap();

    let chunk = chunk.as_chunk();

    assert_eq!(chunk.name, "main");
    assert_eq!(chunk.register_count, 3);
    assert_eq!(chunk.constants, vec![Value::Int(10), Value::Int(32)]);
    assert_eq!(
        chunk.instructions,
        vec![
            Instruction::LoadConst {
                dst: Register(0),
                constant: ConstId(0),
            },
            Instruction::LoadConst {
                dst: Register(1),
                constant: ConstId(1),
            },
            Instruction::Add {
                dst: Register(2),
                lhs: Register(0),
                rhs: Register(1),
            },
            Instruction::Return { src: Register(2) },
        ]
    );
}

#[test]
fn assembled_chunk_runs_on_vm() {
    let chunk = Assembler::new("main")
        .registers(3)
        .int(10)
        .int(32)
        .load_const(0, 0)
        .load_const(1, 1)
        .add(2, 0, 1)
        .ret(2)
        .finish()
        .unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn builder_supports_move_and_arithmetic_opcodes() {
    let chunk = Assembler::new("math")
        .registers(5)
        .int(8)
        .int(2)
        .load_const(0, 0)
        .load_const(1, 1)
        .mov(2, 0)
        .sub(3, 2, 1)
        .mul(3, 3, 1)
        .div(4, 3, 1)
        .ret(4)
        .finish()
        .unwrap();

    assert_eq!(chunk.as_chunk().instructions.len(), 7);
}

#[test]
fn builder_supports_bool_and_nil_constants() {
    let chunk = Assembler::new("constants")
        .registers(2)
        .bool(true)
        .nil()
        .load_const(0, 0)
        .load_const(1, 1)
        .ret(1)
        .finish()
        .unwrap();

    assert_eq!(
        chunk.as_chunk().constants,
        vec![Value::Bool(true), Value::Nil]
    );
}

#[test]
fn builder_supports_string_pool_and_load_string() {
    let chunk = Assembler::new("strings")
        .registers(1)
        .string("hello")
        .load_string(0, 0)
        .ret(0)
        .finish()
        .unwrap();

    assert_eq!(chunk.as_chunk().strings, vec!["hello".to_string()]);
    assert!(matches!(
        chunk.as_chunk().instructions[0],
        Instruction::LoadString { .. }
    ));
}

#[test]
fn builder_supports_record_and_field_opcodes() {
    let chunk = Assembler::new("records")
        .registers(4)
        .string("answer")
        .int(42)
        .load_const(0, 0)
        .record_new(1, 0, vec![0])
        .field_get(2, 1, 0)
        .field_set(1, 0, 2)
        .ret(2)
        .finish()
        .unwrap();

    assert!(matches!(
        chunk.as_chunk().instructions[1],
        Instruction::RecordNew { .. }
    ));

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn label_patching_builds_if_else_program() {
    let chunk = Assembler::new("if_else")
        .registers(2)
        .bool(false)
        .int(10)
        .int(20)
        .load_const(0, 0)
        .jump_if_false(0, "else")
        .load_const(1, 1)
        .jump("end")
        .label("else")
        .load_const(1, 2)
        .label("end")
        .ret(1)
        .finish()
        .unwrap();

    assert_eq!(
        chunk.as_chunk().instructions[1],
        Instruction::JumpIfFalse {
            condition: Register(0),
            target: JumpTarget(4),
        }
    );
    assert_eq!(
        chunk.as_chunk().instructions[3],
        Instruction::Jump {
            target: JumpTarget(5),
        }
    );

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(20));
}

#[test]
fn label_patching_builds_counter_loop() {
    let chunk = Assembler::new("loop")
        .registers(6)
        .int(0)
        .int(1)
        .int(5)
        .load_const(0, 0)
        .load_const(1, 0)
        .load_const(2, 1)
        .load_const(3, 2)
        .label("loop")
        .less(4, 0, 3)
        .jump_if_false(4, "end")
        .add(1, 1, 0)
        .add(0, 0, 2)
        .jump("loop")
        .label("end")
        .ret(1)
        .finish()
        .unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(10));
}

#[test]
fn builder_supports_absolute_jump_targets() {
    let chunk = Assembler::new("absolute")
        .registers(1)
        .bool(true)
        .load_const(0, 0)
        .jump_if_true_to(0, 3)
        .jump_to(0)
        .ret(0)
        .finish()
        .unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Bool(true));
}

#[test]
fn builder_supports_direct_function_call_instruction() {
    let chunk = Assembler::new("caller")
        .registers(2)
        .call_function(1, 7, 0, 1)
        .ret(1)
        .finish_unverified()
        .unwrap();

    assert_eq!(
        chunk.instructions[0],
        Instruction::CallFunction {
            dst: Register(1),
            function: FunctionId(7),
            args_start: Register(0),
            arg_count: 1,
        }
    );
}

#[test]
fn builder_supports_array_instructions() {
    let chunk = Assembler::new("arrays")
        .registers(5)
        .int(0)
        .int(1)
        .int(42)
        .load_const(0, 0)
        .load_const(1, 1)
        .array_new(2, 1, 1)
        .load_const(3, 2)
        .array_set(2, 0, 3)
        .array_get(4, 2, 0)
        .ret(4)
        .finish()
        .unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn builder_supports_map_and_index_instructions() {
    let chunk = Assembler::new("maps")
        .registers(5)
        .int(7)
        .int(1)
        .int(42)
        .load_const(0, 0)
        .load_const(1, 1)
        .map_new(2, 0, 1)
        .load_const(3, 2)
        .index_set(2, 0, 3)
        .index_get(4, 2, 0)
        .ret(4)
        .finish()
        .unwrap();

    let result = Vm::new().run(&chunk).unwrap();

    assert_eq!(result, Value::Int(42));
}

#[test]
fn undefined_labels_are_errors() {
    let err = Assembler::new("bad")
        .registers(1)
        .jump("missing")
        .ret(0)
        .finish()
        .unwrap_err();

    assert_eq!(
        err,
        AssemblerError::UndefinedLabel {
            label: "missing".to_string(),
        }
    );
}

#[test]
fn duplicate_labels_are_errors() {
    let err = Assembler::new("bad")
        .registers(1)
        .label("same")
        .label("same")
        .ret(0)
        .finish()
        .unwrap_err();

    assert_eq!(
        err,
        AssemblerError::DuplicateLabel {
            label: "same".to_string(),
        }
    );
}

#[test]
fn finish_returns_verifier_errors() {
    let err = Assembler::new("bad")
        .registers(1)
        .int(1)
        .load_const(1, 0)
        .ret(0)
        .finish()
        .unwrap_err();

    assert!(matches!(
        err,
        AssemblerError::Verification(ferrix_core::bytecode::VerificationError {
            instruction_ip: Some(0),
            kind: VerificationErrorKind::InvalidRegister {
                register: Register(1),
                register_count: 1,
            },
            ..
        })
    ));
}

#[test]
fn finish_unverified_keeps_raw_chunk_available_for_debugging() {
    let chunk = Assembler::new("raw")
        .registers(1)
        .load_const(0, 0)
        .finish_unverified()
        .unwrap();

    assert_eq!(chunk.instructions.len(), 1);
}
