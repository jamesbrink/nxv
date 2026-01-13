//! Unified logging infrastructure for nxv.
//!
//! Provides consistent logging configuration across CLI, server, and indexer.
//!
//! # Environment Variables
//!
//! - `NXV_LOG` - Log filter (overrides RUST_LOG)
//! - `NXV_LOG_LEVEL` - Log level: error, warn, info, debug, trace
//! - `NXV_LOG_FORMAT` - Output format: pretty, compact, json
//! - `NXV_LOG_FILE` - Path to log file (in addition to stderr)
//! - `RUST_LOG` - Standard Rust log filter (fallback)
//!
//! # Example
//!
//! ```no_run
//! use nxv::logging::{LogConfig, init};
//!
//! // Initialize with default settings
//! init(LogConfig::default());
//!
//! // Or with custom configuration
//! let config = LogConfig::for_server().with_env_overrides();
//! init(config);
//! ```

use std::path::PathBuf;
use std::str::FromStr;

use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    EnvFilter,
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

/// Log output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable pretty format (default for development)
    #[default]
    Pretty,
    /// Compact single-line format
    Compact,
    /// JSON format for log aggregation systems
    Json,
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pretty" | "full" => Ok(Self::Pretty),
            "compact" => Ok(Self::Compact),
            "json" => Ok(Self::Json),
            _ => Err(format!(
                "Unknown log format: '{}'. Valid options: pretty, compact, json",
                s
            )),
        }
    }
}

impl std::fmt::Display for LogFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pretty => write!(f, "pretty"),
            Self::Compact => write!(f, "compact"),
            Self::Json => write!(f, "json"),
        }
    }
}

/// Log rotation configuration for file output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogRotation {
    /// Rotate hourly
    Hourly,
    /// Rotate daily (default)
    #[default]
    Daily,
    /// Never rotate (single file)
    Never,
}

impl FromStr for LogRotation {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "hourly" => Ok(Self::Hourly),
            "daily" => Ok(Self::Daily),
            "never" => Ok(Self::Never),
            _ => Err(format!(
                "Unknown log rotation: '{}'. Valid options: hourly, daily, never",
                s
            )),
        }
    }
}

impl From<LogRotation> for Rotation {
    fn from(rotation: LogRotation) -> Self {
        match rotation {
            LogRotation::Hourly => Rotation::HOURLY,
            LogRotation::Daily => Rotation::DAILY,
            LogRotation::Never => Rotation::NEVER,
        }
    }
}

/// Logging configuration.
///
/// Use the builder methods to customize, then pass to [`init`] or [`init_with_file`].
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Base log level (default: INFO)
    pub level: Level,
    /// Log format (default: Pretty)
    pub format: LogFormat,
    /// Path to log file (None = stderr only)
    pub file_path: Option<PathBuf>,
    /// Log rotation for file output (default: Daily)
    pub rotation: LogRotation,
    /// Log span timing on close (default: false, true for indexer)
    pub span_events: bool,
    /// Custom filter string (overrides level if set)
    pub filter: Option<String>,
    /// Show target module in logs (default: true)
    pub show_target: bool,
    /// Show thread IDs (default: false)
    pub show_thread_ids: bool,
    /// Show line numbers (default: false)
    pub show_line_numbers: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: Level::INFO,
            format: LogFormat::Pretty,
            file_path: None,
            rotation: LogRotation::Daily,
            span_events: false,
            filter: None,
            show_target: true,
            show_thread_ids: false,
            show_line_numbers: false,
        }
    }
}

impl LogConfig {
    /// Create a new LogConfig with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Preset for server mode.
    ///
    /// Uses INFO level, pretty format, shows target.
    pub fn for_server() -> Self {
        Self::default()
    }

    /// Preset for indexer mode.
    ///
    /// Same as default - span timing is disabled to keep logs clean.
    /// Use DEBUG level or `--span-events` to see span timing.
    pub fn for_indexer() -> Self {
        Self::default()
    }

    /// Set the log level.
    pub fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Set the log format.
    pub fn with_format(mut self, format: LogFormat) -> Self {
        self.format = format;
        self
    }

    /// Set the log file path.
    pub fn with_file(mut self, path: PathBuf) -> Self {
        self.file_path = Some(path);
        self
    }

    /// Set log rotation.
    pub fn with_rotation(mut self, rotation: LogRotation) -> Self {
        self.rotation = rotation;
        self
    }

    /// Enable span timing events.
    pub fn with_span_events(mut self, enabled: bool) -> Self {
        self.span_events = enabled;
        self
    }

