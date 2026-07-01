//! Ferrix bytecode interpreter.
//!
//! [`Vm`] executes verified chunks or full programs, manages call frames,
//! dispatches native functions, owns the heap, and exposes tracing/debug hooks
//! for tooling. Normal callers should prefer verified entry points such as
//! [`Vm::run_program`]; unchecked methods are kept for low-level tests.

use std::{collections::HashMap, rc::Rc};

use ferrix_core::{
    Obj, ObjRef, Value,
    bytecode::{
        Chunk, ConstId, FunctionId, FunctionKind, Instruction, JumpTarget, Program, Register,
        VerifiedChunk, VerifiedProgram, format_instruction,
    },
};

use crate::{
    DebugAction, DebugEvent, DebugOutcome, Debugger, GcStats, Heap, NativeContext, NullOutput,
    OutputWriter, RootSet, RuntimeLimits, TraceWriter,
};
use crate::{VmError, VmErrorKind, VmStackFrame};

/// Stateful bytecode interpreter and heap owner.
pub struct Vm {
    limits: RuntimeLimits,
    trace_enabled: bool,
    instruction_ip: usize,
    executed_instruction_count: usize,
    registers: Vec<Value>,
    frames: Vec<CallFrame>,
    program_roots: Vec<ObjRef>,
    heap: Heap,
    output: Box<dyn OutputWriter>,
    native_functions: HashMap<FunctionId, Rc<NativeFunction>>,
}

type NativeFunction =
    dyn for<'vm> Fn(&mut NativeContext<'vm>, &[Value]) -> Result<Value, VmError> + 'static;

/// Saved execution state for one active function call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallFrame {
    /// Function currently executing in this frame.
    pub function_id: FunctionId,
    /// Next instruction pointer for this frame.
    pub ip: usize,
    /// Register file owned by this frame.
    pub registers: Vec<Value>,
    /// Captured values available to this frame.
    pub captures: Vec<Value>,
    /// Caller register that should receive this frame's return value.
    pub return_dst: Option<Register>,
}

impl Vm {
    /// Creates a VM with default runtime limits.
    pub fn new() -> Self {
        Self::with_limits(RuntimeLimits::default())
    }

    /// Creates a VM with explicit runtime limits.
    pub fn with_limits(limits: RuntimeLimits) -> Self {
        Self {
            limits,
            trace_enabled: false,
            instruction_ip: 0,
            executed_instruction_count: 0,
            registers: Vec::new(),
            frames: Vec::new(),
            program_roots: Vec::new(),
            heap: Heap::new(),
            output: Box::new(NullOutput),
            native_functions: HashMap::new(),
        }
    }

    /// Returns the limits currently enforced by the VM.
    pub fn limits(&self) -> RuntimeLimits {
        self.limits
    }

    /// Returns whether simple instruction tracing is enabled.
    pub fn trace_enabled(&self) -> bool {
        self.trace_enabled
    }

    /// Enables or disables simple instruction tracing.
    pub fn set_trace_enabled(&mut self, enabled: bool) {
        self.trace_enabled = enabled;
    }

    /// Returns the last observed instruction pointer.
    pub fn instruction_ip(&self) -> usize {
        self.instruction_ip
    }

    /// Returns how many instructions have executed in the current/last run.
    pub fn executed_instruction_count(&self) -> usize {
        self.executed_instruction_count
    }

    /// Returns the active frame register snapshot.
    pub fn registers(&self) -> &[Value] {
        &self.registers
    }

    /// Returns the number of active call frames.
    pub fn call_depth(&self) -> usize {
        self.frames.len()
    }

    /// Returns the VM heap for read-only inspection.
    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    /// Allocates an object, triggering GC first when the heap limit is reached.
    pub fn allocate_object(&mut self, object: Obj) -> Result<ObjRef, VmError> {
        let extra_roots = object_references(&object);
        if self.heap.len() >= self.limits.max_heap_objects {
            let roots = self.allocation_roots(&extra_roots);
            self.heap.collect_garbage(&roots);
        }

        self.heap.allocate(object, self.limits)
    }

    /// Reads a heap object by reference.
    pub fn heap_object(&self, reference: ObjRef) -> Result<&Obj, VmError> {
        self.heap.get(reference)
    }

    /// Replaces the output sink used by native functions.
    pub fn set_output_writer(&mut self, output: impl OutputWriter + 'static) {
        self.output = Box::new(output);
    }

    /// Writes one line through the configured output sink.
    pub fn write_output_line(&mut self, line: &str) -> Result<(), VmError> {
        self.output.write_line(line)
    }

    /// Runs GC using current registers, frames, and remembered program constants.
    pub fn collect_garbage(&mut self) -> GcStats {
        let roots = self.allocation_roots(&[]);
        self.heap.collect_garbage(&roots)
    }

    /// Runs GC while treating all constants in the supplied program as roots.
    pub fn collect_garbage_with_program(&mut self, program: &Program) -> GcStats {
        let previous_program_roots =
            std::mem::replace(&mut self.program_roots, program_constant_roots(program));
        let stats = self.collect_garbage();
        self.program_roots = previous_program_roots;
        stats
    }

    /// Computes object roots from VM registers and active call frames.
    pub fn root_objects(&self) -> Vec<ObjRef> {
        let mut roots = RootSet::new();
        roots.insert_values(self.registers.iter().copied());
        for frame in &self.frames {
            roots.insert_values(frame.registers.iter().copied());
            roots.insert_values(frame.captures.iter().copied());
        }
        roots.into_vec()
    }

    /// Computes object roots from VM state plus all constants in a program.
    pub fn root_objects_with_program(&self, program: &Program) -> Vec<ObjRef> {
        let mut roots = RootSet::new();
        roots.insert_values(self.root_objects().into_iter().map(Value::Obj));
        roots.insert_program_constants(program);
        roots.into_vec()
    }

