//! Runtime profile definitions.
//!
//! Profiles are intentionally small in the first phase. They provide the stable
//! place where later capability, audit, daemon, and host policies can attach.

use std::{error::Error, fmt, str::FromStr};

use ferrix_vm::HostCapability;
use ferrix_vm::RuntimeLimits;

/// Named runtime behavior profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeProfile {
    /// Local development defaults with normal VM limits.
    Development,
    /// Conservative profile reserved for restricted execution.
    Safe,
    /// Default command-line execution profile.
    #[default]
    Cli,
    /// Long-running service profile reserved for daemon mode.
    Server,
    /// Privileged local profile for trusted embeddings and tests.
    Trusted,
}

impl RuntimeProfile {
    /// Returns the stable lowercase profile name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Safe => "safe",
            Self::Cli => "cli",
            Self::Server => "server",
            Self::Trusted => "trusted",
        }
    }

    /// Returns VM limits for this profile.
    pub fn limits(self) -> RuntimeLimits {
        match self {
            Self::Development | Self::Cli | Self::Trusted => RuntimeLimits::default(),
            Self::Safe => RuntimeLimits {
                max_instruction_count: 100_000,
                max_call_depth: 128,
                max_heap_objects: 100_000,
                gc_allocation_threshold: 1_024,
                gc_incremental_step_budget: 32,
            },
            Self::Server => RuntimeLimits {
                max_instruction_count: 500_000,
                max_call_depth: 256,
                max_heap_objects: 250_000,
                gc_allocation_threshold: 2_048,
                gc_incremental_step_budget: 64,
            },
        }
    }

    /// Returns default host capabilities granted by this profile.
    pub fn default_capabilities(self) -> &'static [HostCapability] {
        match self {
            Self::Development => &[
                HostCapability::NativeCall,
                HostCapability::IoOutput,
                HostCapability::FsRead,
                HostCapability::FsWrite,
                HostCapability::EnvRead,
                HostCapability::TimeRead,
                HostCapability::ModuleLoad,
                HostCapability::ExtensionCall,
            ],
            Self::Safe => &[],
            Self::Cli => &[
                HostCapability::NativeCall,
                HostCapability::IoOutput,
                HostCapability::ModuleLoad,
                HostCapability::TimeRead,
            ],
            Self::Server => &[
                HostCapability::NativeCall,
                HostCapability::ModuleLoad,
                HostCapability::TimeRead,
            ],
            Self::Trusted => &[
                HostCapability::NativeCall,
                HostCapability::IoOutput,
                HostCapability::FsRead,
                HostCapability::FsWrite,
                HostCapability::EnvRead,
                HostCapability::TimeRead,
                HostCapability::ModuleLoad,
                HostCapability::ExtensionCall,
            ],
        }
    }

    /// Returns true when audit collection should be on by default.
    pub fn audit_enabled(self) -> bool {
        matches!(self, Self::Safe | Self::Server)
    }
}

impl fmt::Display for RuntimeProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RuntimeProfile {
    type Err = RuntimeProfileParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "development" => Ok(Self::Development),
            "safe" => Ok(Self::Safe),
            "cli" => Ok(Self::Cli),
            "server" => Ok(Self::Server),
            "trusted" => Ok(Self::Trusted),
            _ => Err(RuntimeProfileParseError {
                value: value.to_string(),
            }),
        }
    }
}

/// Error returned when a runtime profile name is not recognized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeProfileParseError {
    value: String,
}

impl RuntimeProfileParseError {
    /// Returns the invalid raw profile value.
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for RuntimeProfileParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid runtime profile `{}`", self.value)
    }
}

impl Error for RuntimeProfileParseError {}
