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
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            max_instruction_count: 1_000_000,
            max_call_depth: 1_024,
            max_heap_objects: 1_000_000,
        }
    }
}
