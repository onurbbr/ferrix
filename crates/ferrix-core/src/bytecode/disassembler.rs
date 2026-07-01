//! Human-readable bytecode disassembler.
//!
//! Used by tests, debugging tools, and golden outputs to inspect emitted
//! bytecode without executing it.

use std::fmt::Write;

use crate::{
    Value,
    bytecode::{
        BYTECODE_MAGIC, CURRENT_BYTECODE_VERSION, Chunk, ConstId, FunctionId, FunctionKind,
        Instruction, JumpTarget, Program, Register, StringId,
    },
};

/// Stateless disassembler for chunks and whole programs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disassembler;

impl Disassembler {
    /// Formats one chunk into deterministic text.
    pub fn disassemble_chunk(chunk: &Chunk) -> String {
        let mut output = String::new();

        writeln!(&mut output, "== {} ==", chunk.name).expect("writing to String cannot fail");
        writeln!(
            &mut output,
            "format: {} v{}",
            BYTECODE_MAGIC, CURRENT_BYTECODE_VERSION
        )
        .expect("writing to String cannot fail");
        writeln!(&mut output, "registers: {}", chunk.register_count)
            .expect("writing to String cannot fail");
        writeln!(&mut output, "arity: {}", chunk.arity).expect("writing to String cannot fail");
        write_debug_locals(&mut output, chunk);
        writeln!(&mut output, "constants:").expect("writing to String cannot fail");

        if chunk.constants.is_empty() {
            writeln!(&mut output, "  <empty>").expect("writing to String cannot fail");
        } else {
            for (index, value) in chunk.constants.iter().enumerate() {
                writeln!(&mut output, "  #{} {}", index, format_value(value))
                    .expect("writing to String cannot fail");
            }
        }

        if !chunk.strings.is_empty() {
            writeln!(&mut output, "strings:").expect("writing to String cannot fail");
            for (index, value) in chunk.strings.iter().enumerate() {
                writeln!(&mut output, "  str#{} {}", index, format_string(value))
                    .expect("writing to String cannot fail");
            }
        }

        writeln!(&mut output).expect("writing to String cannot fail");

        for (ip, instruction) in chunk.instructions.iter().enumerate() {
            write!(&mut output, "{ip:04} {}", format_instruction(instruction))
                .expect("writing to String cannot fail");

            if let Instruction::LoadConst { constant, .. } = instruction
                && let Some(value) = chunk.constants.get(usize::from(constant.0))
            {
                write!(&mut output, " ; {}", format_value(value))
                    .expect("writing to String cannot fail");
            }

            if let Instruction::LoadString { string, .. } = instruction
                && let Some(value) = chunk.strings.get(usize::from(string.0))
            {
                write!(&mut output, " ; {}", format_string(value))
                    .expect("writing to String cannot fail");
            }

            writeln!(&mut output).expect("writing to String cannot fail");
        }

        output
    }

    /// Formats a whole program and its function table.
    pub fn disassemble_program(program: &Program) -> String {
        let mut output = String::new();

        writeln!(&mut output, "== program ==").expect("writing to String cannot fail");
        writeln!(
            &mut output,
            "format: {} v{} flags={}",
            program.format.magic, program.format.version, program.format.feature_flags
        )
        .expect("writing to String cannot fail");
        writeln!(&mut output, "entry: {}", program.entry).expect("writing to String cannot fail");
        writeln!(&mut output, "functions: {}", program.functions.len())
            .expect("writing to String cannot fail");

        for (index, function) in program.functions.iter().enumerate() {
            writeln!(&mut output).expect("writing to String cannot fail");
            writeln!(
                &mut output,
                "-- fn#{} {} arity={} registers={} --",
                index, function.name, function.arity, function.register_count
            )
            .expect("writing to String cannot fail");
            match &function.kind {
                FunctionKind::Bytecode(chunk) => {
                    output.push_str(&Self::disassemble_chunk(chunk));
                }
                FunctionKind::Native { name } => {
                    writeln!(&mut output, "native: {name}").expect("writing to String cannot fail");
                }
            }
        }

        output
    }
}

fn write_debug_locals(output: &mut String, chunk: &Chunk) {
    if chunk.debug_local_names.is_empty() {
        return;
    }

    writeln!(output, "locals:").expect("writing to String cannot fail");
    for (index, name) in chunk.debug_local_names.iter().enumerate() {
        if let Some(name) = name {
            writeln!(output, "  r{index} {name}").expect("writing to String cannot fail");
        }
    }
}

