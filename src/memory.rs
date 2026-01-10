//! Memory size parsing and formatting utilities.
//!
//! Provides human-readable memory size parsing (e.g., "32G", "1024M") and
//! automatic division among worker processes.

use std::fmt;
use std::str::FromStr;

/// Memory size in bytes with parsing and formatting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemorySize(u64);

impl MemorySize {
    /// Create from raw bytes.
    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Create from mebibytes (MiB).
    pub const fn from_mib(mib: u64) -> Self {
        Self(mib * 1024 * 1024)
    }

    /// Create from gibibytes (GiB).
    pub const fn from_gib(gib: u64) -> Self {
        Self(gib * 1024 * 1024 * 1024)
    }

    /// Get the raw byte count.
    pub const fn as_bytes(&self) -> u64 {
        self.0
    }

    /// Get the size in mebibytes (MiB).
    pub const fn as_mib(&self) -> u64 {
        self.0 / (1024 * 1024)
    }

    /// Get the size in gibibytes (GiB), truncated.
    pub const fn as_gib(&self) -> u64 {
        self.0 / (1024 * 1024 * 1024)
    }

    /// Divide memory among workers, enforcing a minimum per-worker threshold.
    ///
    /// Returns the per-worker allocation if it meets the minimum, otherwise an error.
    pub fn divide_among(&self, workers: usize, min_per_worker: Self) -> Result<Self, MemoryError> {
        if workers == 0 {
            return Err(MemoryError::InvalidWorkerCount);
        }

        let per_worker = Self(self.0 / workers as u64);

        if per_worker < min_per_worker {
            return Err(MemoryError::InsufficientBudget {
                total: *self,
                workers,
                per_worker,
                minimum: min_per_worker,
            });
        }

        Ok(per_worker)
    }
}

impl Default for MemorySize {
    fn default() -> Self {
        DEFAULT_MEMORY_BUDGET
    }
}

/// Errors that can occur when parsing or allocating memory.
#[derive(Debug, Clone)]
pub enum MemoryError {
    /// Invalid format in memory size string.
    InvalidFormat(String),
    /// Worker count cannot be zero.
    InvalidWorkerCount,
    /// Memory budget is too small for the number of workers.
    InsufficientBudget {
        total: MemorySize,
        workers: usize,
        per_worker: MemorySize,
        minimum: MemorySize,
    },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat(msg) => write!(f, "invalid memory size: {}", msg),
            Self::InvalidWorkerCount => write!(f, "worker count cannot be zero"),
            Self::InsufficientBudget {
                total,
                workers,
                per_worker,
                minimum,
            } => write!(
                f,
                "insufficient memory: {} / {} workers = {} per worker (minimum: {})",
                total, workers, per_worker, minimum
            ),
        }
    }
}

impl std::error::Error for MemoryError {}

impl FromStr for MemorySize {
    type Err = MemoryError;

    /// Parse a human-readable memory size string.
    ///
    /// Supported formats:
    /// - Plain number: treated as MiB for backwards compatibility (e.g., "6144" = 6 GiB)
    /// - With suffix: "32G", "32GB", "32GiB", "1024M", "1024MB", "1024MiB"
    /// - Case insensitive
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(MemoryError::InvalidFormat("empty string".into()));
        }

        // Find where the numeric part ends
        let num_end = s
            .chars()
            .position(|c| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len());

        if num_end == 0 {
            return Err(MemoryError::InvalidFormat(format!(
                "no numeric value in '{}'",
                s
            )));
        }

        let num_str = &s[..num_end];
        let suffix = s[num_end..].trim().to_lowercase();

        // Parse the numeric value
        let value: f64 = num_str
            .parse()
            .map_err(|_| MemoryError::InvalidFormat(format!("invalid number: '{}'", num_str)))?;

        if value < 0.0 {
            return Err(MemoryError::InvalidFormat("negative value".into()));
        }

        // Determine multiplier based on suffix
        let multiplier: u64 = match suffix.as_str() {
            "" => 1024 * 1024,                        // No suffix = MiB (backwards compat)
            "b" => 1,                                 // Bytes
            "k" | "kb" | "kib" => 1024,               // Kibibytes
            "m" | "mb" | "mib" => 1024 * 1024,        // Mebibytes
            "g" | "gb" | "gib" => 1024 * 1024 * 1024, // Gibibytes
            "t" | "tb" | "tib" => 1024_u64 * 1024 * 1024 * 1024, // Tebibytes
            _ => {
                return Err(MemoryError::InvalidFormat(format!(
                    "unknown suffix: '{}'",
                    suffix
                )));
            }
        };

        let bytes = (value * multiplier as f64) as u64;

        Ok(MemorySize(bytes))
    }
}