    /// Registers a native function that does not need direct VM context access.
    pub fn register_native_fn(
        &mut self,
        function: FunctionId,
        native: impl Fn(&[Value]) -> Result<Value, VmError> + 'static,
    ) {
        self.register_native_context_fn(function, move |_ctx, args| native(args));
    }

    /// Registers a native function that can allocate, inspect heap, or write output.
    pub fn register_native_context_fn(
        &mut self,
        function: FunctionId,
        native: impl for<'vm> Fn(&mut NativeContext<'vm>, &[Value]) -> Result<Value, VmError> + 'static,
    ) {
        self.native_functions.insert(function, Rc::new(native));
    }

    /// Executes a verified single chunk and returns its final value.
    pub fn run(&mut self, chunk: &VerifiedChunk) -> Result<Value, VmError> {
        self.run_unchecked(chunk.as_chunk())
    }

    /// Executes a verified program from its entry function.
    pub fn run_program(&mut self, program: &VerifiedProgram) -> Result<Value, VmError> {
        let program = program.as_program();
        let previous_program_roots =
            std::mem::replace(&mut self.program_roots, program_constant_roots(program));
        let result = self
            .run_program_inner(program, None, None)
            .map(completed_value)
            .map_err(|error| self.attach_stack_trace(error, program));
        self.program_roots = previous_program_roots;
        result
    }

    /// Executes a verified program and writes one trace line per instruction.
    pub fn run_program_with_trace(
        &mut self,
        program: &VerifiedProgram,
        trace: &mut impl TraceWriter,
    ) -> Result<Value, VmError> {
        let program = program.as_program();
        let previous_program_roots =
            std::mem::replace(&mut self.program_roots, program_constant_roots(program));
        let result = self
            .run_program_inner(program, Some(trace as &mut dyn TraceWriter), None)
            .map(completed_value)
            .map_err(|error| self.attach_stack_trace(error, program));
        self.program_roots = previous_program_roots;
        result
    }

    /// Executes a verified program under a debugger callback.
    pub fn run_program_with_debugger(
        &mut self,
        program: &VerifiedProgram,
        debugger: &mut impl Debugger,
    ) -> Result<DebugOutcome, VmError> {
        let program = program.as_program();
        let previous_program_roots =
            std::mem::replace(&mut self.program_roots, program_constant_roots(program));
        let result = self
            .run_program_inner(program, None, Some(debugger as &mut dyn Debugger))
            .map_err(|error| self.attach_stack_trace(error, program));
        self.program_roots = previous_program_roots;
        result
    }

    /// Executes a verified single chunk and writes trace lines.
    pub fn run_with_trace(
        &mut self,
        chunk: &VerifiedChunk,
        trace: &mut impl TraceWriter,
    ) -> Result<Value, VmError> {
        self.run_unchecked_with_trace(chunk.as_chunk(), trace)
    }

    /// Executes a raw chunk without structural verification.
    ///
    /// Prefer [`Vm::run`] for normal execution. This escape hatch is kept for
    /// negative VM tests and debugging paths that intentionally exercise
    /// runtime guards on malformed bytecode.
    pub fn run_unchecked(&mut self, chunk: &Chunk) -> Result<Value, VmError> {
        let previous_program_roots =
            std::mem::replace(&mut self.program_roots, chunk_constant_roots(chunk));
        let result = self.run_unchecked_inner(chunk, None);
        self.program_roots = previous_program_roots;
        result
    }

    /// Executes a raw chunk with tracing without structural verification.
    ///
    /// Prefer [`Vm::run_with_trace`] for normal execution.
    pub fn run_unchecked_with_trace(
        &mut self,
        chunk: &Chunk,
        trace: &mut impl TraceWriter,
    ) -> Result<Value, VmError> {
        let previous_program_roots =
            std::mem::replace(&mut self.program_roots, chunk_constant_roots(chunk));
        let result = self.run_unchecked_inner(chunk, Some(trace as &mut dyn TraceWriter));
        self.program_roots = previous_program_roots;
        result
    }

    fn run_unchecked_inner(
        &mut self,
        chunk: &Chunk,
        mut trace: Option<&mut dyn TraceWriter>,
    ) -> Result<Value, VmError> {
        self.reset_for_chunk(chunk);

        loop {
            let ip = self.instruction_ip;
            let Some(instruction) = chunk.instructions.get(ip) else {
                return Err(VmError::new(Some(ip), VmErrorKind::MissingReturn));
            };
            self.consume_instruction_budget(ip)?;

            self.instruction_ip = ip.checked_add(1).ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InstructionPointerOutOfBounds {
                        ip,
                        instruction_count: chunk.instructions.len(),
                    },
                )
            })?;

            if let Some(trace) = trace.as_deref_mut() {
                trace_instruction(trace, ip, instruction, &self.registers);
            }

            match instruction {
                Instruction::LoadConst { dst, constant } => {
                    let value = self.read_constant(chunk, ip, *constant)?;
                    self.write_register(ip, *dst, value)?;
                }
                Instruction::LoadString { dst, string } => {
                    let value = self.load_string(chunk, ip, *string)?;
                    self.write_register(ip, *dst, value)?;
                }
                Instruction::Move { dst, src } => {
                    let value = self.read_register(ip, *src)?;
                    self.write_register(ip, *dst, value)?;
                }
                Instruction::Add { dst, lhs, rhs } => {
                    self.write_binary_int(ip, *dst, *lhs, *rhs, "addition", i64::checked_add)?;
                }
                Instruction::Sub { dst, lhs, rhs } => {
                    self.write_binary_int(ip, *dst, *lhs, *rhs, "subtraction", i64::checked_sub)?;
                }
                Instruction::Mul { dst, lhs, rhs } => {
                    self.write_binary_int(
                        ip,
                        *dst,
                        *lhs,
                        *rhs,
                        "multiplication",
                        i64::checked_mul,
                    )?;
                }
                Instruction::Div { dst, lhs, rhs } => {
                    let rhs_value = self.read_int(ip, *rhs)?;
                    if rhs_value == 0 {
                        return Err(VmError::new(Some(ip), VmErrorKind::DivisionByZero));
                    }

                    let lhs_value = self.read_int(ip, *lhs)?;
                    let value = lhs_value.checked_div(rhs_value).ok_or_else(|| {
                        VmError::new(
                            Some(ip),
                            VmErrorKind::ArithmeticOverflow {
                                operation: "division",
                            },
                        )
                    })?;
                    self.write_register(ip, *dst, Value::Int(value))?;
                }
                Instruction::Jump { target } => {
                    self.jump_to(chunk, ip, *target)?;
                }
                Instruction::JumpIfFalse { condition, target } => {
                    if !self.read_bool(ip, *condition)? {
                        self.jump_to(chunk, ip, *target)?;
                    }
                }
                Instruction::JumpIfTrue { condition, target } => {
                    if self.read_bool(ip, *condition)? {
                        self.jump_to(chunk, ip, *target)?;
                    }
                }
                Instruction::Equal { dst, lhs, rhs } => {
                    let lhs = self.read_register(ip, *lhs)?;
                    let rhs = self.read_register(ip, *rhs)?;
                    self.write_register(ip, *dst, Value::Bool(lhs == rhs))?;
                }
                Instruction::NotEqual { dst, lhs, rhs } => {
                    let lhs = self.read_register(ip, *lhs)?;
                    let rhs = self.read_register(ip, *rhs)?;
                    self.write_register(ip, *dst, Value::Bool(lhs != rhs))?;
                }
                Instruction::Less { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, *dst, *lhs, *rhs, |lhs, rhs| lhs < rhs)?;
                }
                Instruction::LessEqual { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, *dst, *lhs, *rhs, |lhs, rhs| lhs <= rhs)?;
                }
                Instruction::Greater { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, *dst, *lhs, *rhs, |lhs, rhs| lhs > rhs)?;
                }
                Instruction::GreaterEqual { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, *dst, *lhs, *rhs, |lhs, rhs| lhs >= rhs)?;
                }
                Instruction::Not { dst, src } => {
                    let value = !self.read_bool(ip, *src)?;
                    self.write_register(ip, *dst, Value::Bool(value))?;
                }
                Instruction::CallFunction { function, .. } => {
                    return Err(VmError::new(
                        Some(ip),
                        VmErrorKind::InvalidFunction {
                            function: *function,
                            function_count: 0,
                        },
                    ));
                }
                Instruction::MakeUpvalue { dst, src } => {
                    let value = self.read_register(ip, *src)?;
                    let reference = self.allocate_object(Obj::Upvalue(value))?;
                    self.write_register(ip, *dst, Value::Obj(reference))?;
                }
                Instruction::LoadUpvalue { dst, upvalue } => {
                    let value = self.load_upvalue_from_register(ip, *upvalue)?;
                    self.write_register(ip, *dst, value)?;
                }
                Instruction::StoreUpvalue { upvalue, src } => {
                    let value = self.read_register(ip, *src)?;
                    self.store_upvalue_from_register(ip, *upvalue, value)?;
                }
                Instruction::MakeClosure { .. }
                | Instruction::LoadCapture { .. }
                | Instruction::LoadCaptureCell { .. }
                | Instruction::StoreCapture { .. }
                | Instruction::CallValue { .. } => {
                    return Err(VmError::new(
                        Some(ip),
                        VmErrorKind::InvalidFunction {
                            function: FunctionId(0),
                            function_count: 0,
                        },
                    ));
                }
                Instruction::ArrayNew {
                    dst,
                    elements_start,
                    element_count,
                } => {
                    let elements = self.read_arguments(ip, *elements_start, *element_count)?;
                    let reference = self.allocate_object(Obj::Array(elements))?;
                    self.write_register(ip, *dst, Value::Obj(reference))?;
                }
                Instruction::MapNew {
                    dst,
                    entries_start,
                    entry_count,
                } => {
                    let entries = self.read_map_entries(ip, *entries_start, *entry_count)?;
                    let reference = self.allocate_object(Obj::Map(entries))?;
                    self.write_register(ip, *dst, Value::Obj(reference))?;
                }
                Instruction::IndexGet { dst, target, index } => {
                    let value = self.index_get(ip, *target, *index)?;
                    self.write_register(ip, *dst, value)?;
                }
                Instruction::IndexSet {
                    target,
                    index,
                    value,
                } => {
                    self.index_set(ip, *target, *index, *value)?;
                }
                Instruction::ArrayGet { dst, array, index } => {
                    let value = self.array_get(ip, *array, *index)?;
                    self.write_register(ip, *dst, value)?;
                }
                Instruction::ArraySet {
                    array,
                    index,
                    value,
                } => {
                    self.array_set(ip, *array, *index, *value)?;
                }
                Instruction::Return { src } => {
                    return self.read_register(ip, *src);
                }
            }
        }
    }

    fn reset_for_chunk(&mut self, chunk: &Chunk) {
        self.instruction_ip = 0;
        self.executed_instruction_count = 0;
        self.registers = vec![Value::Nil; usize::from(chunk.register_count)];
        self.frames.clear();
    }

    fn run_program_inner(
        &mut self,
        program: &Program,
        mut trace: Option<&mut dyn TraceWriter>,
        mut debugger: Option<&mut dyn Debugger>,
    ) -> Result<DebugOutcome, VmError> {
        self.frames.clear();
        self.registers.clear();
        self.instruction_ip = 0;
        self.executed_instruction_count = 0;
        self.push_frame(program, program.entry, None, &[], &[], None)?;

        loop {
            let frame_index = self
                .frames
                .len()
                .checked_sub(1)
                .ok_or_else(|| VmError::new(None, VmErrorKind::MissingReturn))?;
            let function_id = self.frames[frame_index].function_id;
            let function = program.function(function_id).ok_or_else(|| {
                VmError::new(
                    None,
                    VmErrorKind::InvalidFunction {
                        function: function_id,
                        function_count: program.functions.len(),
                    },
                )
            })?;
            let chunk = match &function.kind {
                FunctionKind::Bytecode(chunk) => chunk,
                FunctionKind::Native { .. } => {
                    return Err(VmError::new(
                        None,
                        VmErrorKind::MissingNativeFunction {
                            function: function_id,
                        },
                    ));
                }
            };

            let ip = self.frames[frame_index].ip;
            let Some(instruction) = chunk.instructions.get(ip).cloned() else {
                return Err(VmError::new(Some(ip), VmErrorKind::MissingReturn));
            };
            self.consume_instruction_budget(ip)?;

            self.instruction_ip = ip.checked_add(1).ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InstructionPointerOutOfBounds {
                        ip,
                        instruction_count: chunk.instructions.len(),
                    },
                )
            })?;
            self.registers = self.frames[frame_index].registers.clone();

            if let Some(debugger) = debugger.as_deref_mut() {
                let action = debugger.before_instruction(DebugEvent {
                    program,
                    function: function_id,
                    function_name: &function.name,
                    instruction_ip: ip,
                    instruction: &instruction,
                    registers: &self.registers,
                    frames: &self.frames,
                    heap: &self.heap,
                    source_span: chunk.source_map.get(ip).copied().flatten(),
                });
                if action == DebugAction::Quit {
                    return Ok(DebugOutcome::Quit);
                }
            }

            if let Some(trace) = trace.as_deref_mut() {
                trace_instruction(trace, ip, &instruction, &self.registers);
            }

            match instruction {
                Instruction::LoadConst { dst, constant } => {
                    let value = self.read_constant(chunk, ip, constant)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::LoadString { dst, string } => {
                    let value = self.load_string(chunk, ip, string)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::Move { dst, src } => {
                    let value = self.read_register(ip, src)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::Add { dst, lhs, rhs } => {
                    self.write_binary_int(ip, dst, lhs, rhs, "addition", i64::checked_add)?;
                }
                Instruction::Sub { dst, lhs, rhs } => {
                    self.write_binary_int(ip, dst, lhs, rhs, "subtraction", i64::checked_sub)?;
                }
                Instruction::Mul { dst, lhs, rhs } => {
                    self.write_binary_int(ip, dst, lhs, rhs, "multiplication", i64::checked_mul)?;
                }
                Instruction::Div { dst, lhs, rhs } => {
                    let rhs_value = self.read_int(ip, rhs)?;
                    if rhs_value == 0 {
                        return Err(VmError::new(Some(ip), VmErrorKind::DivisionByZero));
                    }
                    let lhs_value = self.read_int(ip, lhs)?;
                    let value = lhs_value.checked_div(rhs_value).ok_or_else(|| {
                        VmError::new(
                            Some(ip),
                            VmErrorKind::ArithmeticOverflow {
                                operation: "division",
                            },
                        )
                    })?;
                    self.write_register(ip, dst, Value::Int(value))?;
                }
                Instruction::Jump { target } => {
                    self.jump_to(chunk, ip, target)?;
                }
                Instruction::JumpIfFalse { condition, target } => {
                    if !self.read_bool(ip, condition)? {
                        self.jump_to(chunk, ip, target)?;
                    }
                }
                Instruction::JumpIfTrue { condition, target } => {
                    if self.read_bool(ip, condition)? {
                        self.jump_to(chunk, ip, target)?;
                    }
                }
                Instruction::Equal { dst, lhs, rhs } => {
                    let lhs = self.read_register(ip, lhs)?;
                    let rhs = self.read_register(ip, rhs)?;
                    self.write_register(ip, dst, Value::Bool(lhs == rhs))?;
                }
                Instruction::NotEqual { dst, lhs, rhs } => {
                    let lhs = self.read_register(ip, lhs)?;
                    let rhs = self.read_register(ip, rhs)?;
                    self.write_register(ip, dst, Value::Bool(lhs != rhs))?;
                }
                Instruction::Less { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, dst, lhs, rhs, |lhs, rhs| lhs < rhs)?;
                }
                Instruction::LessEqual { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, dst, lhs, rhs, |lhs, rhs| lhs <= rhs)?;
                }
                Instruction::Greater { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, dst, lhs, rhs, |lhs, rhs| lhs > rhs)?;
                }
                Instruction::GreaterEqual { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, dst, lhs, rhs, |lhs, rhs| lhs >= rhs)?;
                }
                Instruction::Not { dst, src } => {
                    let value = !self.read_bool(ip, src)?;
                    self.write_register(ip, dst, Value::Bool(value))?;
                }
                Instruction::CallFunction {
                    dst,
                    function,
                    args_start,
                    arg_count,
                } => {
                    let args = self.read_arguments(ip, args_start, arg_count)?;
                    self.frames[frame_index].registers = self.registers.clone();
                    self.frames[frame_index].ip = self.instruction_ip;
                    if self.call_function(program, ip, function, dst, &args)? {
                        continue;
                    }
                }
                Instruction::MakeUpvalue { dst, src } => {
                    let value = self.read_register(ip, src)?;
                    let reference = self.allocate_object(Obj::Upvalue(value))?;
                    self.write_register(ip, dst, Value::Obj(reference))?;
                }
                Instruction::LoadUpvalue { dst, upvalue } => {
                    let value = self.load_upvalue_from_register(ip, upvalue)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::StoreUpvalue { upvalue, src } => {
                    let value = self.read_register(ip, src)?;
                    self.store_upvalue_from_register(ip, upvalue, value)?;
                }
                Instruction::MakeClosure {
                    dst,
                    function,
                    captures_start,
                    capture_count,
                } => {
                    let captures = self.read_arguments(ip, captures_start, capture_count)?;
                    let reference = self.allocate_object(Obj::Closure { function, captures })?;
                    self.write_register(ip, dst, Value::Obj(reference))?;
                }
                Instruction::LoadCapture { dst, capture } => {
                    let value = self.load_capture(ip, frame_index, capture)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::LoadCaptureCell { dst, capture } => {
                    let value = self.capture_cell(ip, frame_index, capture)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::StoreCapture { capture, src } => {
                    let value = self.read_register(ip, src)?;
                    self.store_capture(ip, frame_index, capture, value)?;
                }
                Instruction::CallValue {
                    dst,
                    callee,
                    args_start,
                    arg_count,
                } => {
                    let callee = self.read_register(ip, callee)?;
                    let args = self.read_arguments(ip, args_start, arg_count)?;
                    self.frames[frame_index].registers = self.registers.clone();
                    self.frames[frame_index].ip = self.instruction_ip;
                    if self.call_value(program, ip, callee, dst, &args)? {
                        continue;
                    }
                }
                Instruction::ArrayNew {
                    dst,
                    elements_start,
                    element_count,
                } => {
                    let elements = self.read_arguments(ip, elements_start, element_count)?;
                    let reference = self.allocate_object(Obj::Array(elements))?;
                    self.write_register(ip, dst, Value::Obj(reference))?;
                }
                Instruction::MapNew {
                    dst,
                    entries_start,
                    entry_count,
                } => {
                    let entries = self.read_map_entries(ip, entries_start, entry_count)?;
                    let reference = self.allocate_object(Obj::Map(entries))?;
                    self.write_register(ip, dst, Value::Obj(reference))?;
                }
                Instruction::IndexGet { dst, target, index } => {
                    let value = self.index_get(ip, target, index)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::IndexSet {
                    target,
                    index,
                    value,
                } => {
                    self.index_set(ip, target, index, value)?;
                }
                Instruction::ArrayGet { dst, array, index } => {
                    let value = self.array_get(ip, array, index)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::ArraySet {
                    array,
                    index,
                    value,
                } => {
                    self.array_set(ip, array, index, value)?;
                }
                Instruction::Return { src } => {
                    let value = self.read_register(ip, src)?;
                    let returned_frame = self.frames.pop().expect("current frame exists");
                    if let Some(caller) = self.frames.last_mut() {
                        let return_dst = returned_frame
                            .return_dst
                            .ok_or_else(|| VmError::new(Some(ip), VmErrorKind::MissingReturn))?;
                        write_register_in(Some(ip), &mut caller.registers, return_dst, value)?;
                        self.registers = caller.registers.clone();
                        self.instruction_ip = caller.ip;
                        continue;
                    }

                    self.registers = returned_frame.registers;
                    self.instruction_ip = ip;
                    return Ok(DebugOutcome::Completed(value));
                }
            }

            if let Some(frame) = self.frames.get_mut(frame_index) {
                frame.registers = self.registers.clone();
                frame.ip = self.instruction_ip;
            }
        }
    }

    fn call_function(
        &mut self,
        program: &Program,
        ip: usize,
        function_id: FunctionId,
        return_dst: Register,
        args: &[Value],
    ) -> Result<bool, VmError> {
        let function = program.function(function_id).ok_or_else(|| {
            VmError::new(
                Some(ip),
                VmErrorKind::InvalidFunction {
                    function: function_id,
                    function_count: program.functions.len(),
                },
            )
        })?;

        match &function.kind {
            FunctionKind::Bytecode(_) => {
                self.push_frame(program, function_id, Some(return_dst), args, &[], Some(ip))?;
                Ok(true)
            }
            FunctionKind::Native { .. } => {
                let native = self
                    .native_functions
                    .get(&function_id)
                    .cloned()
                    .ok_or_else(|| {
                        VmError::new(
                            Some(ip),
                            VmErrorKind::MissingNativeFunction {
                                function: function_id,
                            },
                        )
                    })?;
                let value = {
                    let mut context = NativeContext::new(self);
                    native(&mut context, args).map_err(|mut err| {
                        if err.instruction_ip.is_none() {
                            err.instruction_ip = Some(ip);
                        }
                        err
                    })?
                };
                self.write_register(ip, return_dst, value)?;
                Ok(false)
            }
        }
    }

    fn call_value(
        &mut self,
        program: &Program,
        ip: usize,
        callee: Value,
        return_dst: Register,
        args: &[Value],
    ) -> Result<bool, VmError> {
        let Value::Obj(reference) = callee else {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "function",
                    found: callee,
                },
            ));
        };
        let Obj::Closure { function, captures } = self.heap_object(reference)?.clone() else {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "function",
                    found: callee,
                },
            ));
        };
        let expected = program.function(function).ok_or_else(|| {
            VmError::new(
                Some(ip),
                VmErrorKind::InvalidFunction {
                    function,
                    function_count: program.functions.len(),
                },
            )
        })?;
        if expected.arity != args.len() as u8 {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "matching function arity",
                    found: callee,
                },
            ));
        }
        self.push_frame(
            program,
            function,
            Some(return_dst),
            args,
            &captures,
            Some(ip),
        )?;
        Ok(true)
    }

    fn capture_cell(
        &self,
        ip: usize,
        frame_index: usize,
        capture: ferrix_core::bytecode::CaptureId,
    ) -> Result<Value, VmError> {
        self.frames[frame_index]
            .captures
            .get(usize::from(capture.0))
            .copied()
            .ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InvalidCapture {
                        capture,
                        capture_count: self.frames[frame_index].captures.len(),
                    },
                )
            })
    }

    fn load_capture(
        &self,
        ip: usize,
        frame_index: usize,
        capture: ferrix_core::bytecode::CaptureId,
    ) -> Result<Value, VmError> {
        let cell = self.capture_cell(ip, frame_index, capture)?;
        self.load_upvalue_value(ip, cell)
    }

    fn store_capture(
        &mut self,
        ip: usize,
        frame_index: usize,
        capture: ferrix_core::bytecode::CaptureId,
        value: Value,
    ) -> Result<(), VmError> {
        let cell = self.capture_cell(ip, frame_index, capture)?;
        self.store_upvalue_value(ip, cell, value)
    }

    fn load_upvalue_from_register(&self, ip: usize, upvalue: Register) -> Result<Value, VmError> {
        let cell = self.read_register(ip, upvalue)?;
        self.load_upvalue_value(ip, cell)
    }

    fn store_upvalue_from_register(
        &mut self,
        ip: usize,
        upvalue: Register,
        value: Value,
    ) -> Result<(), VmError> {
        let cell = self.read_register(ip, upvalue)?;
        self.store_upvalue_value(ip, cell, value)
    }

    fn load_upvalue_value(&self, ip: usize, cell: Value) -> Result<Value, VmError> {
        let reference = self.upvalue_ref(ip, cell)?;
        let Obj::Upvalue(value) = self.heap.get(reference)? else {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "upvalue",
                    found: cell,
                },
            ));
        };
        Ok(*value)
    }

    fn store_upvalue_value(&mut self, ip: usize, cell: Value, value: Value) -> Result<(), VmError> {
        let reference = self.upvalue_ref(ip, cell)?;
        let Obj::Upvalue(stored) = self.heap.get_mut(reference)? else {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "upvalue",
                    found: cell,
                },
            ));
        };
        *stored = value;
        Ok(())
    }

    fn upvalue_ref(&self, ip: usize, cell: Value) -> Result<ObjRef, VmError> {
        let Value::Obj(reference) = cell else {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "upvalue",
                    found: cell,
                },
            ));
        };
        Ok(reference)
    }

    fn push_frame(
        &mut self,
        program: &Program,
        function_id: FunctionId,
        return_dst: Option<Register>,
        args: &[Value],
        captures: &[Value],
        ip: Option<usize>,
    ) -> Result<(), VmError> {
        if self.frames.len() >= self.limits.max_call_depth {
            return Err(VmError::new(
                ip,
                VmErrorKind::CallDepthExceeded {
                    max_call_depth: self.limits.max_call_depth,
                },
            ));
        }

        let function = program.function(function_id).ok_or_else(|| {
            VmError::new(
                ip,
                VmErrorKind::InvalidFunction {
                    function: function_id,
                    function_count: program.functions.len(),
                },
            )
        })?;
        let mut registers = vec![Value::Nil; usize::from(function.register_count)];
        for (index, value) in args.iter().enumerate() {
            registers[index] = *value;
        }

        self.frames.push(CallFrame {
            function_id,
            ip: 0,
            registers,
            captures: captures.to_vec(),
            return_dst,
        });
        Ok(())
    }

    fn read_constant(&self, chunk: &Chunk, ip: usize, constant: ConstId) -> Result<Value, VmError> {
        chunk
            .constants
            .get(usize::from(constant.0))
            .cloned()
            .ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InvalidConstant {
                        constant,
                        constant_count: chunk.constants.len(),
                    },
                )
            })
    }

    fn load_string(
        &mut self,
        chunk: &Chunk,
        ip: usize,
        string: ferrix_core::bytecode::StringId,
    ) -> Result<Value, VmError> {
        let value = chunk
            .strings
            .get(usize::from(string.0))
            .cloned()
            .ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InvalidString {
                        string,
                        string_count: chunk.strings.len(),
                    },
                )
            })?;
        let reference = self.allocate_object(Obj::String(value))?;
        Ok(Value::Obj(reference))
    }

    fn index_get(
        &self,
        ip: usize,
        target_register: Register,
        index_register: Register,
    ) -> Result<Value, VmError> {
        let target = self.read_indexable_ref(ip, target_register)?;
        match self.heap.get(target)? {
            Obj::Array(values) => {
                let raw_index = self.read_int(ip, index_register)?;
                let index = array_index(raw_index, values.len(), ip)?;
                Ok(values[index])
            }
            Obj::Map(entries) => {
                let key = self.read_register(ip, index_register)?;
                let Some(index) = self.map_entry_index(entries, key)? else {
                    return Ok(Value::Nil);
                };
                Ok(entries[index].1)
            }
            _ => {
                let found = self.read_register(ip, target_register)?;
                Err(VmError::new(
                    Some(ip),
                    VmErrorKind::TypeError {
                        expected: "array or map",
                        found,
                    },
                ))
            }
        }
    }

    fn index_set(
        &mut self,
        ip: usize,
        target_register: Register,
        index_register: Register,
        value_register: Register,
    ) -> Result<(), VmError> {
        let target = self.read_indexable_ref(ip, target_register)?;
        let value = self.read_register(ip, value_register)?;

        match self.heap.get(target)? {
            Obj::Array(values) => {
                let raw_index = self.read_int(ip, index_register)?;
                let index = array_index(raw_index, values.len(), ip)?;
                let Obj::Array(values) = self.heap.get_mut(target)? else {
                    unreachable!("object kind checked before mutable access");
                };
                values[index] = value;
                Ok(())
            }
            Obj::Map(entries) => {
                let key = self.read_register(ip, index_register)?;
                let entry_index = self.map_entry_index(entries, key)?;
                let Obj::Map(entries) = self.heap.get_mut(target)? else {
                    unreachable!("object kind checked before mutable access");
                };
                if let Some(entry_index) = entry_index {
                    entries[entry_index].1 = value;
                } else {
                    entries.push((key, value));
                }
                Ok(())
            }
            _ => {
                let found = self.read_register(ip, target_register)?;
                Err(VmError::new(
                    Some(ip),
                    VmErrorKind::TypeError {
                        expected: "array or map",
                        found,
                    },
                ))
            }
        }
    }

    fn array_get(
        &self,
        ip: usize,
        array_register: Register,
        index_register: Register,
    ) -> Result<Value, VmError> {
        let array = self.read_array_ref(ip, array_register)?;
        let raw_index = self.read_int(ip, index_register)?;
        let Obj::Array(values) = self.heap.get(array)? else {
            let found = self.read_register(ip, array_register)?;
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "array",
                    found,
                },
            ));
        };
        let index = array_index(raw_index, values.len(), ip)?;

        Ok(values[index])
    }

    fn array_set(
        &mut self,
        ip: usize,
        array_register: Register,
        index_register: Register,
        value_register: Register,
    ) -> Result<(), VmError> {
        let array = self.read_array_ref(ip, array_register)?;
        let raw_index = self.read_int(ip, index_register)?;
        let value = self.read_register(ip, value_register)?;
        let found = self.read_register(ip, array_register)?;

        let Obj::Array(values) = self.heap.get_mut(array)? else {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "array",
                    found,
                },
            ));
        };
        let index = array_index(raw_index, values.len(), ip)?;

        values[index] = value;
        Ok(())
    }

    fn consume_instruction_budget(&mut self, ip: usize) -> Result<(), VmError> {
        if self.executed_instruction_count >= self.limits.max_instruction_count {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::InstructionLimitExceeded {
                    max_instruction_count: self.limits.max_instruction_count,
                },
            ));
        }

        self.executed_instruction_count += 1;
        Ok(())
    }

    fn read_register(&self, ip: usize, register: Register) -> Result<Value, VmError> {
        self.registers
            .get(usize::from(register.0))
            .cloned()
            .ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InvalidRegister {
                        register,
                        register_count: self.register_count(),
                    },
                )
            })
    }

    fn write_register(
        &mut self,
        ip: usize,
        register: Register,
        value: Value,
    ) -> Result<(), VmError> {
        write_register_in(Some(ip), &mut self.registers, register, value)
    }

    fn read_int(&self, ip: usize, register: Register) -> Result<i64, VmError> {
        match self.read_register(ip, register)? {
            Value::Int(value) => Ok(value),
            found => Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "int",
                    found,
                },
            )),
        }
    }

    fn read_bool(&self, ip: usize, register: Register) -> Result<bool, VmError> {
        match self.read_register(ip, register)? {
            Value::Bool(value) => Ok(value),
            found => Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "bool",
                    found,
                },
            )),
        }
    }

    fn read_array_ref(&self, ip: usize, register: Register) -> Result<ObjRef, VmError> {
        match self.read_register(ip, register)? {
            Value::Obj(reference) => Ok(reference),
            found => Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "array",
                    found,
                },
            )),
        }
    }

    fn read_indexable_ref(&self, ip: usize, register: Register) -> Result<ObjRef, VmError> {
        match self.read_register(ip, register)? {
            Value::Obj(reference) => Ok(reference),
            found => Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "array or map",
                    found,
                },
            )),
        }
    }

    fn read_map_entries(
        &self,
        ip: usize,
        entries_start: Register,
        entry_count: u8,
    ) -> Result<Vec<(Value, Value)>, VmError> {
        let mut entries = Vec::with_capacity(usize::from(entry_count));
        for offset in 0..entry_count {
            let key_register = entries_start
                .0
                .checked_add(offset.saturating_mul(2))
                .ok_or_else(|| {
                    VmError::new(
                        Some(ip),
                        VmErrorKind::InvalidRegister {
                            register: Register(u8::MAX),
                            register_count: self.register_count(),
                        },
                    )
                })?;
            let value_register = key_register.checked_add(1).ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InvalidRegister {
                        register: Register(u8::MAX),
                        register_count: self.register_count(),
                    },
                )
            })?;
            let key = self.read_register(ip, Register(key_register))?;
            let value = self.read_register(ip, Register(value_register))?;
            if let Some(index) = self.map_entry_index(&entries, key)? {
                entries[index].1 = value;
            } else {
                entries.push((key, value));
            }
        }
        Ok(entries)
    }

    fn map_entry_index(
        &self,
        entries: &[(Value, Value)],
        key: Value,
    ) -> Result<Option<usize>, VmError> {
        for (index, (entry_key, _)) in entries.iter().enumerate() {
            if self.map_keys_equal(*entry_key, key)? {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    fn map_keys_equal(&self, lhs: Value, rhs: Value) -> Result<bool, VmError> {
        if lhs == rhs {
            return Ok(true);
        }

        let (Value::Obj(lhs), Value::Obj(rhs)) = (lhs, rhs) else {
            return Ok(false);
        };
        match (self.heap.get(lhs)?, self.heap.get(rhs)?) {
            (Obj::String(lhs), Obj::String(rhs)) => Ok(lhs == rhs),
            _ => Ok(false),
        }
    }

    fn write_binary_int(
        &mut self,
        ip: usize,
        dst: Register,
        lhs: Register,
        rhs: Register,
        operation: &'static str,
        op: fn(i64, i64) -> Option<i64>,
    ) -> Result<(), VmError> {
        let lhs = self.read_int(ip, lhs)?;
        let rhs = self.read_int(ip, rhs)?;
        let value = op(lhs, rhs)
            .ok_or_else(|| VmError::new(Some(ip), VmErrorKind::ArithmeticOverflow { operation }))?;

        self.write_register(ip, dst, Value::Int(value))
    }

    fn write_int_comparison(
        &mut self,
        ip: usize,
        dst: Register,
        lhs: Register,
        rhs: Register,
        op: fn(i64, i64) -> bool,
    ) -> Result<(), VmError> {
        let lhs = self.read_int(ip, lhs)?;
        let rhs = self.read_int(ip, rhs)?;
        self.write_register(ip, dst, Value::Bool(op(lhs, rhs)))
    }

    fn jump_to(&mut self, chunk: &Chunk, ip: usize, target: JumpTarget) -> Result<(), VmError> {
        let target_ip = usize::try_from(target.0).map_err(|_| {
            VmError::new(
                Some(ip),
                VmErrorKind::InvalidJumpTarget {
                    target,
                    instruction_count: chunk.instructions.len(),
                },
            )
        })?;

        if target_ip >= chunk.instructions.len() {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::InvalidJumpTarget {
                    target,
                    instruction_count: chunk.instructions.len(),
                },
            ));
        }

        self.instruction_ip = target_ip;
        Ok(())
    }

    fn register_count(&self) -> u8 {
        self.registers.len().try_into().unwrap_or(u8::MAX)
    }

    fn allocation_roots(&self, extra_roots: &[ObjRef]) -> Vec<ObjRef> {
        let mut roots = RootSet::new();
        roots.insert_values(self.root_objects().into_iter().map(Value::Obj));
        roots.insert_values(self.program_roots.iter().copied().map(Value::Obj));
        roots.insert_values(extra_roots.iter().copied().map(Value::Obj));
        roots.into_vec()
    }

    fn attach_stack_trace(&self, error: VmError, program: &Program) -> VmError {
        let stack_trace = self.stack_trace_for_error(error.instruction_ip, program);
        error.with_stack_trace(stack_trace)
    }

    fn stack_trace_for_error(
        &self,
        error_ip: Option<usize>,
        program: &Program,
    ) -> Vec<VmStackFrame> {
        let Some(current_frame_index) = self.frames.len().checked_sub(1) else {
            return Vec::new();
        };

        self.frames
            .iter()
            .enumerate()
            .rev()
            .map(|(index, frame)| {
                let instruction_ip = if index == current_frame_index {
                    error_ip.or(Some(frame.ip))
                } else {
                    frame.ip.checked_sub(1)
                };
                let name = program
                    .function(frame.function_id)
                    .map(|function| function.name.clone())
                    .unwrap_or_else(|| frame.function_id.to_string());

                VmStackFrame {
                    function: frame.function_id,
                    name,
                    instruction_ip,
                }
            })
            .collect()
    }

    fn read_arguments(
        &self,
        ip: usize,
        args_start: Register,
        arg_count: u8,
    ) -> Result<Vec<Value>, VmError> {
        let mut args = Vec::with_capacity(usize::from(arg_count));
        for offset in 0..arg_count {
            let register = args_start.0.checked_add(offset).ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InvalidRegister {
                        register: Register(u8::MAX),
                        register_count: self.register_count(),
                    },
                )
            })?;
            args.push(self.read_register(ip, Register(register))?);
        }
        Ok(args)
    }
}