/// Formats one instruction without requiring chunk/program context.
pub fn format_instruction(instruction: &Instruction) -> String {
    match instruction {
        Instruction::LoadConst { dst, constant } => {
            format!(
                "{:<11} {}, {}",
                "LoadConst",
                register(*dst),
                constant_id(*constant)
            )
        }
        Instruction::LoadString { dst, string } => {
            format!(
                "{:<11} {}, {}",
                "LoadString",
                register(*dst),
                string_id(*string)
            )
        }
        Instruction::Move { dst, src } => {
            format!("{:<11} {}, {}", "Move", register(*dst), register(*src))
        }
        Instruction::Add { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "Add",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::Sub { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "Sub",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::Mul { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "Mul",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::Div { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "Div",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::Jump { target } => {
            format!("{:<11} {}", "Jump", jump_target(*target))
        }
        Instruction::JumpIfFalse { condition, target } => {
            format!(
                "{:<11} {}, {}",
                "JumpIfFalse",
                register(*condition),
                jump_target(*target)
            )
        }
        Instruction::JumpIfTrue { condition, target } => {
            format!(
                "{:<11} {}, {}",
                "JumpIfTrue",
                register(*condition),
                jump_target(*target)
            )
        }
        Instruction::Equal { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "Equal",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::NotEqual { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "NotEqual",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::Less { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "Less",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::LessEqual { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "LessEqual",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::Greater { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "Greater",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::GreaterEqual { dst, lhs, rhs } => {
            format!(
                "{:<11} {}, {}, {}",
                "GreaterEqual",
                register(*dst),
                register(*lhs),
                register(*rhs)
            )
        }
        Instruction::Not { dst, src } => {
            format!("{:<11} {}, {}", "Not", register(*dst), register(*src))
        }
        Instruction::CallFunction {
            dst,
            function,
            args_start,
            arg_count,
        } => {
            format!(
                "{:<11} {}, {}, {}, {}",
                "CallFunction",
                register(*dst),
                function_id(*function),
                register(*args_start),
                arg_count
            )
        }
        Instruction::ArrayNew {
            dst,
            elements_start,
            element_count,
        } => {
            format!(
                "{:<11} {}, {}, {}",
                "ArrayNew",
                register(*dst),
                register(*elements_start),
                element_count
            )
        }
        Instruction::MapNew {
            dst,
            entries_start,
            entry_count,
        } => {
            format!(
                "{:<11} {}, {}, {}",
                "MapNew",
                register(*dst),
                register(*entries_start),
                entry_count
            )
        }
        Instruction::IndexGet { dst, target, index } => {
            format!(
                "{:<11} {}, {}, {}",
                "IndexGet",
                register(*dst),
                register(*target),
                register(*index)
            )
        }
        Instruction::IndexSet {
            target,
            index,
            value,
        } => {
            format!(
                "{:<11} {}, {}, {}",
                "IndexSet",
                register(*target),
                register(*index),
                register(*value)
            )
        }
        Instruction::ArrayGet { dst, array, index } => {
            format!(
                "{:<11} {}, {}, {}",
                "ArrayGet",
                register(*dst),
                register(*array),
                register(*index)
            )
        }
        Instruction::ArraySet {
            array,
            index,
            value,
        } => {
            format!(
                "{:<11} {}, {}, {}",
                "ArraySet",
                register(*array),
                register(*index),
                register(*value)
            )
        }
        Instruction::Return { src } => {
            format!("{:<11} {}", "Return", register(*src))
        }
    }
}

fn format_value(value: &Value) -> String {
    match value {
        Value::Int(value) => format!("Int({value})"),
        Value::Float(value) => format!("Float({value:?})"),
        Value::Bool(value) => format!("Bool({value})"),
        Value::Obj(reference) => format!("Obj({reference:?})"),
        Value::Nil => "Nil".to_string(),
    }
}

fn format_string(value: &str) -> String {
    format!("{value:?}")
}

fn register(register: Register) -> String {
    register.to_string()
}

fn constant_id(constant: ConstId) -> String {
    constant.to_string()
}

fn string_id(string: StringId) -> String {
    string.to_string()
}

fn function_id(function: FunctionId) -> String {
    function.to_string()
}

fn jump_target(target: JumpTarget) -> String {
    target.to_string()
}
