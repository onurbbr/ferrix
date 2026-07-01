//! Bytecode optimization pass.
//!
//! The optimizer works on verified-shaped chunks without changing the register
//! file or function metadata. It performs conservative local rewrites that keep
//! the output acceptable to the structural and program verifiers.

use std::collections::HashSet;

use crate::{
    Value,
    bytecode::{Chunk, Instruction, JumpTarget, Register},
};

/// Optimizes a single bytecode chunk while preserving verifier invariants.
pub fn optimize_chunk(mut chunk: Chunk) -> Chunk {
    fold_constant_instructions(&mut chunk);
    collapse_jump_chains(&mut chunk);
    remove_redundant_moves(&mut chunk);
    remove_unreachable_instructions(&mut chunk);
    collapse_jump_chains(&mut chunk);
    chunk
}

fn fold_constant_instructions(chunk: &mut Chunk) {
    let jump_targets = jump_targets(&chunk.instructions);
    let mut constants_by_register = vec![None; usize::from(chunk.register_count)];

    for ip in 0..chunk.instructions.len() {
        if jump_targets.contains(&ip) {
            constants_by_register.fill(None);
        }

        let replacement = match &chunk.instructions[ip] {
            Instruction::LoadConst { dst, constant } => {
                set_register_constant(
                    &mut constants_by_register,
                    *dst,
                    chunk.constants.get(usize::from(constant.0)).copied(),
                );
                None
            }
            Instruction::Move { dst, src } => {
                let value = get_register_constant(&constants_by_register, *src);
                set_register_constant(&mut constants_by_register, *dst, value);
                None
            }
            Instruction::Add { dst, lhs, rhs }
            | Instruction::Sub { dst, lhs, rhs }
            | Instruction::Mul { dst, lhs, rhs }
            | Instruction::Div { dst, lhs, rhs }
            | Instruction::Equal { dst, lhs, rhs }
            | Instruction::NotEqual { dst, lhs, rhs }
            | Instruction::Less { dst, lhs, rhs }
            | Instruction::LessEqual { dst, lhs, rhs }
            | Instruction::Greater { dst, lhs, rhs }
            | Instruction::GreaterEqual { dst, lhs, rhs } => {
                let lhs_value = get_register_constant(&constants_by_register, *lhs);
                let rhs_value = get_register_constant(&constants_by_register, *rhs);
                let folded = fold_binary(&chunk.instructions[ip], lhs_value, rhs_value);
                replace_with_constant_load(chunk, &mut constants_by_register, *dst, folded)
            }
            Instruction::Not { dst, src } => {
                let folded = match get_register_constant(&constants_by_register, *src) {
                    Some(Value::Bool(value)) => Some(Value::Bool(!value)),
                    _ => None,
                };
                replace_with_constant_load(chunk, &mut constants_by_register, *dst, folded)
            }
            instruction => {
                forget_written_registers(&mut constants_by_register, instruction);
                if matches!(
                    instruction,
                    Instruction::Jump { .. }
                        | Instruction::JumpIfFalse { .. }
                        | Instruction::JumpIfTrue { .. }
                        | Instruction::Return { .. }
                ) {
                    constants_by_register.fill(None);
                }
                None
            }
        };

        if let Some(instruction) = replacement {
            chunk.instructions[ip] = instruction;
        }
    }
}

