//! Ferrix bytecode interpreter.
//!
//! [`Vm`] executes verified chunks or full programs, manages call frames,
//! dispatches native functions, owns the heap, and exposes tracing/debug hooks
//! for tooling. Normal callers should prefer verified entry points such as
//! [`Vm::run_program`]; unchecked methods are kept for low-level tests.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use ferrix_core::{
    Obj, ObjRef, Value,
    bytecode::{
        Chunk, ConstId, FunctionId, FunctionKind, Instruction, JumpTarget, Program, Register,
        StringId, VerifiedChunk, VerifiedProgram, format_instruction,
    },
};

use crate::{
    DebugAction, DebugEvent, DebugOutcome, Debugger, GcStats, Heap, HostCapability,
    IncrementalGcPhase, NativeContext, NullOutput, OutputWriter, RootSet, RuntimeLimits,
    TraceWriter,
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
    exception_handlers: Vec<ExceptionHandler>,
    program_roots: Vec<ObjRef>,
    heap: Heap,
    gc_stats: VmGcStats,
    execution_stats: VmExecutionStats,
    output: Box<dyn OutputWriter>,
    capabilities: HashSet<HostCapability>,
    audit_events: Vec<String>,
    native_functions: HashMap<FunctionId, Rc<NativeFunction>>,
    extension_functions: HashMap<String, Rc<ExtensionFunction>>,
    field_cache: HashMap<FieldCacheKey, usize>,
}

type NativeFunction =
    dyn for<'vm> Fn(&mut NativeContext<'vm>, &[Value]) -> Result<Value, VmError> + 'static;
type ExtensionFunction =
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExceptionHandler {
    frame_depth: usize,
    error_register: Register,
    target_ip: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FieldCacheKey {
    object: ObjRef,
    field: String,
}

/// Cumulative GC and allocation counters for one VM instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VmGcStats {
    /// Successful heap allocations performed by this VM.
    pub allocations: u64,
    /// Successful allocations since the last GC pass.
    pub allocation_pressure: usize,
    /// Number of GC passes run manually or automatically.
    pub collections: u64,
    /// Number of bounded incremental GC steps run at VM safepoints.
    pub incremental_steps: u64,
    /// Total reachable objects marked across all collections.
    pub total_marked: usize,
    /// Total unreachable objects swept across all collections.
    pub total_swept: usize,
    /// Live object count reported by the latest collection.
    pub live_after_last_collection: usize,
    /// Per-pass stats from the latest collection.
    pub last_collection: GcStats,
}

