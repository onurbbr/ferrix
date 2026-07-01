//! Tests for converting runtime errors into source diagnostics.

use ferrix_core::{
    Value,
    bytecode::{Chunk, Instruction, Register},
    diagnostics::{FileId, SourceManager, SourceSpan},
};
use ferrix_vm::{Vm, VmErrorKind};

#[test]
fn runtime_error_converts_to_source_diagnostic_when_chunk_has_span() {
    let mut chunk = Chunk::new("main", 3);
    let file = FileId(0);
    let one = chunk.add_constant(Value::Int(1)).unwrap();
    let zero = chunk.add_constant(Value::Int(0)).unwrap();
    chunk.push_instruction_with_span(
        Instruction::LoadConst {
            dst: Register(0),
            constant: one,
        },
        Some(SourceSpan::new(file, 0, 1)),
    );
    chunk.push_instruction_with_span(
        Instruction::LoadConst {
            dst: Register(1),
            constant: zero,
        },
        Some(SourceSpan::new(file, 4, 5)),
    );
    chunk.push_instruction_with_span(
        Instruction::Div {
            dst: Register(2),
            lhs: Register(0),
            rhs: Register(1),
        },
        Some(SourceSpan::new(file, 0, 5)),
    );
    chunk.push_instruction(Instruction::Return { src: Register(2) });

    let err = Vm::new().run_unchecked(&chunk).unwrap_err();
    let mut sources = SourceManager::new();
    sources.add_file("main.fx", "1 / 0;\n");

    assert_eq!(err.kind, VmErrorKind::DivisionByZero);
    assert_eq!(
        sources.render_diagnostic(&err.to_diagnostic_with_chunk(&chunk)),
        "\
error: division by zero at instruction 2
 --> main.fx:1:1
  |
1 | 1 / 0;
  | ^^^^^
"
    );
}