fn fold_binary(instruction: &Instruction, lhs: Option<Value>, rhs: Option<Value>) -> Option<Value> {
    match (instruction, lhs?, rhs?) {
        (Instruction::Add { .. }, Value::Int(lhs), Value::Int(rhs)) => {
            lhs.checked_add(rhs).map(Value::Int)
        }
        (Instruction::Sub { .. }, Value::Int(lhs), Value::Int(rhs)) => {
            lhs.checked_sub(rhs).map(Value::Int)
        }
        (Instruction::Mul { .. }, Value::Int(lhs), Value::Int(rhs)) => {
            lhs.checked_mul(rhs).map(Value::Int)
        }
        (Instruction::Div { .. }, Value::Int(_), Value::Int(0)) => None,
        (Instruction::Div { .. }, Value::Int(lhs), Value::Int(rhs)) => {
            lhs.checked_div(rhs).map(Value::Int)
        }
        (Instruction::Equal { .. }, lhs, rhs)
            if is_foldable_value(lhs) && is_foldable_value(rhs) =>
        {
            Some(Value::Bool(lhs == rhs))
        }
        (Instruction::NotEqual { .. }, lhs, rhs)
            if is_foldable_value(lhs) && is_foldable_value(rhs) =>
        {
            Some(Value::Bool(lhs != rhs))
        }
        (Instruction::Less { .. }, Value::Int(lhs), Value::Int(rhs)) => {
            Some(Value::Bool(lhs < rhs))
        }
        (Instruction::LessEqual { .. }, Value::Int(lhs), Value::Int(rhs)) => {
            Some(Value::Bool(lhs <= rhs))
        }
        (Instruction::Greater { .. }, Value::Int(lhs), Value::Int(rhs)) => {
            Some(Value::Bool(lhs > rhs))
        }
        (Instruction::GreaterEqual { .. }, Value::Int(lhs), Value::Int(rhs)) => {
            Some(Value::Bool(lhs >= rhs))
        }
        _ => None,
    }
}

fn replace_with_constant_load(
    chunk: &mut Chunk,
    constants_by_register: &mut [Option<Value>],
    dst: Register,
    folded: Option<Value>,
) -> Option<Instruction> {
    let Some(value) = folded else {
        set_register_constant(constants_by_register, dst, None);
        return None;
    };

    let Ok(constant) = chunk.add_constant(value) else {
        set_register_constant(constants_by_register, dst, None);
        return None;
    };

    set_register_constant(constants_by_register, dst, Some(value));
    Some(Instruction::LoadConst { dst, constant })
}

fn is_foldable_value(value: Value) -> bool {
    matches!(
        value,
        Value::Nil | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    )
}

fn set_register_constant(
    constants_by_register: &mut [Option<Value>],
    register: Register,
    value: Option<Value>,
) {
    if let Some(slot) = constants_by_register.get_mut(usize::from(register.0)) {
        *slot = value;
    }
}

fn get_register_constant(
    constants_by_register: &[Option<Value>],
    register: Register,
) -> Option<Value> {
    constants_by_register
        .get(usize::from(register.0))
        .copied()
        .flatten()
}

fn forget_written_registers(
    constants_by_register: &mut [Option<Value>],
    instruction: &Instruction,
) {
    for register in written_registers(instruction) {
        set_register_constant(constants_by_register, register, None);
    }
}

fn written_registers(instruction: &Instruction) -> Vec<Register> {
    match instruction {
        Instruction::LoadConst { dst, .. }
        | Instruction::LoadString { dst, .. }
        | Instruction::Move { dst, .. }
        | Instruction::Add { dst, .. }
        | Instruction::Sub { dst, .. }
        | Instruction::Mul { dst, .. }
        | Instruction::Div { dst, .. }
        | Instruction::Equal { dst, .. }
        | Instruction::NotEqual { dst, .. }
        | Instruction::Less { dst, .. }
        | Instruction::LessEqual { dst, .. }
        | Instruction::Greater { dst, .. }
        | Instruction::GreaterEqual { dst, .. }
        | Instruction::Not { dst, .. }
        | Instruction::CallFunction { dst, .. }
        | Instruction::MakeUpvalue { dst, .. }
        | Instruction::LoadUpvalue { dst, .. }
        | Instruction::MakeClosure { dst, .. }
        | Instruction::LoadCapture { dst, .. }
        | Instruction::LoadCaptureCell { dst, .. }
        | Instruction::CallValue { dst, .. }
        | Instruction::ArrayNew { dst, .. }
        | Instruction::MapNew { dst, .. }
        | Instruction::IndexGet { dst, .. }
        | Instruction::ArrayGet { dst, .. } => vec![*dst],
        Instruction::Jump { .. }
        | Instruction::JumpIfFalse { .. }
        | Instruction::JumpIfTrue { .. }
        | Instruction::StoreUpvalue { .. }
        | Instruction::StoreCapture { .. }
        | Instruction::IndexSet { .. }
        | Instruction::ArraySet { .. }
        | Instruction::Return { .. } => Vec::new(),
    }
}