/// Cumulative execution counters for one VM instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VmExecutionStats {
    /// Host native calls executed successfully or attempted by this VM.
    pub native_calls: u64,
    /// Maximum number of active call frames observed during execution.
    pub max_call_depth: usize,
    /// Maximum register file size observed during execution.
    pub max_register_count: usize,
    /// Number of throw instructions executed.
    pub thrown_errors: u64,
    /// Number of throw instructions caught by an exception handler.
    pub handled_exceptions: u64,
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
            exception_handlers: Vec::new(),
            program_roots: Vec::new(),
            heap: Heap::new(),
            gc_stats: VmGcStats::default(),
            execution_stats: VmExecutionStats::default(),
            output: Box::new(NullOutput),
            capabilities: HashSet::new(),
            audit_events: Vec::new(),
            native_functions: HashMap::new(),
            extension_functions: HashMap::new(),
            field_cache: HashMap::new(),
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

    /// Returns cumulative allocation and GC counters.
    pub fn gc_stats(&self) -> VmGcStats {
        self.gc_stats
    }

    /// Returns cumulative non-GC execution counters.
    pub fn execution_stats(&self) -> VmExecutionStats {
        self.execution_stats
    }

    /// Returns the current incremental GC phase.
    pub fn incremental_gc_phase(&self) -> IncrementalGcPhase {
        self.heap.incremental_phase()
    }

    /// Allocates an object, triggering GC first when pressure or heap limits ask.
    pub fn allocate_object(&mut self, object: Obj) -> Result<ObjRef, VmError> {
        let extra_roots = object_references(&object);
        if self.should_collect_before_allocation() {
            self.start_incremental_garbage_with_extra_roots(&extra_roots);
            self.step_incremental_garbage();
        }
        if self.heap.len() >= self.limits.max_heap_objects {
            self.start_incremental_garbage_with_extra_roots(&extra_roots);
            self.finish_incremental_garbage();
        }

        let reference = self.heap.allocate(object, self.limits)?;
        self.record_allocation();
        Ok(reference)
    }

    /// Reads a heap object by reference.
    pub fn heap_object(&self, reference: ObjRef) -> Result<&Obj, VmError> {
        self.heap.get(reference)
    }

    /// Replaces the output sink used by native functions.
    pub fn set_output_writer(&mut self, output: impl OutputWriter + 'static) {
        self.output = Box::new(output);
    }

    /// Replaces the host capabilities granted to this VM.
    pub fn set_capabilities(&mut self, capabilities: impl IntoIterator<Item = HostCapability>) {
        self.capabilities = capabilities.into_iter().collect();
    }

    /// Grants one host capability to this VM.
    pub fn grant_capability(&mut self, capability: HostCapability) {
        self.capabilities.insert(capability);
    }

    /// Returns true when this VM has a host capability.
    pub fn has_capability(&self, capability: HostCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// Requires a capability and emits an audit event when denied.
    pub fn require_capability(
        &mut self,
        capability: HostCapability,
        operation: &'static str,
    ) -> Result<(), VmError> {
        if self.has_capability(capability) {
            return Ok(());
        }
        self.audit_events.push(format!(
            "capability_denied capability={} operation={operation}",
            capability.as_str()
        ));
        Err(VmError::new(
            None,
            VmErrorKind::CapabilityDenied {
                capability,
                operation,
            },
        ))
    }

    /// Returns audit events captured during VM execution.
    pub fn audit_events(&self) -> &[String] {
        &self.audit_events
    }

    /// Writes one line through the configured output sink.
    pub fn write_output_line(&mut self, line: &str) -> Result<(), VmError> {
        self.output.write_line(line)
    }

    /// Runs GC using current registers, frames, and remembered program constants.
    pub fn collect_garbage(&mut self) -> GcStats {
        self.collect_garbage_with_extra_roots(&[])
    }

    /// Runs GC while treating all constants in the supplied program as roots.
    pub fn collect_garbage_with_program(&mut self, program: &Program) -> GcStats {
        let previous_program_roots =
            std::mem::replace(&mut self.program_roots, program_constant_roots(program));
        let stats = self.collect_garbage();
        self.program_roots = previous_program_roots;
        stats
    }

    /// Starts an incremental GC pass from the VM's current root snapshot.
    pub fn start_incremental_garbage(&mut self) -> bool {
        self.start_incremental_garbage_with_extra_roots(&[])
    }

    /// Advances active incremental GC by the configured safepoint budget.
    pub fn step_incremental_garbage(&mut self) -> Option<GcStats> {
        if self.heap.is_incremental_collection_active() {
            self.gc_stats.incremental_steps = self.gc_stats.incremental_steps.saturating_add(1);
        }
        let stats = self
            .heap
            .step_incremental_collection(self.limits.gc_incremental_step_budget);
        if stats.is_some() {
            self.field_cache.clear();
        }
        if let Some(stats) = stats {
            self.record_collection(stats);
            Some(stats)
        } else {
            None
        }
    }

    /// Completes active incremental GC immediately, returning stats if active.
    pub fn finish_incremental_garbage(&mut self) -> Option<GcStats> {
        let stats = self.heap.finish_incremental_collection();
        if stats.is_some() {
            self.field_cache.clear();
        }
        if let Some(stats) = stats {
            self.record_collection(stats);
            Some(stats)
        } else {
            None
        }
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

    /// Registers a custom extension handler that does not need direct VM context access.
    pub fn register_extension_fn(
        &mut self,
        id: impl Into<String>,
        extension: impl Fn(&[Value]) -> Result<Value, VmError> + 'static,
    ) {
        self.register_extension_context_fn(id, move |_ctx, args| extension(args));
    }

    /// Registers a custom extension handler that can allocate, inspect heap, or write output.
    pub fn register_extension_context_fn(
        &mut self,
        id: impl Into<String>,
        extension: impl for<'vm> Fn(&mut NativeContext<'vm>, &[Value]) -> Result<Value, VmError>
        + 'static,
    ) {
        self.extension_functions
            .insert(id.into(), Rc::new(extension));
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
            self.step_incremental_garbage();

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
                Instruction::AddInt { dst, lhs, rhs } => {
                    self.write_binary_int_specialized(
                        ip,
                        *dst,
                        *lhs,
                        *rhs,
                        "addition",
                        i64::checked_add,
                    )?;
                }
                Instruction::Sub { dst, lhs, rhs } => {
                    self.write_binary_int(ip, *dst, *lhs, *rhs, "subtraction", i64::checked_sub)?;
                }
                Instruction::SubInt { dst, lhs, rhs } => {
                    self.write_binary_int_specialized(
                        ip,
                        *dst,
                        *lhs,
                        *rhs,
                        "subtraction",
                        i64::checked_sub,
                    )?;
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
                Instruction::MulInt { dst, lhs, rhs } => {
                    self.write_binary_int_specialized(
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
                Instruction::DivInt { dst, lhs, rhs } => {
                    self.write_int_division_specialized(ip, *dst, *lhs, *rhs)?;
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
                Instruction::LessInt { dst, lhs, rhs } => {
                    self.write_int_comparison_specialized(ip, *dst, *lhs, *rhs, |lhs, rhs| {
                        lhs < rhs
                    })?;
                }
                Instruction::LessEqual { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, *dst, *lhs, *rhs, |lhs, rhs| lhs <= rhs)?;
                }
                Instruction::LessEqualInt { dst, lhs, rhs } => {
                    self.write_int_comparison_specialized(ip, *dst, *lhs, *rhs, |lhs, rhs| {
                        lhs <= rhs
                    })?;
                }
                Instruction::Greater { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, *dst, *lhs, *rhs, |lhs, rhs| lhs > rhs)?;
                }
                Instruction::GreaterInt { dst, lhs, rhs } => {
                    self.write_int_comparison_specialized(ip, *dst, *lhs, *rhs, |lhs, rhs| {
                        lhs > rhs
                    })?;
                }
                Instruction::GreaterEqual { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, *dst, *lhs, *rhs, |lhs, rhs| lhs >= rhs)?;
                }
                Instruction::GreaterEqualInt { dst, lhs, rhs } => {
                    self.write_int_comparison_specialized(ip, *dst, *lhs, *rhs, |lhs, rhs| {
                        lhs >= rhs
                    })?;
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
                Instruction::CallExtension {
                    dst,
                    extension,
                    args_start,
                    arg_count,
                } => {
                    let args = self.read_arguments(ip, *args_start, *arg_count)?;
                    let value = self.call_extension(chunk, ip, *extension, &args)?;
                    self.write_register(ip, *dst, value)?;
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
                Instruction::RecordNew {
                    dst,
                    fields_start,
                    fields,
                } => {
                    let fields = self.read_record_fields(ip, *fields_start, fields, chunk)?;
                    let reference = self.allocate_object(Obj::Record(fields))?;
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
                Instruction::FieldGet { dst, target, field } => {
                    let value = self.field_get(ip, *target, *field, chunk)?;
                    self.write_register(ip, *dst, value)?;
                }
                Instruction::FieldSet {
                    target,
                    field,
                    value,
                } => {
                    self.field_set(ip, *target, *field, *value, chunk)?;
                }
                Instruction::PushHandler { error, target } => {
                    self.push_exception_handler_for_chunk(
                        ip,
                        *error,
                        *target,
                        chunk.instructions.len(),
                    )?;
                }
                Instruction::PopHandler => {
                    self.pop_exception_handler();
                }
                Instruction::Throw { src } => {
                    let value = self.read_register(ip, *src)?;
                    if self.throw_value(ip, value)? {
                        continue;
                    }
                }
                Instruction::Return { src } => {
                    self.clear_chunk_exception_handlers();
                    return self.read_register(ip, *src);
                }
            }
        }
    }

    fn reset_for_chunk(&mut self, chunk: &Chunk) {
        self.instruction_ip = 0;
        self.executed_instruction_count = 0;
        self.registers = vec![Value::Nil; usize::from(chunk.register_count)];
        self.record_register_count(self.registers.len());
        self.frames.clear();
        self.exception_handlers.clear();
        self.field_cache.clear();
    }

    fn run_program_inner(
        &mut self,
        program: &Program,
        mut trace: Option<&mut dyn TraceWriter>,
        mut debugger: Option<&mut dyn Debugger>,
    ) -> Result<DebugOutcome, VmError> {
        self.frames.clear();
        self.exception_handlers.clear();
        self.registers.clear();
        self.field_cache.clear();
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
            self.record_register_count(self.registers.len());
            self.step_incremental_garbage();

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
                Instruction::AddInt { dst, lhs, rhs } => {
                    self.write_binary_int_specialized(
                        ip,
                        dst,
                        lhs,
                        rhs,
                        "addition",
                        i64::checked_add,
                    )?;
                }
                Instruction::Sub { dst, lhs, rhs } => {
                    self.write_binary_int(ip, dst, lhs, rhs, "subtraction", i64::checked_sub)?;
                }
                Instruction::SubInt { dst, lhs, rhs } => {
                    self.write_binary_int_specialized(
                        ip,
                        dst,
                        lhs,
                        rhs,
                        "subtraction",
                        i64::checked_sub,
                    )?;
                }
                Instruction::Mul { dst, lhs, rhs } => {
                    self.write_binary_int(ip, dst, lhs, rhs, "multiplication", i64::checked_mul)?;
                }
                Instruction::MulInt { dst, lhs, rhs } => {
                    self.write_binary_int_specialized(
                        ip,
                        dst,
                        lhs,
                        rhs,
                        "multiplication",
                        i64::checked_mul,
                    )?;
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
                Instruction::DivInt { dst, lhs, rhs } => {
                    self.write_int_division_specialized(ip, dst, lhs, rhs)?;
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
                Instruction::LessInt { dst, lhs, rhs } => {
                    self.write_int_comparison_specialized(ip, dst, lhs, rhs, |lhs, rhs| lhs < rhs)?;
                }
                Instruction::LessEqual { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, dst, lhs, rhs, |lhs, rhs| lhs <= rhs)?;
                }
                Instruction::LessEqualInt { dst, lhs, rhs } => {
                    self.write_int_comparison_specialized(ip, dst, lhs, rhs, |lhs, rhs| {
                        lhs <= rhs
                    })?;
                }
                Instruction::Greater { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, dst, lhs, rhs, |lhs, rhs| lhs > rhs)?;
                }
                Instruction::GreaterInt { dst, lhs, rhs } => {
                    self.write_int_comparison_specialized(ip, dst, lhs, rhs, |lhs, rhs| lhs > rhs)?;
                }
                Instruction::GreaterEqual { dst, lhs, rhs } => {
                    self.write_int_comparison(ip, dst, lhs, rhs, |lhs, rhs| lhs >= rhs)?;
                }
                Instruction::GreaterEqualInt { dst, lhs, rhs } => {
                    self.write_int_comparison_specialized(ip, dst, lhs, rhs, |lhs, rhs| {
                        lhs >= rhs
                    })?;
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
                Instruction::CallExtension {
                    dst,
                    extension,
                    args_start,
                    arg_count,
                } => {
                    let args = self.read_arguments(ip, args_start, arg_count)?;
                    let value = self.call_extension(chunk, ip, extension, &args)?;
                    self.write_register(ip, dst, value)?;
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
                Instruction::RecordNew {
                    dst,
                    fields_start,
                    fields,
                } => {
                    let fields = self.read_record_fields(ip, fields_start, &fields, chunk)?;
                    let reference = self.allocate_object(Obj::Record(fields))?;
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
                Instruction::FieldGet { dst, target, field } => {
                    let value = self.field_get(ip, target, field, chunk)?;
                    self.write_register(ip, dst, value)?;
                }
                Instruction::FieldSet {
                    target,
                    field,
                    value,
                } => {
                    self.field_set(ip, target, field, value, chunk)?;
                }
                Instruction::PushHandler { error, target } => {
                    self.push_exception_handler(ip, error, target, chunk.instructions.len())?;
                }
                Instruction::PopHandler => {
                    self.pop_exception_handler();
                }
                Instruction::Throw { src } => {
                    let value = self.read_register(ip, src)?;
                    if self.throw_value(ip, value)? {
                        continue;
                    }
                }
                Instruction::Return { src } => {
                    let value = self.read_register(ip, src)?;
                    self.clear_returning_frame_exception_handlers();
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
                self.require_capability(HostCapability::NativeCall, "call native function")
                    .map_err(|mut err| {
                        if err.instruction_ip.is_none() {
                            err.instruction_ip = Some(ip);
                        }
                        err
                    })?;
                self.execution_stats.native_calls =
                    self.execution_stats.native_calls.saturating_add(1);
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

    fn call_extension(
        &mut self,
        chunk: &Chunk,
        ip: usize,
        extension: StringId,
        args: &[Value],
    ) -> Result<Value, VmError> {
        self.require_capability(HostCapability::ExtensionCall, "call custom extension")
            .map_err(|mut err| {
                if err.instruction_ip.is_none() {
                    err.instruction_ip = Some(ip);
                }
                err
            })?;
        let id = chunk
            .strings
            .get(usize::from(extension.0))
            .cloned()
            .ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InvalidString {
                        string: extension,
                        string_count: chunk.strings.len(),
                    },
                )
            })?;
        let extension_fn = self.extension_functions.get(&id).cloned().ok_or_else(|| {
            VmError::new(Some(ip), VmErrorKind::MissingExtension { id: id.clone() })
        })?;
        let value = {
            let mut context = NativeContext::new(self);
            extension_fn(&mut context, args).map_err(|mut err| {
                if err.instruction_ip.is_none() {
                    err.instruction_ip = Some(ip);
                }
                err
            })?
        };
        self.audit_events.push(format!(
            "custom_extension_called id={id} arity={}",
            args.len()
        ));
        Ok(value)
    }

    fn push_exception_handler_for_chunk(
        &mut self,
        ip: usize,
        error_register: Register,
        target: JumpTarget,
        instruction_count: usize,
    ) -> Result<(), VmError> {
        let target_ip = usize::try_from(target.0).map_err(|_| {
            VmError::new(
                Some(ip),
                VmErrorKind::InvalidJumpTarget {
                    target,
                    instruction_count,
                },
            )
        })?;
        if target_ip >= instruction_count {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::InvalidJumpTarget {
                    target,
                    instruction_count,
                },
            ));
        }

        self.exception_handlers.push(ExceptionHandler {
            frame_depth: 0,
            error_register,
            target_ip,
        });
        Ok(())
    }

    fn push_exception_handler(
        &mut self,
        ip: usize,
        error_register: Register,
        target: JumpTarget,
        instruction_count: usize,
    ) -> Result<(), VmError> {
        let target_ip = usize::try_from(target.0).map_err(|_| {
            VmError::new(
                Some(ip),
                VmErrorKind::InvalidJumpTarget {
                    target,
                    instruction_count,
                },
            )
        })?;
        if target_ip >= instruction_count {
            return Err(VmError::new(
                Some(ip),
                VmErrorKind::InvalidJumpTarget {
                    target,
                    instruction_count,
                },
            ));
        }

        self.exception_handlers.push(ExceptionHandler {
            frame_depth: self.frames.len(),
            error_register,
            target_ip,
        });
        Ok(())
    }

    fn pop_exception_handler(&mut self) {
        self.exception_handlers.pop();
    }

    fn clear_chunk_exception_handlers(&mut self) {
        self.exception_handlers
            .retain(|handler| handler.frame_depth != 0);
    }

    fn clear_returning_frame_exception_handlers(&mut self) {
        let returning_depth = self.frames.len();
        self.exception_handlers
            .retain(|handler| handler.frame_depth < returning_depth);
    }

    fn throw_value(&mut self, ip: usize, value: Value) -> Result<bool, VmError> {
        self.execution_stats.thrown_errors = self.execution_stats.thrown_errors.saturating_add(1);
        let Some(handler) = self.exception_handlers.pop() else {
            return Err(VmError::new(Some(ip), VmErrorKind::UncaughtThrow { value }));
        };
        self.execution_stats.handled_exceptions =
            self.execution_stats.handled_exceptions.saturating_add(1);

        if handler.frame_depth == 0 {
            write_register_in(Some(ip), &mut self.registers, handler.error_register, value)?;
            self.instruction_ip = handler.target_ip;
            return Ok(true);
        }

        while self.frames.len() > handler.frame_depth {
            self.frames.pop();
        }
        let frame_index = handler
            .frame_depth
            .checked_sub(1)
            .ok_or_else(|| VmError::new(Some(ip), VmErrorKind::UncaughtThrow { value }))?;
        let frame = self
            .frames
            .get_mut(frame_index)
            .ok_or_else(|| VmError::new(Some(ip), VmErrorKind::UncaughtThrow { value }))?;
        write_register_in(
            Some(ip),
            &mut frame.registers,
            handler.error_register,
            value,
        )?;
        frame.ip = handler.target_ip;
        self.registers = frame.registers.clone();
        self.instruction_ip = handler.target_ip;
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
        self.heap.write_barrier_value(value);
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
        self.record_call_depth();
        if let Some(frame) = self.frames.last() {
            self.record_register_count(frame.registers.len());
        }
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
        self.heap.write_barrier_value(value);

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
                self.heap.write_barrier_value(key);
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
        self.heap.write_barrier_value(value);

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

    fn read_int_specialized(&self, ip: usize, register: Register) -> Result<i64, VmError> {
        match self.registers.get(usize::from(register.0)).copied() {
            Some(Value::Int(value)) => Ok(value),
            Some(found) => Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "int",
                    found,
                },
            )),
            None => Err(VmError::new(
                Some(ip),
                VmErrorKind::InvalidRegister {
                    register,
                    register_count: self.register_count(),
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

    fn read_record_fields(
        &self,
        ip: usize,
        fields_start: Register,
        field_ids: &[StringId],
        chunk: &Chunk,
    ) -> Result<Vec<(String, Value)>, VmError> {
        let mut fields = Vec::with_capacity(field_ids.len());
        for (offset, field_id) in field_ids.iter().enumerate() {
            let register = fields_start
                .0
                .checked_add(offset.try_into().unwrap_or(u8::MAX))
                .ok_or_else(|| {
                    VmError::new(
                        Some(ip),
                        VmErrorKind::InvalidRegister {
                            register: Register(u8::MAX),
                            register_count: self.register_count(),
                        },
                    )
                })?;
            let field = self.read_field_name(ip, *field_id, chunk)?;
            let value = self.read_register(ip, Register(register))?;
            if let Some(index) = record_field_index(&fields, &field) {
                fields[index].1 = value;
            } else {
                fields.push((field, value));
            }
        }
        Ok(fields)
    }

    fn field_get(
        &mut self,
        ip: usize,
        target_register: Register,
        field: StringId,
        chunk: &Chunk,
    ) -> Result<Value, VmError> {
        let target = self.read_record_ref(ip, target_register)?;
        let field = self.read_field_name(ip, field, chunk)?;
        let key = FieldCacheKey {
            object: target,
            field: field.clone(),
        };
        let cached_index = self.field_cache.get(&key).copied();
        let lookup = {
            let Obj::Record(fields) = self.heap.get(target)? else {
                let found = self.read_register(ip, target_register)?;
                return Err(VmError::new(
                    Some(ip),
                    VmErrorKind::TypeError {
                        expected: "record",
                        found,
                    },
                ));
            };

            if let Some(index) = cached_index
                && fields
                    .get(index)
                    .is_some_and(|(existing_field, _)| existing_field == &field)
            {
                return Ok(fields[index].1);
            }

            record_field_index(fields, &field).map(|index| (index, fields[index].1))
        };

        let Some((index, value)) = lookup else {
            return Ok(Value::Nil);
        };
        self.field_cache.insert(key, index);
        Ok(value)
    }

    fn field_set(
        &mut self,
        ip: usize,
        target_register: Register,
        field: StringId,
        value_register: Register,
        chunk: &Chunk,
    ) -> Result<(), VmError> {
        let target = self.read_record_ref(ip, target_register)?;
        let field = self.read_field_name(ip, field, chunk)?;
        let value = self.read_register(ip, value_register)?;
        let found = self.read_register(ip, target_register)?;
        self.heap.write_barrier_value(value);
        let key = FieldCacheKey {
            object: target,
            field: field.clone(),
        };
        let cached_index = self.field_cache.get(&key).copied();

        let index = {
            let Obj::Record(fields) = self.heap.get_mut(target)? else {
                return Err(VmError::new(
                    Some(ip),
                    VmErrorKind::TypeError {
                        expected: "record",
                        found,
                    },
                ));
            };

            if let Some(index) = cached_index
                && fields
                    .get(index)
                    .is_some_and(|(existing_field, _)| existing_field == &field)
            {
                fields[index].1 = value;
                index
            } else if let Some(index) = record_field_index(fields, &field) {
                fields[index].1 = value;
                index
            } else {
                fields.push((field, value));
                fields.len() - 1
            }
        };
        self.field_cache.insert(key, index);
        Ok(())
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

    fn read_record_ref(&self, ip: usize, register: Register) -> Result<ObjRef, VmError> {
        match self.read_register(ip, register)? {
            Value::Obj(reference) => Ok(reference),
            found => Err(VmError::new(
                Some(ip),
                VmErrorKind::TypeError {
                    expected: "record",
                    found,
                },
            )),
        }
    }

    fn read_field_name(
        &self,
        ip: usize,
        field: StringId,
        chunk: &Chunk,
    ) -> Result<String, VmError> {
        chunk
            .strings
            .get(usize::from(field.0))
            .cloned()
            .ok_or_else(|| {
                VmError::new(
                    Some(ip),
                    VmErrorKind::InvalidString {
                        string: field,
                        string_count: chunk.strings.len(),
                    },
                )
            })
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

    fn write_binary_int_specialized(
        &mut self,
        ip: usize,
        dst: Register,
        lhs: Register,
        rhs: Register,
        operation: &'static str,
        op: fn(i64, i64) -> Option<i64>,
    ) -> Result<(), VmError> {
        let lhs = self.read_int_specialized(ip, lhs)?;
        let rhs = self.read_int_specialized(ip, rhs)?;
        let value = op(lhs, rhs)
            .ok_or_else(|| VmError::new(Some(ip), VmErrorKind::ArithmeticOverflow { operation }))?;

        self.write_register(ip, dst, Value::Int(value))
    }

    fn write_int_division_specialized(
        &mut self,
        ip: usize,
        dst: Register,
        lhs: Register,
        rhs: Register,
    ) -> Result<(), VmError> {
        let rhs_value = self.read_int_specialized(ip, rhs)?;
        if rhs_value == 0 {
            return Err(VmError::new(Some(ip), VmErrorKind::DivisionByZero));
        }

        let lhs_value = self.read_int_specialized(ip, lhs)?;
        let value = lhs_value.checked_div(rhs_value).ok_or_else(|| {
            VmError::new(
                Some(ip),
                VmErrorKind::ArithmeticOverflow {
                    operation: "division",
                },
            )
        })?;
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

    fn write_int_comparison_specialized(
        &mut self,
        ip: usize,
        dst: Register,
        lhs: Register,
        rhs: Register,
        op: fn(i64, i64) -> bool,
    ) -> Result<(), VmError> {
        let lhs = self.read_int_specialized(ip, lhs)?;
        let rhs = self.read_int_specialized(ip, rhs)?;
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

    fn should_collect_before_allocation(&self) -> bool {
        self.limits.gc_allocation_threshold > 0
            && self.gc_stats.allocation_pressure >= self.limits.gc_allocation_threshold
    }

    fn collect_garbage_with_extra_roots(&mut self, extra_roots: &[ObjRef]) -> GcStats {
        let roots = self.allocation_roots(extra_roots);
        let stats = self.heap.collect_garbage(&roots);
        self.field_cache.clear();
        self.record_collection(stats);
        stats
    }

    fn start_incremental_garbage_with_extra_roots(&mut self, extra_roots: &[ObjRef]) -> bool {
        let roots = self.allocation_roots(extra_roots);
        self.heap.start_incremental_collection(&roots)
    }

    fn record_allocation(&mut self) {
        self.gc_stats.allocations = self.gc_stats.allocations.saturating_add(1);
        self.gc_stats.allocation_pressure = self.gc_stats.allocation_pressure.saturating_add(1);
    }

    fn record_collection(&mut self, stats: GcStats) {
        self.gc_stats.collections = self.gc_stats.collections.saturating_add(1);
        self.gc_stats.total_marked = self.gc_stats.total_marked.saturating_add(stats.marked);
        self.gc_stats.total_swept = self.gc_stats.total_swept.saturating_add(stats.swept);
        self.gc_stats.live_after_last_collection = stats.live;
        self.gc_stats.last_collection = stats;
        self.gc_stats.allocation_pressure = 0;
    }

    fn record_call_depth(&mut self) {
        self.execution_stats.max_call_depth =
            self.execution_stats.max_call_depth.max(self.frames.len());
    }

    fn record_register_count(&mut self, register_count: usize) {
        self.execution_stats.max_register_count =
            self.execution_stats.max_register_count.max(register_count);
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

fn record_field_index(fields: &[(String, Value)], field: &str) -> Option<usize> {
    fields
        .iter()
        .position(|(existing_field, _)| existing_field == field)
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
        Obj::Record(fields) => fields
            .iter()
            .filter_map(|(_, value)| value.as_obj_ref())
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
