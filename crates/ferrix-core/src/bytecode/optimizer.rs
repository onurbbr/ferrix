//! Bytecode optimization pass.
//!
//! The optimizer works on verified-shaped chunks without changing the register
//! file or function metadata. It performs conservative local rewrites that keep
//! the output acceptable to the structural and program verifiers.

use std::{collections::HashSet, time::Instant};

use crate::{
    Value,
    bytecode::{Chunk, Instruction, JumpTarget, Register},
};

/// Structured metadata emitted by one optimizer pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizationPassReport {
    /// Stable pass name suitable for CLI output.
    pub name: &'static str,
    /// Short description of the transformation.
    pub description: &'static str,
    /// Whether the pass changed the instruction stream.
    pub changed: bool,
    /// Pass wall-clock duration in nanoseconds.
    pub duration_ns: u128,
    /// Human-readable pass warnings.
    pub warnings: Vec<String>,
    /// Number of instructions inspected by the pass.
    pub instructions_inspected: usize,
    /// Number of bytecode functions inspected by the pass.
    pub functions_inspected: usize,
    /// Number of local rewrites/removals performed by the pass.
    pub transformations_applied: usize,
}

/// Optimization metadata for one bytecode chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizationReport {
    /// Chunk/function name optimized.
    pub chunk_name: String,
    /// Instruction count before running optimizer passes.
    pub instructions_before: usize,
    /// Instruction count after running optimizer passes.
    pub instructions_after: usize,
    /// Reports emitted by individual passes.
    pub passes: Vec<OptimizationPassReport>,
}

impl OptimizationReport {
    /// Returns the total number of transformations applied by all passes.
    pub fn total_transformations(&self) -> usize {
        self.passes
            .iter()
            .map(|pass| pass.transformations_applied)
            .sum()
    }
}

/// Optimized chunk paired with pass metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizedChunk {
    /// Optimized bytecode chunk.
    pub chunk: Chunk,
    /// Optimization report for this chunk.
    pub report: OptimizationReport,
}

/// Optimizes a single bytecode chunk while preserving verifier invariants.
pub fn optimize_chunk(chunk: Chunk) -> Chunk {
    optimize_chunk_with_report(chunk).chunk
}

/// Optimizes a single bytecode chunk and returns pass-level metadata.
pub fn optimize_chunk_with_report(mut chunk: Chunk) -> OptimizedChunk {
    let mut report = OptimizationReport {
        chunk_name: chunk.name.clone(),
        instructions_before: chunk.instructions.len(),
        instructions_after: chunk.instructions.len(),
        passes: Vec::new(),
    };

    run_pass(
        &mut report,
        "constant-folding",
        "Fold register-local constant expressions into constant loads.",
        &mut chunk,
        fold_constant_instructions,
    );
    run_pass(
        &mut report,
        "integer-specialization",
        "Specialize generic arithmetic/comparison opcodes to integer opcodes.",
        &mut chunk,
        specialize_integer_instructions,
    );
    run_pass(
        &mut report,
        "jump-chain-collapse",
        "Collapse jump targets that point at unconditional jumps.",
        &mut chunk,
        collapse_jump_chains,
    );
    run_pass(
        &mut report,
        "redundant-move-removal",
        "Remove self-moves that are not jump targets.",
        &mut chunk,
        remove_redundant_moves,
    );
    run_pass(
        &mut report,
        "unreachable-code-removal",
        "Remove straight-line instructions after terminal control flow.",
        &mut chunk,
        remove_unreachable_instructions,
    );
    run_pass(
        &mut report,
        "post-compact-jump-chain-collapse",
        "Collapse jump chains after instruction compaction.",
        &mut chunk,
        collapse_jump_chains,
    );

    report.instructions_after = chunk.instructions.len();
    OptimizedChunk { chunk, report }
}

fn run_pass(
    report: &mut OptimizationReport,
    name: &'static str,
    description: &'static str,
    chunk: &mut Chunk,
    pass: fn(&mut Chunk) -> usize,
) {
    let instructions_before = chunk.instructions.len();
    let started = Instant::now();
    let transformations = pass(chunk);
    let duration_ns = started.elapsed().as_nanos();
    let instructions_after = chunk.instructions.len();
    report.passes.push(OptimizationPassReport {
        name,
        description,
        changed: transformations > 0 || instructions_before != instructions_after,
        duration_ns,
        warnings: Vec::new(),
        instructions_inspected: instructions_before,
        functions_inspected: 1,
        transformations_applied: transformations,
    });
}

