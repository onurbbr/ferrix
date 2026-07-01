//! Runtime guardrails used to keep VM execution bounded.

/// Configurable limits checked while executing bytecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeLimits {
    /// Maximum number of bytecode instructions allowed per run.
    pub max_instruction_count: usize,
    /// Maximum number of nested call frames allowed on the VM stack.
    pub max_call_depth: usize,
    /// Maximum number of live heap objects allowed before allocation fails.
    pub max_heap_objects: usize,
    /// Successful allocations allowed between automatic GC checks.
    ///
    /// A value of `0` disables threshold-based collection. Heap-limit
    /// collection still runs as a final attempt before allocation failure.
    pub gc_allocation_threshold: usize,
    /// Incremental GC work units advanced at each VM safepoint.
    ///
    /// A work unit marks one grey object or sweeps one heap slot. Values below
    /// `1` are treated as one unit so active collections always make progress.
    pub gc_incremental_step_budget: usize,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            max_instruction_count: 1_000_000,
            max_call_depth: 1_024,
            max_heap_objects: 1_000_000,
            gc_allocation_threshold: 4_096,
            gc_incremental_step_budget: 64,
        }
    }
}