    /// Set a custom filter string.
    pub fn with_filter(mut self, filter: String) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Apply environment variable overrides.
    ///
    /// Reads from:
    /// - `NXV_LOG` or `RUST_LOG` for filter (only if filter not already set from CLI)
    /// - `NXV_LOG_FORMAT` for format
    /// - `NXV_LOG_FILE` for file path
    /// - `NXV_LOG_LEVEL` for level (only if filter not already set)
    ///
    /// Note: CLI arguments take precedence over environment variables. If a filter
    /// is already set (e.g., from -v or --log-level), env vars won't override it.
    pub fn with_env_overrides(mut self) -> Self {
        // NXV_LOG takes precedence over RUST_LOG for filter (only if no filter already set)
        // This allows CLI args like -v or --log-level to take precedence over env vars
        if self.filter.is_none() {
            if let Ok(filter) = std::env::var("NXV_LOG") {
                self.filter = Some(filter);
            } else if let Ok(filter) = std::env::var("RUST_LOG") {
                self.filter = Some(filter);
            }
        }

        // NXV_LOG_LEVEL overrides level (only if no filter is set)
        if self.filter.is_none()
            && let Ok(level_str) = std::env::var("NXV_LOG_LEVEL")
        {
            self.level = parse_level(&level_str).unwrap_or(self.level);
        }

        // NXV_LOG_FORMAT overrides format
        if let Ok(format) = std::env::var("NXV_LOG_FORMAT")
            && let Ok(f) = format.parse()
        {
            self.format = f;
        }

        // NXV_LOG_FILE enables file logging
        if let Ok(path) = std::env::var("NXV_LOG_FILE") {
            self.file_path = Some(PathBuf::from(path));
        }

        self
    }

    /// Build the EnvFilter for this configuration.
    fn build_filter(&self) -> EnvFilter {
        if let Some(ref filter) = self.filter {
            EnvFilter::try_new(filter).unwrap_or_else(|_| {
                eprintln!("Warning: Invalid log filter '{}', using default", filter);
                EnvFilter::new(format!("{}", self.level).to_lowercase())
            })
        } else {
            EnvFilter::new(format!("{}", self.level).to_lowercase())
        }
    }
}