fn specialize_integer_instructions(chunk: &mut Chunk) -> usize {
    let mut transformations = 0;
    for instruction in &mut chunk.instructions {
        let replacement = match instruction {
            Instruction::Add { dst, lhs, rhs } => Some(Instruction::AddInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            }),
            Instruction::Sub { dst, lhs, rhs } => Some(Instruction::SubInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            }),
            Instruction::Mul { dst, lhs, rhs } => Some(Instruction::MulInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            }),
            Instruction::Div { dst, lhs, rhs } => Some(Instruction::DivInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            }),
            Instruction::Less { dst, lhs, rhs } => Some(Instruction::LessInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            }),
            Instruction::LessEqual { dst, lhs, rhs } => Some(Instruction::LessEqualInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            }),
            Instruction::Greater { dst, lhs, rhs } => Some(Instruction::GreaterInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            }),
            Instruction::GreaterEqual { dst, lhs, rhs } => Some(Instruction::GreaterEqualInt {
                dst: *dst,
                lhs: *lhs,
                rhs: *rhs,
            }),
            _ => None,
        };

        if let Some(replacement) = replacement {
            *instruction = replacement;
            transformations += 1;
        }
    }
    transformations
}

fn fold_constant_instructions(chunk: &mut Chunk) -> usize {
    let jump_targets = jump_targets(&chunk.instructions);
    let mut constants_by_register = vec![None; usize::from(chunk.register_count)];
    let mut transformations = 0;

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
            | Instruction::AddInt { dst, lhs, rhs }
            | Instruction::Sub { dst, lhs, rhs }
            | Instruction::SubInt { dst, lhs, rhs }
            | Instruction::Mul { dst, lhs, rhs }
            | Instruction::MulInt { dst, lhs, rhs }
            | Instruction::Div { dst, lhs, rhs }
            | Instruction::DivInt { dst, lhs, rhs }
            | Instruction::Equal { dst, lhs, rhs }
            | Instruction::NotEqual { dst, lhs, rhs }
            | Instruction::Less { dst, lhs, rhs }
            | Instruction::LessInt { dst, lhs, rhs }
            | Instruction::LessEqual { dst, lhs, rhs }
            | Instruction::LessEqualInt { dst, lhs, rhs }
            | Instruction::Greater { dst, lhs, rhs }
            | Instruction::GreaterInt { dst, lhs, rhs }
            | Instruction::GreaterEqual { dst, lhs, rhs }
            | Instruction::GreaterEqualInt { dst, lhs, rhs } => {
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
                        | Instruction::RecordNew { .. }
                        | Instruction::FieldGet { .. }
                        | Instruction::FieldSet { .. }
                        | Instruction::PushHandler { .. }
                        | Instruction::Throw { .. }
                        | Instruction::Return { .. }
                ) {
                    constants_by_register.fill(None);
                }
                None
            }
        };

        if let Some(instruction) = replacement {
            chunk.instructions[ip] = instruction;
            transformations += 1;
        }
    }
    transformations
}

