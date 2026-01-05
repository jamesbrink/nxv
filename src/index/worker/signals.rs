//! Crash analysis for worker subprocesses.
//!
//! Analyzes process termination reasons to determine if failures are recoverable.

#![allow(dead_code)] // Some methods are for future diagnostics

use nix::sys::signal::Signal;
use nix::sys::wait::WaitStatus;

/// Reason why a worker process terminated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminationReason {
    /// Normal exit with status code.
    Exited(i32),
    /// Killed by signal.
    Signaled(Signal),
    /// Likely out of memory (SIGKILL from OOM killer).
    OutOfMemory,
    /// Stack overflow (SIGSEGV on Linux).
    StackOverflow,
    /// Stack overflow (SIGBUS on macOS).
    StackOverflowMacOS,
    /// Process is still running.
    StillAlive,
    /// Unknown termination reason.
    Unknown,
}

impl TerminationReason {
    /// Check if this termination is recoverable (worker can be restarted).
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::OutOfMemory | Self::StackOverflow | Self::StackOverflowMacOS
        )
    }

    /// Check if this is a successful exit.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Exited(0))
    }

    /// Get a human-readable description.
    pub fn description(&self) -> String {
        match self {
            Self::Exited(code) => format!("exited with code {}", code),
            Self::Signaled(sig) => format!("killed by signal {:?}", sig),
            Self::OutOfMemory => "out of memory (SIGKILL from OOM killer)".to_string(),
            Self::StackOverflow => "stack overflow (SIGSEGV)".to_string(),
            Self::StackOverflowMacOS => "stack overflow (SIGBUS)".to_string(),
            Self::StillAlive => "still running".to_string(),
            Self::Unknown => "unknown reason".to_string(),
        }
    }
}

impl std::fmt::Display for TerminationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// Analyze a `WaitStatus` to determine the termination reason.
pub fn analyze_wait_status(status: WaitStatus) -> TerminationReason {
    match status {
        WaitStatus::Exited(_, code) => TerminationReason::Exited(code),

        WaitStatus::Signaled(_, signal, _) => {
            match signal {
                // SIGKILL (9) - likely OOM killer
                Signal::SIGKILL => TerminationReason::OutOfMemory,

                // SIGSEGV (11) - stack overflow on Linux
                Signal::SIGSEGV => TerminationReason::StackOverflow,

                // SIGBUS (10) - stack overflow on macOS
                Signal::SIGBUS => TerminationReason::StackOverflowMacOS,

                // Other signals
                _ => TerminationReason::Signaled(signal),
            }
        }

        WaitStatus::StillAlive => TerminationReason::StillAlive,

        _ => TerminationReason::Unknown,
    }
}

/// Information about a worker failure.
#[derive(Debug)]
pub struct WorkerFailure {
    /// Why the worker terminated.
    pub reason: TerminationReason,
    /// What the worker was doing when it failed.
    pub context: Option<String>,
    /// Additional error message.
    pub message: Option<String>,
}

impl WorkerFailure {
    /// Create a new worker failure.
    pub fn new(reason: TerminationReason) -> Self {
        Self {
            reason,
            context: None,
            message: None,
        }
    }

    /// Add context about what the worker was doing.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Add an error message.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Check if this failure is recoverable.
    pub fn is_recoverable(&self) -> bool {
        self.reason.is_recoverable()
    }
}

impl std::fmt::Display for WorkerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Worker {}", self.reason)?;
        if let Some(ctx) = &self.context {
            write!(f, " while {}", ctx)?;
        }
        if let Some(msg) = &self.message {
            write!(f, ": {}", msg)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::unistd::Pid;

    #[test]
    fn test_termination_reason_recoverable() {
        assert!(TerminationReason::OutOfMemory.is_recoverable());
        assert!(TerminationReason::StackOverflow.is_recoverable());
        assert!(TerminationReason::StackOverflowMacOS.is_recoverable());

        assert!(!TerminationReason::Exited(0).is_recoverable());
        assert!(!TerminationReason::Exited(1).is_recoverable());
        assert!(!TerminationReason::Signaled(Signal::SIGTERM).is_recoverable());
    }

    #[test]
    fn test_termination_reason_success() {
        assert!(TerminationReason::Exited(0).is_success());
        assert!(!TerminationReason::Exited(1).is_success());
        assert!(!TerminationReason::OutOfMemory.is_success());
    }

    #[test]
    fn test_analyze_wait_status() {
        // Normal exit
        let status = WaitStatus::Exited(Pid::from_raw(1), 0);
        assert_eq!(analyze_wait_status(status), TerminationReason::Exited(0));

        // Still alive
        let status = WaitStatus::StillAlive;
        assert_eq!(analyze_wait_status(status), TerminationReason::StillAlive);
    }

    #[test]
    fn test_worker_failure_display() {
        let failure = WorkerFailure::new(TerminationReason::OutOfMemory)
            .with_context("evaluating python3")
            .with_message("worker exceeded memory limit");

        let display = failure.to_string();
        assert!(display.contains("out of memory"));
        assert!(display.contains("evaluating python3"));
        assert!(display.contains("exceeded memory limit"));
    }
}