impl fmt::Display for MemorySize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const GIB: u64 = 1024 * 1024 * 1024;

        if self.0 >= GIB {
            let gib = self.0 as f64 / GIB as f64;
            if (gib.fract()) < 0.01 {
                write!(f, "{} GiB", self.as_gib())
            } else {
                write!(f, "{:.1} GiB", gib)
            }
        } else if self.as_mib() > 0 {
            write!(f, "{} MiB", self.as_mib())
        } else {
            write!(f, "{} bytes", self.0)
        }
    }
}

/// Minimum per-worker memory threshold (512 MiB).
///
/// Workers below this threshold may OOM during Nix evaluation.
pub const MIN_WORKER_MEMORY: MemorySize = MemorySize::from_mib(512);

/// Default per-worker memory allocation (6 GiB).
///
/// This is the target per-worker memory when dividing the total budget.
/// Change this value to adjust the default memory per worker.
pub const DEFAULT_PER_WORKER_MEMORY: MemorySize = MemorySize::from_gib(6);

/// Default number of workers (one per system architecture).
pub const DEFAULT_WORKER_COUNT: usize = 4;

/// Default total memory budget.
///
/// Calculated as DEFAULT_PER_WORKER_MEMORY × DEFAULT_WORKER_COUNT.
/// With defaults: 6 GiB × 4 = 24 GiB.
pub const DEFAULT_MEMORY_BUDGET: MemorySize =
    MemorySize::from_bytes(DEFAULT_PER_WORKER_MEMORY.as_bytes() * DEFAULT_WORKER_COUNT as u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plain_number_as_mib() {
        // Backwards compatibility: plain numbers are MiB
        assert_eq!(MemorySize::from_str("6144").unwrap().as_mib(), 6144);
        assert_eq!(MemorySize::from_str("1024").unwrap().as_gib(), 1);
    }

    #[test]
    fn test_parse_with_suffix() {
        assert_eq!(MemorySize::from_str("32G").unwrap().as_gib(), 32);
        assert_eq!(MemorySize::from_str("32GB").unwrap().as_gib(), 32);
        assert_eq!(MemorySize::from_str("32GiB").unwrap().as_gib(), 32);
        assert_eq!(MemorySize::from_str("32gb").unwrap().as_gib(), 32);
        assert_eq!(MemorySize::from_str("1024M").unwrap().as_mib(), 1024);
        assert_eq!(MemorySize::from_str("1024MiB").unwrap().as_mib(), 1024);
        assert_eq!(MemorySize::from_str("1T").unwrap().as_gib(), 1024);
    }

    #[test]
    fn test_parse_fractional() {
        assert_eq!(MemorySize::from_str("1.5G").unwrap().as_mib(), 1536);
        assert_eq!(MemorySize::from_str("0.5G").unwrap().as_mib(), 512);
    }

    #[test]
    fn test_parse_errors() {
        assert!(MemorySize::from_str("").is_err());
        assert!(MemorySize::from_str("abc").is_err());
        assert!(MemorySize::from_str("32X").is_err());
        assert!(MemorySize::from_str("-5G").is_err());
    }

    #[test]
    fn test_divide_among_workers() {
        let budget = MemorySize::from_gib(32);
        let min = MemorySize::from_mib(512);

        // 32 GiB / 4 workers = 8 GiB each
        let per_worker = budget.divide_among(4, min).unwrap();
        assert_eq!(per_worker.as_gib(), 8);

        // 32 GiB / 16 workers = 2 GiB each
        let per_worker = budget.divide_among(16, min).unwrap();
        assert_eq!(per_worker.as_gib(), 2);

        // 2 GiB / 8 workers = 256 MiB (below 512 MiB minimum)
        let small_budget = MemorySize::from_gib(2);
        assert!(small_budget.divide_among(8, min).is_err());
    }

    #[test]
    fn test_divide_zero_workers() {
        let budget = MemorySize::from_gib(32);
        let min = MemorySize::from_mib(512);
        assert!(matches!(
            budget.divide_among(0, min),
            Err(MemoryError::InvalidWorkerCount)
        ));
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", MemorySize::from_gib(8)), "8 GiB");
        assert_eq!(format!("{}", MemorySize::from_mib(512)), "512 MiB");
        assert_eq!(format!("{}", MemorySize::from_mib(1536)), "1.5 GiB");
    }

    #[test]
    fn test_default() {
        assert_eq!(MemorySize::default().as_gib(), 24);
    }
}
