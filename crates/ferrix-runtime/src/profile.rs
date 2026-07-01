//! Runtime profile definitions.
//!
//! Profiles are intentionally small in the first phase. They provide the stable
//! place where later capability, audit, daemon, and host policies can attach.

use std::{error::Error, fmt, str::FromStr};

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
            Self::Development | Self::Cli | Self::Safe | Self::Server | Self::Trusted => {
                RuntimeLimits::default()
            }
        }
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