fn write_register_in(
    ip: Option<usize>,
    registers: &mut [Value],
    register: Register,
    value: Value,
) -> Result<(), VmError> {
    let register_count = registers.len().try_into().unwrap_or(u8::MAX);
    let slot = registers.get_mut(usize::from(register.0)).ok_or_else(|| {
        VmError::new(
            ip,
            VmErrorKind::InvalidRegister {
                register,
                register_count,
            },
        )
    })?;
    *slot = value;
    Ok(())
}

fn completed_value(outcome: DebugOutcome) -> Value {
    match outcome {
        DebugOutcome::Completed(value) => value,
        DebugOutcome::Quit => unreachable!("only debugger execution can quit early"),
    }
}

fn array_index(index: i64, len: usize, ip: usize) -> Result<usize, VmError> {
    let index = usize::try_from(index)
        .map_err(|_| VmError::new(Some(ip), VmErrorKind::IndexOutOfBounds { index, len }))?;

    if index >= len {
        return Err(VmError::new(
            Some(ip),
            VmErrorKind::IndexOutOfBounds {
                index: index as i64,
                len,
            },
        ));
    }

    Ok(index)
}

fn program_constant_roots(program: &Program) -> Vec<ObjRef> {
    let mut roots = RootSet::new();
    roots.insert_program_constants(program);
    roots.into_vec()
}

