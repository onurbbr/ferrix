//! Runtime profile definitions.
//!
//! Profiles are intentionally small in the first phase. They provide the stable
//! place where later capability, audit, daemon, and host policies can attach.

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
    /// Returns VM limits for this profile.
    pub fn limits(self) -> RuntimeLimits {
        match self {
            Self::Development | Self::Cli | Self::Safe | Self::Server | Self::Trusted => {
                RuntimeLimits::default()
            }
        }
    }
}