/// Parse a log level string.
fn parse_level(s: &str) -> Option<Level> {
    match s.to_lowercase().as_str() {
        "error" => Some(Level::ERROR),
        "warn" | "warning" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
}

/// Initialize the global tracing subscriber.
///
/// This should be called once at program startup. Subsequent calls are silently ignored.
///
/// For file logging, use [`init_with_file`] instead.
///
/// # Example
///
/// ```no_run
/// use nxv::logging::{LogConfig, init};
///
/// init(LogConfig::default());
/// ```
pub fn init(config: LogConfig) {
    let filter = config.build_filter();

    let span_events = if config.span_events {
        FmtSpan::CLOSE
    } else {
        FmtSpan::NONE
    };

    // Build subscriber based on format
    let result = match config.format {
        LogFormat::Json => {
            let layer = fmt::layer()
                .json()
                .with_span_events(span_events)
                .with_target(config.show_target)
                .with_writer(std::io::stderr);

            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
        }
        LogFormat::Compact => {
            let layer = fmt::layer()
                .compact()
                .with_span_events(span_events)
                .with_target(config.show_target)
                .with_thread_ids(config.show_thread_ids)
                .with_line_number(config.show_line_numbers)
                .with_writer(std::io::stderr);

            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
        }
        LogFormat::Pretty => {
            let layer = fmt::layer()
                .with_span_events(span_events)
                .with_target(config.show_target)
                .with_thread_ids(config.show_thread_ids)
                .with_line_number(config.show_line_numbers)
                .with_writer(std::io::stderr);

            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
        }
    };

    // Silently ignore if already initialized (idempotent)
    let _ = result;
}

/// Initialize logging with file appender support.
///
/// Logs to both stderr and the configured file. Use this when `config.file_path` is set.
///
/// # Example
///
/// ```no_run
/// use std::path::PathBuf;
/// use nxv::logging::{LogConfig, init_with_file};
///
/// let config = LogConfig::default().with_file(PathBuf::from("/var/log/nxv.log"));
/// init_with_file(config);
/// ```
pub fn init_with_file(config: LogConfig) {
    let filter = config.build_filter();

    // Helper to create span events (FmtSpan doesn't implement Copy)
    let span_events = || {
        if config.span_events {
            FmtSpan::CLOSE
        } else {
            FmtSpan::NONE
        }
    };

    // Create file appender if path is configured
    let file_appender = config.file_path.as_ref().map(|path| {
        let parent = path.parent().unwrap_or(std::path::Path::new("."));
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("nxv.log");

        RollingFileAppender::new(config.rotation.into(), parent, file_name)
    });

    // Build layers based on format
    let result = if let Some(appender) = file_appender {
        // Dual output: stderr + file
        match config.format {
            LogFormat::Json => {
                let stderr_layer = fmt::layer()
                    .json()
                    .with_span_events(span_events())
                    .with_target(config.show_target)
                    .with_writer(std::io::stderr);

                let file_layer = fmt::layer()
                    .json()
                    .with_span_events(span_events())
                    .with_target(config.show_target)
                    .with_ansi(false)
                    .with_writer(appender);

                tracing_subscriber::registry()
                    .with(filter)
                    .with(stderr_layer)
                    .with(file_layer)
                    .try_init()
            }
            LogFormat::Compact => {
                let stderr_layer = fmt::layer()
                    .compact()
                    .with_span_events(span_events())
                    .with_target(config.show_target)
                    .with_thread_ids(config.show_thread_ids)
                    .with_line_number(config.show_line_numbers)
                    .with_writer(std::io::stderr);

                let file_layer = fmt::layer()
                    .compact()
                    .with_span_events(span_events())
                    .with_target(config.show_target)
                    .with_thread_ids(config.show_thread_ids)
                    .with_line_number(config.show_line_numbers)
                    .with_ansi(false)
                    .with_writer(appender);

                tracing_subscriber::registry()
                    .with(filter)
                    .with(stderr_layer)
                    .with(file_layer)
                    .try_init()
            }
            LogFormat::Pretty => {
                let stderr_layer = fmt::layer()
                    .with_span_events(span_events())
                    .with_target(config.show_target)
                    .with_thread_ids(config.show_thread_ids)
                    .with_line_number(config.show_line_numbers)
                    .with_writer(std::io::stderr);

                let file_layer = fmt::layer()
                    .with_span_events(span_events())
                    .with_target(config.show_target)
                    .with_thread_ids(config.show_thread_ids)
                    .with_line_number(config.show_line_numbers)
                    .with_ansi(false)
                    .with_writer(appender);

                tracing_subscriber::registry()
                    .with(filter)
                    .with(stderr_layer)
                    .with(file_layer)
                    .try_init()
            }
        }
    } else {
        // Fallback to stderr only
        init(config);
        return;
    };

    // Silently ignore if already initialized
    let _ = result;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_format_from_str() {
        assert_eq!("pretty".parse::<LogFormat>().unwrap(), LogFormat::Pretty);
        assert_eq!("full".parse::<LogFormat>().unwrap(), LogFormat::Pretty);
        assert_eq!("compact".parse::<LogFormat>().unwrap(), LogFormat::Compact);
        assert_eq!("json".parse::<LogFormat>().unwrap(), LogFormat::Json);
        assert_eq!("JSON".parse::<LogFormat>().unwrap(), LogFormat::Json);
        assert!("invalid".parse::<LogFormat>().is_err());
    }

    #[test]
    fn test_log_rotation_from_str() {
        assert_eq!(
            "hourly".parse::<LogRotation>().unwrap(),
            LogRotation::Hourly
        );
        assert_eq!("daily".parse::<LogRotation>().unwrap(), LogRotation::Daily);
        assert_eq!("never".parse::<LogRotation>().unwrap(), LogRotation::Never);
        assert!("invalid".parse::<LogRotation>().is_err());
    }

    #[test]
    fn test_parse_level() {
        assert_eq!(parse_level("error"), Some(Level::ERROR));
        assert_eq!(parse_level("warn"), Some(Level::WARN));
        assert_eq!(parse_level("warning"), Some(Level::WARN));
        assert_eq!(parse_level("info"), Some(Level::INFO));
        assert_eq!(parse_level("debug"), Some(Level::DEBUG));
        assert_eq!(parse_level("trace"), Some(Level::TRACE));
        assert_eq!(parse_level("INFO"), Some(Level::INFO));
        assert_eq!(parse_level("invalid"), None);
    }

    #[test]
    fn test_log_config_defaults() {
        let config = LogConfig::default();
        assert_eq!(config.level, Level::INFO);
        assert_eq!(config.format, LogFormat::Pretty);
        assert!(config.file_path.is_none());
        assert!(!config.span_events);
        assert!(config.show_target);
    }

    #[test]
    fn test_log_config_for_indexer() {
        let config = LogConfig::for_indexer();
        // Span events disabled by default to keep logs clean
        assert!(!config.span_events);
    }

    #[test]
    fn test_log_config_builder() {
        let config = LogConfig::new()
            .with_level(Level::DEBUG)
            .with_format(LogFormat::Json)
            .with_span_events(true)
            .with_file(PathBuf::from("/tmp/test.log"));

        assert_eq!(config.level, Level::DEBUG);
        assert_eq!(config.format, LogFormat::Json);
        assert!(config.span_events);
        assert_eq!(config.file_path, Some(PathBuf::from("/tmp/test.log")));
    }
}
