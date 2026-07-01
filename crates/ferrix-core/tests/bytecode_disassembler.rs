//! Golden-style tests for bytecode disassembly output.

use ferrix_core::{
    Value,
    bytecode::{
        CaptureId, Chunk, Disassembler, Function, FunctionId, Instruction, Program, Register,
    },
};

#[test]
fn disassembles_chunk_deterministically() {
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

    let output = Disassembler::disassemble_chunk(&chunk);

    assert_eq!(
        output,
        "\
== main ==
format: FERRIXBC v1
registers: 3
arity: 0
captures: 0
constants:
  #0 Int(10)
  #1 Int(32)

0000 LoadConst   r0, #0 ; Int(10)
0001 LoadConst   r1, #1 ; Int(32)
0002 Add         r2, r0, r1
0003 Return      r2
"
    );
}

#[test]
fn disassembles_empty_constant_pool() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let output = Disassembler::disassemble_chunk(&chunk);

    assert!(output.contains("constants:\n  <empty>\n"));
}

#[test]
fn disassembles_string_pool_and_load_string() {
    let mut chunk = Chunk::new("strings", 1);
    let string = chunk.add_string("hello").unwrap();
    chunk.push_instruction(Instruction::LoadString {
        dst: Register(0),
        string,
    });
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let output = Disassembler::disassemble_chunk(&chunk);

    assert!(output.contains("strings:\n  str#0 \"hello\"\n"));
    assert!(output.contains("0000 LoadString  r0, str#0 ; \"hello\"\n"));
}

#[test]
fn disassembles_array_instructions() {
    let mut chunk = Chunk::new("arrays", 4);
    chunk.push_instruction(Instruction::ArrayNew {
        dst: Register(0),
        elements_start: Register(1),
        element_count: 2,
    });
    chunk.push_instruction(Instruction::ArrayGet {
        dst: Register(3),
        array: Register(0),
        index: Register(1),
    });
    chunk.push_instruction(Instruction::ArraySet {
        array: Register(0),
        index: Register(1),
        value: Register(2),
    });
    chunk.push_instruction(Instruction::Return { src: Register(3) });

    let output = Disassembler::disassemble_chunk(&chunk);

    assert!(output.contains("0000 ArrayNew    r0, r1, 2\n"));
    assert!(output.contains("0001 ArrayGet    r3, r0, r1\n"));
    assert!(output.contains("0002 ArraySet    r0, r1, r2\n"));
}

#[test]
fn disassembles_map_and_index_instructions() {
    let mut chunk = Chunk::new("maps", 4);
    chunk.push_instruction(Instruction::MapNew {
        dst: Register(0),
        entries_start: Register(1),
        entry_count: 1,
    });
    chunk.push_instruction(Instruction::IndexGet {
        dst: Register(3),
        target: Register(0),
        index: Register(1),
    });
    chunk.push_instruction(Instruction::IndexSet {
        target: Register(0),
        index: Register(1),
        value: Register(2),
    });
    chunk.push_instruction(Instruction::Return { src: Register(3) });

    let output = Disassembler::disassemble_chunk(&chunk);

    assert!(output.contains("0000 MapNew      r0, r1, 1\n"));
    assert!(output.contains("0001 IndexGet    r3, r0, r1\n"));
    assert!(output.contains("0002 IndexSet    r0, r1, r2\n"));
}

#[test]
fn disassembles_upvalue_and_closure_instructions() {
    let mut chunk = Chunk::new("closures", 4).with_capture_count(1);
    chunk.push_instruction(Instruction::MakeUpvalue {
        dst: Register(0),
        src: Register(1),
    });
    chunk.push_instruction(Instruction::LoadUpvalue {
        dst: Register(2),
        upvalue: Register(0),
    });
    chunk.push_instruction(Instruction::StoreUpvalue {
        upvalue: Register(0),
        src: Register(2),
    });
    chunk.push_instruction(Instruction::LoadCaptureCell {
        dst: Register(3),
        capture: CaptureId(0),
    });
    chunk.push_instruction(Instruction::StoreCapture {
        capture: CaptureId(0),
        src: Register(2),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });

    let output = Disassembler::disassemble_chunk(&chunk);

    assert!(output.contains("0000 MakeUpvalue r0, r1\n"));
    assert!(output.contains("0001 LoadUpvalue r2, r0\n"));
    assert!(output.contains("0002 StoreUpvalue r0, r2\n"));
    assert!(output.contains("0003 LoadCaptureCell r3, cap#0\n"));
    assert!(output.contains("0004 StoreCapture cap#0, r2\n"));
}

#[test]
fn disassembles_debug_local_names() {
    let mut chunk = Chunk::new("main", 1);
    chunk.set_debug_local_name(Register(0), "answer");
    chunk.push_instruction(Instruction::Return { src: Register(0) });

    let output = Disassembler::disassemble_chunk(&chunk);

    assert!(output.contains("locals:\n  r0 answer\n"));
}

#[test]
fn disassembles_program_header() {
    let mut chunk = Chunk::new("main", 1);
    chunk.push_instruction(Instruction::Return { src: Register(0) });
    let mut program = Program::new(FunctionId(0));
    program.add_function(Function::bytecode(chunk)).unwrap();

    let output = Disassembler::disassemble_program(&program);

    assert!(output.contains("== program ==\nformat: FERRIXBC v1 flags=0\n"));
    assert!(output.contains("entry: fn#0\nfunctions: 1\n"));
}