fn chunk_constant_roots(chunk: &Chunk) -> Vec<ObjRef> {
    let mut roots = RootSet::new();
    roots.insert_chunk_constants(chunk);
    roots.into_vec()
}

fn object_references(object: &Obj) -> Vec<ObjRef> {
    match object {
        Obj::Array(values) => values.iter().filter_map(Value::as_obj_ref).collect(),
        Obj::Map(entries) => entries
            .iter()
            .flat_map(|(key, value)| [key.as_obj_ref(), value.as_obj_ref()])
            .flatten()
            .collect(),
        Obj::Upvalue(value) => value.as_obj_ref().into_iter().collect(),
        Obj::Closure { captures, .. } => captures.iter().filter_map(Value::as_obj_ref).collect(),
        Obj::String(_) | Obj::Function(_) | Obj::NativeFunction(_) | Obj::Module(_) => Vec::new(),
    }
}

fn trace_instruction(
    trace: &mut (impl TraceWriter + ?Sized),
    ip: usize,
    instruction: &Instruction,
    registers: &[Value],
) {
    trace.write_trace_line(&format!("ip={ip:04} {}", format_instruction(instruction)));
    trace.write_trace_line(&format!("registers: {}", format_registers(registers)));
}

fn format_registers(registers: &[Value]) -> String {
    registers
        .iter()
        .enumerate()
        .map(|(index, value)| format!("r{index}={value:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}