fn fold_binary(instruction: &Instruction, lhs: Option<Value>, rhs: Option<Value>) -> Option<Value> {
    match (instruction, lhs?, rhs?) {
        (
            Instruction::Add { .. } | Instruction::AddInt { .. },
            Value::Int(lhs),
            Value::Int(rhs),
        ) => lhs.checked_add(rhs).map(Value::Int),
        (
            Instruction::Sub { .. } | Instruction::SubInt { .. },
            Value::Int(lhs),
            Value::Int(rhs),
        ) => lhs.checked_sub(rhs).map(Value::Int),
        (
            Instruction::Mul { .. } | Instruction::MulInt { .. },
            Value::Int(lhs),
            Value::Int(rhs),
        ) => lhs.checked_mul(rhs).map(Value::Int),
        (Instruction::Div { .. } | Instruction::DivInt { .. }, Value::Int(_), Value::Int(0)) => {
            None
        }
        (
            Instruction::Div { .. } | Instruction::DivInt { .. },
            Value::Int(lhs),
            Value::Int(rhs),
        ) => lhs.checked_div(rhs).map(Value::Int),
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
        (
            Instruction::Less { .. } | Instruction::LessInt { .. },
            Value::Int(lhs),
            Value::Int(rhs),
        ) => Some(Value::Bool(lhs < rhs)),
        (
            Instruction::LessEqual { .. } | Instruction::LessEqualInt { .. },
            Value::Int(lhs),
            Value::Int(rhs),
        ) => Some(Value::Bool(lhs <= rhs)),
        (
            Instruction::Greater { .. } | Instruction::GreaterInt { .. },
            Value::Int(lhs),
            Value::Int(rhs),
        ) => Some(Value::Bool(lhs > rhs)),
        (
            Instruction::GreaterEqual { .. } | Instruction::GreaterEqualInt { .. },
            Value::Int(lhs),
            Value::Int(rhs),
        ) => Some(Value::Bool(lhs >= rhs)),
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
        | Instruction::AddInt { dst, .. }
        | Instruction::Sub { dst, .. }
        | Instruction::SubInt { dst, .. }
        | Instruction::Mul { dst, .. }
        | Instruction::MulInt { dst, .. }
        | Instruction::Div { dst, .. }
        | Instruction::DivInt { dst, .. }
        | Instruction::Equal { dst, .. }
        | Instruction::NotEqual { dst, .. }
        | Instruction::Less { dst, .. }
        | Instruction::LessInt { dst, .. }
        | Instruction::LessEqual { dst, .. }
        | Instruction::LessEqualInt { dst, .. }
        | Instruction::Greater { dst, .. }
        | Instruction::GreaterInt { dst, .. }
        | Instruction::GreaterEqual { dst, .. }
        | Instruction::GreaterEqualInt { dst, .. }
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
        | Instruction::RecordNew { dst, .. }
        | Instruction::IndexGet { dst, .. }
        | Instruction::ArrayGet { dst, .. }
        | Instruction::FieldGet { dst, .. }
        | Instruction::PushHandler { error: dst, .. } => vec![*dst],
        Instruction::Jump { .. }
        | Instruction::JumpIfFalse { .. }
        | Instruction::JumpIfTrue { .. }
        | Instruction::PopHandler
        | Instruction::Throw { .. }
        | Instruction::StoreUpvalue { .. }
        | Instruction::StoreCapture { .. }
        | Instruction::IndexSet { .. }
        | Instruction::ArraySet { .. }
        | Instruction::FieldSet { .. }
        | Instruction::Return { .. } => Vec::new(),
    }
}

fn collapse_jump_chains(chunk: &mut Chunk) -> usize {
    let instructions = chunk.instructions.clone();
    let mut transformations = 0;
    for instruction in &mut chunk.instructions {
        let Some(target) = instruction.jump_operand() else {
            continue;
        };
        let collapsed = follow_jump_chain(&instructions, target);
        match instruction {
            Instruction::Jump { target }
            | Instruction::JumpIfFalse { target, .. }
            | Instruction::JumpIfTrue { target, .. }
            | Instruction::PushHandler { target, .. } => {
                update_jump_target(target, collapsed, &mut transformations);
            }
            _ => {}
        }
    }
    transformations
}

fn update_jump_target(target: &mut JumpTarget, collapsed: JumpTarget, transformations: &mut usize) {
    if *target != collapsed {
        *target = collapsed;
        *transformations += 1;
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

fn remove_redundant_moves(chunk: &mut Chunk) -> usize {
    let targets = jump_targets(&chunk.instructions);
    let keep = chunk
        .instructions
        .iter()
        .enumerate()
        .map(|(index, instruction)| {
            !matches!(instruction, Instruction::Move { dst, src } if dst == src && !targets.contains(&index))
        })
        .collect::<Vec<_>>();
    let removed = keep.iter().filter(|keep| !**keep).count();
    compact_chunk(chunk, &keep);
    removed
}

fn remove_unreachable_instructions(chunk: &mut Chunk) -> usize {
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
                Instruction::Jump { .. } | Instruction::Throw { .. } | Instruction::Return { .. }
            )
        {
            reachable = false;
        }
    }

    let removed = keep.iter().filter(|keep| !**keep).count();
    compact_chunk(chunk, &keep);
    removed
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
        | Instruction::JumpIfTrue { target, .. }
        | Instruction::PushHandler { target, .. } => *target = new_target,
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
