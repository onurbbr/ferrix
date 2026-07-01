//! Golden output tests for diagnostics and disassembler formatting.

use ferrix_core::{
    Value,
    bytecode::{Chunk, Disassembler, Instruction, Register},
    diagnostics::{Diagnostic, SourceManager, SourceSpan},
};

fn assert_disassembly(chunk: &Chunk, expected: &str) {
    assert_eq!(Disassembler::disassemble_chunk(chunk), expected);
}

#[test]
fn disassembler_output_is_golden_tested_outside_unit_modules() {
    let mut chunk = Chunk::new("golden", 3);
    let lhs = chunk.add_constant(Value::Int(2)).unwrap();
    let rhs = chunk.add_constant(Value::Int(3)).unwrap();
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(0),
        constant: lhs,
    });
    chunk.push_instruction(Instruction::LoadConst {
        dst: Register(1),
        constant: rhs,
    });
    chunk.push_instruction(Instruction::Mul {
        dst: Register(2),
        lhs: Register(0),
        rhs: Register(1),
    });
    chunk.push_instruction(Instruction::Return { src: Register(2) });

    assert_disassembly(
        &chunk,
        "\
== golden ==
format: FERRIXBC v1
registers: 3
arity: 0
captures: 0
constants:
  #0 Int(2)
  #1 Int(3)

0000 LoadConst   r0, #0 ; Int(2)
0001 LoadConst   r1, #1 ; Int(3)
0002 Mul         r2, r0, r1
0003 Return      r2
",
    );
}

#[test]
fn diagnostic_output_is_golden_tested_outside_unit_modules() {
    let mut sources = SourceManager::new();
    let file = sources.add_file("golden.fx", "let x = 1;\nreturn y;\n");
    let diagnostic = Diagnostic::new(
        "undefined variable `y`",
        Some(SourceSpan::new(file, 18, 19)),
    );

    assert_eq!(
        sources.render_diagnostic(&diagnostic),
        "\
error: undefined variable `y`
 --> golden.fx:2:8
  |
2 | return y;
  |        ^
"
    );
}
