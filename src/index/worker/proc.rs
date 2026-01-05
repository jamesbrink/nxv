//! Process handle for worker subprocesses.
//!
//! Wraps a child process with IPC channels for communication.

#![allow(dead_code)] // Some methods are for debugging/monitoring

use super::ipc::{LineReader, LineWriter, PipeFd};
use super::protocol::{WorkRequest, WorkResponse};
use crate::error::{NxvError, Result};
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use std::time::Duration;

/// Handle to a worker subprocess with IPC channels.
pub struct Proc {
    /// Process ID
    pid: Pid,
    /// Writer for sending requests to the worker
    writer: LineWriter,
    /// Reader for receiving responses from the worker
    reader: LineReader,
    /// Whether the process has been reaped
    reaped: bool,
}

impl Proc {
    /// Create a new process handle from its components.
    ///
    /// # Arguments
    /// * `pid` - The process ID
    /// * `stdin` - File descriptor for writing to the worker's stdin
    /// * `stdout` - File descriptor for reading from the worker's stdout
    pub fn new(pid: Pid, stdin: PipeFd, stdout: PipeFd) -> Self {
        Self {
            pid,
            writer: LineWriter::new(stdin),
            reader: LineReader::new(stdout),
            reaped: false,
        }
    }

    /// Create from a spawned `std::process::Child`.
    ///
    /// Takes ownership of the child's stdin and stdout.
    pub fn from_child(mut child: std::process::Child) -> Result<Self> {
        let pid = Pid::from_raw(child.id() as i32);

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| NxvError::Worker("Child stdin not captured".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| NxvError::Worker("Child stdout not captured".into()))?;

        // Convert to raw fds and wrap in PipeFd
        use std::os::unix::io::AsRawFd;
        let stdin_fd = unsafe { PipeFd::from_raw(stdin.as_raw_fd()) };
        let stdout_fd = unsafe { PipeFd::from_raw(stdout.as_raw_fd()) };

        // Prevent the original handles from closing the fds
        std::mem::forget(stdin);
        std::mem::forget(stdout);

        Ok(Self::new(pid, stdin_fd, stdout_fd))
    }

    /// Get the process ID.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Send a request to the worker.
    pub fn send(&mut self, request: &WorkRequest) -> Result<()> {
        let line = request.to_line();
        self.writer
            .write_line(&line)
            .map_err(|e| NxvError::Worker(format!("Failed to send to worker: {}", e)))
    }

    /// Receive a response from the worker.
    ///
    /// Returns `None` if the worker closed its stdout (EOF).
    pub fn recv(&mut self) -> Result<Option<WorkResponse>> {
        match self.reader.read_line() {
            Ok(Some(line)) => {
                let response = WorkResponse::from_line(line)
                    .map_err(|e| NxvError::Worker(format!("Invalid worker response: {}", e)))?;
                Ok(Some(response))
            }
            Ok(None) => Ok(None), // EOF
            Err(e) => Err(NxvError::Worker(format!(
                "Failed to receive from worker: {}",
                e
            ))),
        }
    }

    /// Check if the process is still running (non-blocking).
    pub fn is_running(&mut self) -> bool {
        if self.reaped {
            return false;
        }
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => true,
            Ok(_) => {
                self.reaped = true;
                false
            }
            Err(_) => {
                self.reaped = true;
                false
            }
        }
    }

    /// Wait for the process to exit (blocking).
    ///
    /// Returns the wait status.
    pub fn wait(&mut self) -> Result<WaitStatus> {
        if self.reaped {
            return Err(NxvError::Worker("Process already reaped".into()));
        }
        match waitpid(self.pid, None) {
            Ok(status) => {
                self.reaped = true;
                Ok(status)
            }
            Err(e) => Err(NxvError::Worker(format!("waitpid failed: {}", e))),
        }
    }

    /// Try to wait for the process (non-blocking).
    ///
    /// Returns `None` if the process is still running.
    pub fn try_wait(&mut self) -> Result<Option<WaitStatus>> {
        if self.reaped {
            return Err(NxvError::Worker("Process already reaped".into()));
        }
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => Ok(None),
            Ok(status) => {
                self.reaped = true;
                Ok(Some(status))
            }
            Err(e) => Err(NxvError::Worker(format!("waitpid failed: {}", e))),
        }
    }

    /// Send SIGTERM to the process.
    pub fn terminate(&self) -> Result<()> {
        if self.reaped {
            return Ok(());
        }
        signal::kill(self.pid, Signal::SIGTERM)
            .map_err(|e| NxvError::Worker(format!("Failed to send SIGTERM: {}", e)))
    }

    /// Send SIGKILL to the process and wait for it to exit.
    pub fn kill(&mut self) -> Result<()> {
        if self.reaped {
            return Ok(());
        }
        signal::kill(self.pid, Signal::SIGKILL)
            .map_err(|e| NxvError::Worker(format!("Failed to send SIGKILL: {}", e)))?;
        self.wait()?;
        Ok(())
    }

    /// Gracefully stop the worker: send exit command, wait briefly, then kill if needed.
    pub fn stop(&mut self, timeout: Duration) -> Result<()> {
        if self.reaped {
            return Ok(());
        }

        // Try to send exit command
        let _ = self.send(&WorkRequest::Exit);

        // Wait with timeout
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Ok(Some(_)) = self.try_wait() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // Timeout: send SIGTERM
        let _ = self.terminate();

        // Brief wait for SIGTERM
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(Some(_)) = self.try_wait() {
            return Ok(());
        }

        // Still running: SIGKILL
        self.kill()
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if !self.reaped {
            // Try graceful termination
            let _ = self.terminate();
            std::thread::sleep(Duration::from_millis(10));

            // Check if it exited
            if let Ok(Some(_)) = self.try_wait() {
                return;
            }

            // Force kill
            let _ = signal::kill(self.pid, Signal::SIGKILL);
            let _ = waitpid(self.pid, None);
            self.reaped = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn test_proc_from_child() {
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to spawn cat");

        let mut proc = Proc::from_child(child).expect("Failed to create Proc");
        assert!(proc.is_running());

        // Stop the process
        proc.stop(Duration::from_secs(1)).expect("Failed to stop");
        assert!(!proc.is_running());
    }

    #[test]
    fn test_proc_terminate() {
        let child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Failed to spawn sleep");

        let mut proc = Proc::from_child(child).expect("Failed to create Proc");
        assert!(proc.is_running());

        proc.terminate().expect("Failed to terminate");
        std::thread::sleep(Duration::from_millis(100));

        // Process should have exited
        assert!(!proc.is_running());
    }
}