fn collapse_jump_chains(chunk: &mut Chunk) {
    let instructions = chunk.instructions.clone();
    for instruction in &mut chunk.instructions {
        let Some(target) = instruction.jump_operand() else {
            continue;
        };
        let collapsed = follow_jump_chain(&instructions, target);
        match instruction {
            Instruction::Jump { target }
            | Instruction::JumpIfFalse { target, .. }
            | Instruction::JumpIfTrue { target, .. } => *target = collapsed,
            _ => {}
        }
    }
}

fn follow_jump_chain(instructions: &[Instruction], mut target: JumpTarget) -> JumpTarget {
    let mut seen = HashSet::new();
    while seen.insert(target.0) {
        let Some(Instruction::Jump { target: next }) = usize::try_from(target.0)
            .ok()
            .and_then(|index| instructions.get(index))
        else {
            break;
        };
        target = *next;
    }
    target
}

fn remove_redundant_moves(chunk: &mut Chunk) {
    let targets = jump_targets(&chunk.instructions);
    let keep = chunk
        .instructions
        .iter()
        .enumerate()
        .map(|(index, instruction)| {
            !matches!(instruction, Instruction::Move { dst, src } if dst == src && !targets.contains(&index))
        })
        .collect::<Vec<_>>();
    compact_chunk(chunk, &keep);
}

fn remove_unreachable_instructions(chunk: &mut Chunk) {
    let targets = jump_targets(&chunk.instructions);
    let mut keep = Vec::with_capacity(chunk.instructions.len());
    let mut reachable = true;

    for (index, instruction) in chunk.instructions.iter().enumerate() {
        if targets.contains(&index) {
            reachable = true;
        }

        keep.push(reachable);

        if reachable
            && matches!(
                instruction,
                Instruction::Jump { .. } | Instruction::Return { .. }
            )
        {
            reachable = false;
        }
    }

    compact_chunk(chunk, &keep);
}

fn compact_chunk(chunk: &mut Chunk, keep: &[bool]) {
    if keep.iter().all(|keep| *keep) {
        return;
    }

    let mapping = instruction_mapping(keep);
    let mut instructions = Vec::with_capacity(keep.iter().filter(|keep| **keep).count());
    let mut source_map = Vec::with_capacity(instructions.capacity());

    for (index, keep_instruction) in keep.iter().copied().enumerate() {
        if !keep_instruction {
            continue;
        }

        let mut instruction = chunk.instructions[index].clone();
        remap_jump_target(&mut instruction, &mapping);
        instructions.push(instruction);
        source_map.push(chunk.source_map.get(index).copied().flatten());
    }

    chunk.instructions = instructions;
    chunk.source_map = source_map;
}

fn instruction_mapping(keep: &[bool]) -> Vec<Option<usize>> {
    let mut next_kept = vec![None; keep.len() + 1];
    let mut next = None;

    for index in (0..keep.len()).rev() {
        if keep[index] {
            next = Some(keep[..=index].iter().filter(|keep| **keep).count() - 1);
        }
        next_kept[index] = next;
    }

    next_kept
}

fn remap_jump_target(instruction: &mut Instruction, mapping: &[Option<usize>]) {
    let Some(target) = instruction.jump_operand() else {
        return;
    };
    let Some(new_target) = usize::try_from(target.0)
        .ok()
        .and_then(|target| mapping.get(target))
        .and_then(|target| *target)
    else {
        return;
    };

    let new_target = JumpTarget(new_target as u32);
    match instruction {
        Instruction::Jump { target }
        | Instruction::JumpIfFalse { target, .. }
        | Instruction::JumpIfTrue { target, .. } => *target = new_target,
        _ => {}
    }
}

fn jump_targets(instructions: &[Instruction]) -> HashSet<usize> {
    instructions
        .iter()
        .filter_map(|instruction| instruction.jump_operand())
        .filter_map(|target| usize::try_from(target.0).ok())
        .collect()
}
