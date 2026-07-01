//! Runtime configuration loading.
//!
//! Ferrix intentionally keeps runtime configuration local and file based. This
//! module does not read environment variables; CLI flags may override these
//! values at the command boundary.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{RuntimeError, RuntimeErrorKind, RuntimeMode, RuntimeProfile};

/// Runtime log verbosity selected by config.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeLogLevel {
    /// Only errors and lifecycle state.
    Error,
    /// Warnings and errors.
    Warn,
    /// Normal operational events.
    #[default]
    Info,
    /// Verbose request tracing.
    Debug,
    /// Very verbose internal tracing.
    Trace,
}

impl RuntimeLogLevel {
    /// Stable lowercase config name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

/// Runtime behavior loaded from built-in defaults and local config files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Default runtime mode for CLI commands.
    pub mode: RuntimeMode,
    /// Whether managed clients may auto-start the daemon.
    pub auto_start: bool,
    /// Runtime service home relative to Ferrix home unless absolute.
    pub home: PathBuf,
    /// Optional Unix socket override.
    pub socket_path: Option<PathBuf>,
    /// Default runtime profile for future service callers.
    pub default_profile: RuntimeProfile,
    /// Runtime log verbosity.
    pub log_level: RuntimeLogLevel,
    /// Whether audit collection is enabled by default.
    pub audit_enabled: bool,
    /// Whether stats collection is enabled by default.
    pub stats_enabled: bool,
    /// Middleware request timeout.
    pub request_timeout_ms: u64,
    /// Maximum active daemon process records.
    pub max_concurrent_runtime_processes: usize,
    /// Global daemon request rate limit.
    pub rate_limit_per_second: u32,
}

impl RuntimeConfig {
    /// Built-in runtime defaults.
    pub fn builtin_default() -> Self {
        Self {
            mode: RuntimeMode::Embedded,
            auto_start: true,
            home: PathBuf::from("services/runtime"),
            socket_path: None,
            default_profile: RuntimeProfile::Cli,
            log_level: RuntimeLogLevel::Info,
            audit_enabled: false,
            stats_enabled: false,
            request_timeout_ms: 30_000,
            max_concurrent_runtime_processes: 16,
            rate_limit_per_second: 64,
        }
    }

    /// Loads config from a local file, falling back to built-in defaults when missing.
    pub fn load(path: &Path) -> Result<Self, RuntimeError> {
        match fs::read_to_string(path) {
            Ok(source) => Self::parse(&source, path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::builtin_default()),
            Err(error) => Err(RuntimeError::new(
                66,
                RuntimeErrorKind::Read {
                    path: path.to_path_buf(),
                    message: error.to_string(),
                },
            )),
        }
    }

    /// Parses a minimal TOML-like config source used by Ferrix's generated config.
    pub fn parse(source: &str, path: &Path) -> Result<Self, RuntimeError> {
        let mut config = Self::builtin_default();
        let mut section = String::new();

        for (line_index, raw_line) in source.lines().enumerate() {
            let line_number = line_index + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                section = name.trim().to_string();
                continue;
            }
            let Some((raw_key, raw_value)) = line.split_once('=') else {
                return Err(config_error(path, line_number, "expected `key = value`"));
            };
            let key = raw_key.trim();
            let value = parse_string(raw_value.trim(), path, line_number)?;
            match (section.as_str(), key) {
                ("runtime", "mode") => {
                    config.mode = value
                        .parse::<RuntimeMode>()
                        .map_err(|error| config_error(path, line_number, &error.to_string()))?;
                }
                ("runtime", "home") | ("services", "runtime") => {
                    config.home = PathBuf::from(value);
                }
                ("runtime", "auto_start") => {
                    config.auto_start = parse_bool(&value, path, line_number)?;
                }
                ("runtime", "socket") => {
                    config.socket_path = (!value.is_empty()).then(|| PathBuf::from(value));
                }
                ("runtime", "default_profile") => {
                    config.default_profile = value
                        .parse::<RuntimeProfile>()
                        .map_err(|error| config_error(path, line_number, &error.to_string()))?;
                }
                ("runtime", "log_level") => {
                    config.log_level = RuntimeLogLevel::parse(&value).ok_or_else(|| {
                        config_error(path, line_number, "invalid runtime log level")
                    })?;
                }
                ("runtime", "audit_enabled") => {
                    config.audit_enabled = parse_bool(&value, path, line_number)?;
                }
                ("runtime", "stats_enabled") => {
                    config.stats_enabled = parse_bool(&value, path, line_number)?;
                }
                ("runtime", "request_timeout_ms") => {
                    config.request_timeout_ms =
                        parse_number(&value, path, line_number, "request_timeout_ms")?;
                }
                ("runtime", "max_concurrent_processes") => {
                    config.max_concurrent_runtime_processes =
                        parse_number(&value, path, line_number, "max_concurrent_processes")?;
                }
                ("runtime", "rate_limit_per_second") => {
                    config.rate_limit_per_second =
                        parse_number(&value, path, line_number, "rate_limit_per_second")?;
                }
                _ => {}
            }
        }

        Ok(config)
    }

    /// Resolves the runtime home against the Ferrix home directory.
    pub fn resolved_home(&self, ferrix_home: &Path) -> PathBuf {
        if self.home.is_absolute() {
            self.home.clone()
        } else {
            ferrix_home.join(&self.home)
        }
    }

    /// Serializes the default local config file.
    pub fn default_config_source() -> String {
        [
            "# Ferrix local configuration.",
            "# This file is created next to the ferrix binary for local runtime services.",
            "",
            "[runtime]",
            "mode = \"embedded\"",
            "home = \"services/runtime\"",
            "auto_start = true",
            "default_profile = \"cli\"",
            "log_level = \"info\"",
            "audit_enabled = false",
            "stats_enabled = false",
            "request_timeout_ms = 30000",
            "max_concurrent_processes = 16",
            "rate_limit_per_second = 64",
            "",
            "[services]",
            "runtime = \"services/runtime\"",
            "",
        ]
        .join("\n")
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::builtin_default()
    }
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(before, _)| before)
}

fn parse_string(value: &str, path: &Path, line_number: usize) -> Result<String, RuntimeError> {
    let value = value.trim();
    if value == "true" || value == "false" || value.parse::<u64>().is_ok() {
        return Ok(value.to_string());
    }
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_string)
        .ok_or_else(|| config_error(path, line_number, "expected string, bool, or integer value"))
}

fn parse_bool(value: &str, path: &Path, line_number: usize) -> Result<bool, RuntimeError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(config_error(path, line_number, "expected boolean value")),
    }
}

fn parse_number<T>(
    value: &str,
    path: &Path,
    line_number: usize,
    field: &str,
) -> Result<T, RuntimeError>
where
    T: std::str::FromStr,
{
    value
        .parse::<T>()
        .map_err(|_| config_error(path, line_number, &format!("invalid integer for `{field}`")))
}

fn config_error(path: &Path, line_number: usize, message: &str) -> RuntimeError {
    RuntimeError::new(
        65,
        RuntimeErrorKind::Manifest {
            path: path.to_path_buf(),
            message: format!("{message} at line {line_number}"),
        },
    )
}
