//! Runtime socket protocol versioning and compatibility checks.
//!
//! The protocol is intentionally tiny: the CLI and daemon exchange a `HELLO`
//! request before daemon-backed work so incompatible major versions fail with a
//! clear error before execution starts.

use std::fmt;

/// Runtime protocol version used by CLI/daemon requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeProtocolVersion {
    /// Breaking protocol version.
    pub major: u16,
    /// Backward-compatible protocol revision.
    pub minor: u16,
}

impl RuntimeProtocolVersion {
    /// Creates a protocol version.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Parses `major.minor`.
    pub fn parse(value: &str) -> Option<Self> {
        let (major, minor) = value.split_once('.')?;
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }

    /// Returns true when this version fits the supported range.
    pub fn is_supported_by(self, min: RuntimeProtocolVersion, max: RuntimeProtocolVersion) -> bool {
        self >= min && self <= max
    }
}

impl fmt::Display for RuntimeProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Current CLI/runtime protocol version.
pub const CURRENT_PROTOCOL_VERSION: RuntimeProtocolVersion = RuntimeProtocolVersion::new(1, 0);
/// Oldest protocol this crate can speak.
pub const MIN_SUPPORTED_PROTOCOL_VERSION: RuntimeProtocolVersion =
    RuntimeProtocolVersion::new(1, 0);
/// Newest protocol this crate can speak.
pub const MAX_SUPPORTED_PROTOCOL_VERSION: RuntimeProtocolVersion =
    RuntimeProtocolVersion::new(1, 0);

/// Daemon protocol capabilities returned by `HELLO`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeProtocolInfo {
    /// Runtime crate version.
    pub runtime_version: String,
    /// Daemon protocol version.
    pub protocol_version: RuntimeProtocolVersion,
    /// Minimum supported daemon protocol.
    pub supported_min: RuntimeProtocolVersion,
    /// Maximum supported daemon protocol.
    pub supported_max: RuntimeProtocolVersion,
    /// Stable protocol feature names.
    pub features: Vec<String>,
}

impl RuntimeProtocolInfo {
    /// Returns the protocol info for the current crate.
    pub fn current() -> Self {
        Self {
            runtime_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            supported_min: MIN_SUPPORTED_PROTOCOL_VERSION,
            supported_max: MAX_SUPPORTED_PROTOCOL_VERSION,
            features: vec![
                "runtime.lifecycle".to_string(),
                "process.history".to_string(),
                "bytecode.container".to_string(),
                "request.identity".to_string(),
                "middleware.basic".to_string(),
            ],
        }
    }

    /// Returns true when the current CLI can talk to this daemon.
    pub fn is_compatible_with_current(&self) -> bool {
        self.protocol_version.is_supported_by(
            MIN_SUPPORTED_PROTOCOL_VERSION,
            MAX_SUPPORTED_PROTOCOL_VERSION,
        ) && CURRENT_PROTOCOL_VERSION.is_supported_by(self.supported_min, self.supported_max)
    }

    /// Encodes protocol info for the local line-based daemon protocol.
    pub fn encode(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.runtime_version,
            self.protocol_version,
            self.supported_min,
            self.supported_max,
            self.features.join(",")
        )
    }

    /// Decodes protocol info from the local line-based daemon protocol.
    pub fn decode(value: &str) -> Option<Self> {
        let mut parts = value.split('\t');
        let runtime_version = parts.next()?.to_string();
        let protocol_version = RuntimeProtocolVersion::parse(parts.next()?)?;
        let supported_min = RuntimeProtocolVersion::parse(parts.next()?)?;
        let supported_max = RuntimeProtocolVersion::parse(parts.next()?)?;
        let features = parts
            .next()
            .unwrap_or_default()
            .split(',')
            .filter(|feature| !feature.is_empty())
            .map(str::to_string)
            .collect();
        Some(Self {
            runtime_version,
            protocol_version,
            supported_min,
            supported_max,
            features,
        })
    }

    /// Renders a CLI-friendly mismatch message.
    pub fn mismatch_message(&self) -> String {
        format!(
            "Ferrix runtime protocol mismatch.\nCLI supports protocol {}-{}, daemon speaks protocol {}.\n",
            MIN_SUPPORTED_PROTOCOL_VERSION, MAX_SUPPORTED_PROTOCOL_VERSION, self.protocol_version
        )
    }
}
