//! Custom extension registry for host-provided runtime operations.
//!
//! Extensions are deliberately runtime-owned instead of bytecode-owned.
//! `CallExtension` bytecode references a stable extension id, while this
//! registry validates policy and resolves the host handler.

use std::{collections::BTreeMap, sync::Arc};

use ferrix_core::Value;
use ferrix_vm::HostCapability;

use crate::{RuntimeError, RuntimeErrorKind, RuntimePolicy};

/// Synchronous custom extension handler.
pub trait CustomExtensionHandler: Send + Sync {
    /// Executes an extension call with already-decoded Ferrix values.
    fn call(&self, args: &[Value]) -> Result<Value, RuntimeError>;
}

impl<F> CustomExtensionHandler for F
where
    F: Fn(&[Value]) -> Result<Value, RuntimeError> + Send + Sync,
{
    fn call(&self, args: &[Value]) -> Result<Value, RuntimeError> {
        self(args)
    }
}

/// Estimated extension cost for scheduling and future policy decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionCostClass {
    /// Pure or near-pure local work.
    Cheap,
    /// Normal local host work.
    Normal,
    /// Potentially expensive or externally visible host work.
    External,
}

/// Stable extension metadata exposed to runtime inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomExtensionMetadata {
    /// Stable extension id referenced by bytecode.
    pub id: String,
    /// Human-readable extension name.
    pub name: String,
    /// Declared positional argument count.
    pub arity: u8,
    /// Optional output register shape reserved for future verifier integration.
    pub output_register: Option<u8>,
    /// Capabilities required in addition to `extension.call`.
    pub required_capabilities: Vec<HostCapability>,
    /// Estimated cost class.
    pub cost: ExtensionCostClass,
    /// Documentation string for CLI/runtime inspection.
    pub docs: String,
}

/// Registered extension plus host handler.
#[derive(Clone)]
pub struct CustomExtension {
    metadata: CustomExtensionMetadata,
    handler: Arc<dyn CustomExtensionHandler>,
}

impl CustomExtension {
    /// Creates an extension from metadata and a sync host handler.
    pub fn new(
        metadata: CustomExtensionMetadata,
        handler: impl CustomExtensionHandler + 'static,
    ) -> Self {
        Self {
            metadata,
            handler: Arc::new(handler),
        }
    }

    /// Returns extension metadata.
    pub fn metadata(&self) -> &CustomExtensionMetadata {
        &self.metadata
    }
}

/// Result of a custom extension call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomExtensionCallResult {
    /// Returned Ferrix value.
    pub value: Value,
    /// Audit event emitted for the call.
    pub audit_event: String,
}

/// Runtime-owned custom extension registry.
#[derive(Clone, Default)]
pub struct RuntimeExtensionRegistry {
    extensions: BTreeMap<String, CustomExtension>,
}

impl RuntimeExtensionRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or replaces one custom extension.
    pub fn register(&mut self, extension: CustomExtension) {
        self.extensions
            .insert(extension.metadata.id.clone(), extension);
    }

    /// Returns true when an extension id has a registered handler.
    pub fn contains(&self, id: &str) -> bool {
        self.extensions.contains_key(id)
    }

    /// Returns metadata for all registered extensions.
    pub fn metadata(&self) -> Vec<CustomExtensionMetadata> {
        self.extensions
            .values()
            .map(|extension| extension.metadata.clone())
            .collect()
    }

    /// Calls a registered extension after policy and arity validation.
    pub fn call(
        &self,
        id: &str,
        args: &[Value],
        policy: &RuntimePolicy,
    ) -> Result<CustomExtensionCallResult, RuntimeError> {
        let extension = self.extensions.get(id).ok_or_else(|| {
            RuntimeError::new(
                70,
                RuntimeErrorKind::MissingExtension { id: id.to_string() },
            )
        })?;
        if args.len() != usize::from(extension.metadata.arity) {
            return Err(RuntimeError::new(
                70,
                RuntimeErrorKind::Execution(format!(
                    "extension `{id}` expected {} arguments, got {}",
                    extension.metadata.arity,
                    args.len()
                )),
            ));
        }
        require_extension_capability(policy, HostCapability::ExtensionCall)?;
        for capability in &extension.metadata.required_capabilities {
            require_extension_capability(policy, *capability)?;
        }
        let value = extension.handler.call(args)?;
        Ok(CustomExtensionCallResult {
            value,
            audit_event: format!("custom_extension_called id={id} arity={}", args.len()),
        })
    }
}

fn require_extension_capability(
    policy: &RuntimePolicy,
    capability: HostCapability,
) -> Result<(), RuntimeError> {
    policy
        .require_capability(capability, "call custom extension")
        .map_err(|error| {
            RuntimeError::new(
                70,
                RuntimeErrorKind::PolicyDenied {
                    message: error.to_string(),
                },
            )
        })
}
