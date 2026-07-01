//! Runtime profile, capability, and native availability policy rules.

use std::{collections::BTreeSet, fmt};

use ferrix_stdlib::NativeBinding;
use ferrix_vm::HostCapability;

use crate::RuntimeProfile;

/// Individual policy rule evaluated before host-visible runtime behavior.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyRule {
    /// Runtime profile grants the capability by default.
    ProfileAllowsCapability,
    /// Runtime request explicitly grants the capability.
    RequestGrantsCapability,
    /// Native binding is available in the selected profile.
    NativeAvailableInProfile,
    /// Module load path is allowed by the selected policy.
    ModulePathAllowed,
    /// Custom extension is registered before use.
    ExtensionRegistered,
    /// Daemon command is allowed for the caller/session.
    DaemonCommandAllowed,
}

/// Structured policy failure used by runtime diagnostics and audit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyFailure {
    /// Rule that rejected the operation.
    pub rule: PolicyRule,
    /// Profile active when the rule was evaluated.
    pub profile: RuntimeProfile,
    /// Capability involved in the rejection, if any.
    pub capability: Option<HostCapability>,
    /// Human-readable operation name.
    pub operation: &'static str,
    /// Optional native function name involved in the rejection.
    pub native: Option<&'static str>,
}

impl PolicyFailure {
    /// Creates a capability failure for one operation.
    pub fn capability(
        rule: PolicyRule,
        profile: RuntimeProfile,
        capability: HostCapability,
        operation: &'static str,
    ) -> Self {
        Self {
            rule,
            profile,
            capability: Some(capability),
            operation,
            native: None,
        }
    }

    /// Creates a native availability failure.
    pub fn native_unavailable(profile: RuntimeProfile, native: &'static str) -> Self {
        Self {
            rule: PolicyRule::NativeAvailableInProfile,
            profile,
            capability: None,
            operation: "call native function",
            native: Some(native),
        }
    }
}

impl fmt::Display for PolicyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.capability, self.native) {
            (Some(capability), _) => write!(
                f,
                "policy denied `{}` for profile `{}` while trying to {}",
                capability.as_str(),
                self.profile.as_str(),
                self.operation
            ),
            (_, Some(native)) => write!(
                f,
                "policy denied native `{native}` for profile `{}`",
                self.profile.as_str()
            ),
            _ => write!(
                f,
                "policy denied operation `{}` for profile `{}`",
                self.operation,
                self.profile.as_str()
            ),
        }
    }
}

/// Profile-aware runtime policy evaluator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimePolicy {
    profile: RuntimeProfile,
    explicit_capabilities: BTreeSet<HostCapability>,
}

impl RuntimePolicy {
    /// Creates a policy for a runtime profile plus request-level grants.
    pub fn new(
        profile: RuntimeProfile,
        explicit_capabilities: impl IntoIterator<Item = HostCapability>,
    ) -> Self {
        Self {
            profile,
            explicit_capabilities: explicit_capabilities.into_iter().collect(),
        }
    }

    /// Returns the profile this policy evaluates.
    pub fn profile(&self) -> RuntimeProfile {
        self.profile
    }

    /// Returns the complete capability set granted by profile and request.
    pub fn granted_capabilities(&self) -> BTreeSet<HostCapability> {
        let mut capabilities = self
            .profile
            .default_capabilities()
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        capabilities.extend(self.explicit_capabilities.iter().copied());
        capabilities
    }

    /// Returns true when the profile or request grants a capability.
    pub fn allows_capability(&self, capability: HostCapability) -> bool {
        self.profile.default_capabilities().contains(&capability)
            || self.explicit_capabilities.contains(&capability)
    }

    /// Requires a capability for one host-visible operation.
    pub fn require_capability(
        &self,
        capability: HostCapability,
        operation: &'static str,
    ) -> Result<(), PolicyFailure> {
        if self.allows_capability(capability) {
            Ok(())
        } else {
            Err(PolicyFailure::capability(
                PolicyRule::ProfileAllowsCapability,
                self.profile,
                capability,
                operation,
            ))
        }
    }

    /// Returns true when a native binding is available in this profile.
    pub fn native_available(&self, binding: &NativeBinding) -> bool {
        binding
            .available_profiles
            .iter()
            .any(|profile| *profile == self.profile.as_str())
    }

    /// Requires that a native binding can be installed for this profile.
    pub fn require_native_available(&self, binding: &NativeBinding) -> Result<(), PolicyFailure> {
        if self.native_available(binding) {
            Ok(())
        } else {
            Err(PolicyFailure::native_unavailable(
                self.profile,
                binding.qualified_name,
            ))
        }
    }
}
