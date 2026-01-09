//! Buffered IPC primitives for worker communication.
//!
//! Provides efficient line-based I/O over pipes with EINTR handling.

#![allow(dead_code)] // Some methods are for future use or testing

use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

/// Default buffer size for IPC (64KB).
const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

/// A file descriptor wrapper that implements Read/Write with EINTR handling.
pub struct PipeFd {
    fd: OwnedFd,
}

impl PipeFd {
    /// Create from an owned file descriptor.
    pub fn new(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Create from a raw file descriptor (takes ownership).
    ///
    /// # Safety
    /// The caller must ensure `fd` is a valid file descriptor that can be owned.
    pub unsafe fn from_raw(fd: RawFd) -> Self {
        Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        }
    }
}

impl AsFd for PipeFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl AsRawFd for PipeFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Read for PipeFd {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match nix::unistd::read(&self.fd, buf) {
                Ok(n) => return Ok(n),
                Err(nix::errno::Errno::EINTR) => continue, // Retry on interrupt
                // Convert nix::errno::Errno to std::io::Error via raw OS error code
                Err(e) => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        }
    }
}

impl Write for PipeFd {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match nix::unistd::write(&self.fd, buf) {
                Ok(n) => return Ok(n),
                Err(nix::errno::Errno::EINTR) => continue, // Retry on interrupt
                // Convert nix::errno::Errno to std::io::Error via raw OS error code
                Err(e) => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(()) // Pipes don't need flushing at the fd level
    }
}

/// Buffered line reader for IPC.
pub struct LineReader {
    reader: BufReader<PipeFd>,
    line_buffer: String,
}

impl LineReader {
    /// Create a new line reader from a file descriptor.
    pub fn new(fd: PipeFd) -> Self {
        Self {
            reader: BufReader::with_capacity(DEFAULT_BUFFER_SIZE, fd),
            line_buffer: String::with_capacity(4096),
        }
    }

    /// Read a line, returning a reference to the internal buffer.
    /// Returns `None` on EOF.
    pub fn read_line(&mut self) -> io::Result<Option<&str>> {
        self.line_buffer.clear();
        match self.reader.read_line(&mut self.line_buffer) {
            Ok(0) => Ok(None), // EOF
            Ok(_) => {
                // Strip trailing newline
                if self.line_buffer.ends_with('\n') {
                    self.line_buffer.pop();
                }
                if self.line_buffer.ends_with('\r') {
                    self.line_buffer.pop();
                }
                Ok(Some(&self.line_buffer))
            }
            Err(e) => Err(e),
        }
    }

    /// Read a line and return an owned string.
    /// Returns `None` on EOF.
    pub fn read_line_owned(&mut self) -> io::Result<Option<String>> {
        Ok(self.read_line()?.map(String::from))
    }
}

/// Buffered line writer for IPC.
pub struct LineWriter {
    writer: BufWriter<PipeFd>,
}

impl LineWriter {
    /// Create a new line writer from a file descriptor.
    pub fn new(fd: PipeFd) -> Self {
        Self {
            writer: BufWriter::with_capacity(DEFAULT_BUFFER_SIZE, fd),
        }
    }

    /// Write a line (appends newline if not present) and flush.
    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            self.writer.write_all(b"\n")?;
        }
        self.writer.flush()
    }

    /// Write raw bytes and flush.
    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::IntoRawFd;

    fn create_pipe() -> (PipeFd, PipeFd) {
        let (read_fd, write_fd) = nix::unistd::pipe().expect("Failed to create pipe");
        unsafe {
            (
                PipeFd::from_raw(read_fd.into_raw_fd()),
                PipeFd::from_raw(write_fd.into_raw_fd()),
            )
        }
    }

    #[test]
    fn test_line_reader_writer_roundtrip() {
        let (read_fd, write_fd) = create_pipe();
        let mut reader = LineReader::new(read_fd);
        let mut writer = LineWriter::new(write_fd);

        // Write some lines
        writer.write_line("hello").unwrap();
        writer.write_line("world\n").unwrap(); // Already has newline
        writer.write_line("").unwrap(); // Empty line
        drop(writer); // Close write end to signal EOF

        // Read them back
        assert_eq!(reader.read_line().unwrap(), Some("hello"));
        assert_eq!(reader.read_line().unwrap(), Some("world"));
        assert_eq!(reader.read_line().unwrap(), Some(""));
        assert_eq!(reader.read_line().unwrap(), None); // EOF
    }

    #[test]
    fn test_line_reader_owned() {
        let (read_fd, write_fd) = create_pipe();
        let mut reader = LineReader::new(read_fd);
        let mut writer = LineWriter::new(write_fd);

        writer.write_line("test line").unwrap();
        drop(writer);

        let line = reader.read_line_owned().unwrap();
        assert_eq!(line, Some("test line".to_string()));
    }

    #[test]
    fn test_crlf_handling() {
        let (read_fd, write_fd) = create_pipe();
        let mut reader = LineReader::new(read_fd);
        let mut writer = LineWriter::new(write_fd);

        // Write with CRLF (Windows-style)
        writer.write_all(b"line1\r\n").unwrap();
        writer.write_all(b"line2\n").unwrap();
        drop(writer);

        assert_eq!(reader.read_line().unwrap(), Some("line1"));
        assert_eq!(reader.read_line().unwrap(), Some("line2"));
    }
}
