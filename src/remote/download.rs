//! HTTP download with progress indication.

use crate::error::{NxvError, Result};
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

/// Maximum number of retry attempts for network operations.
const MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff (in milliseconds).
const BASE_DELAY_MS: u64 = 1000;

/// Connection timeout for HTTP requests.
const CONNECT_TIMEOUT_SECS: u64 = 30;

/// Read timeout for HTTP requests.
const READ_TIMEOUT_SECS: u64 = 300;

/// Retry a network operation with exponential backoff.
///
/// Returns the result of the operation, or the last error if all retries failed.
fn retry_with_backoff<T, E, F>(
    max_retries: u32,
    show_progress: bool,
    mut operation: F,
) -> std::result::Result<T, E>
where
    F: FnMut() -> std::result::Result<T, E>,
    E: std::fmt::Display,
{
    let mut last_error = None;

    for attempt in 0..=max_retries {
        match operation() {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt < max_retries {
                    let delay_ms = BASE_DELAY_MS * 2u64.pow(attempt);
                    if show_progress {
                        eprintln!(
                            "Network error (attempt {}/{}): {}. Retrying in {}s...",
                            attempt + 1,
                            max_retries + 1,
                            e,
                            delay_ms / 1000
                        );
                    }
                    std::thread::sleep(Duration::from_millis(delay_ms));
                }
                last_error = Some(e);
            }
        }
    }

    Err(last_error.expect("at least one attempt should have been made"))
}

/// Download a file from a URL to a destination path.
///
/// Verifies the SHA256 checksum after download.
/// If the URL ends in `.zst`, the file is decompressed automatically.
/// Retries failed requests with exponential backoff.
/// Supports resuming partial downloads if a temp file exists.
pub fn download_file<P: AsRef<Path>>(
    url: &str,
    dest: P,
    expected_sha256: &str,
    show_progress: bool,
) -> Result<()> {
    let dest = dest.as_ref();

    // Ensure parent directory exists
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Download to a temporary file first
    let temp_path = dest.with_extension("tmp");

    // Create HTTP client with timeouts
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(READ_TIMEOUT_SECS))
        .build()?;

    // Check if we can resume a partial download
    let resume_offset = if temp_path.exists() {
        match std::fs::metadata(&temp_path) {
            Ok(meta) => meta.len(),
            Err(_) => 0,
        }
    } else {
        0
    };

    // Retry loop with exponential backoff, with Range header for resume
    let response = retry_with_backoff(MAX_RETRIES, show_progress, || {
        let mut request = client.get(url);
        if resume_offset > 0 {
            request = request.header("Range", format!("bytes={}-", resume_offset));
        }
        request.send()
    })?;

    // Check if the server supports resuming
    let status = response.status();
    let is_partial = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let is_full = status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT;

    if !is_partial && !is_full {
        return Err(NxvError::NetworkMessage(format!(
            "HTTP {} for {}",
            status, url
        )));
    }

    // If server doesn't support range requests, start fresh
    let actual_offset = if is_partial { resume_offset } else { 0 };

    // Get total size (from Content-Range header for partial, Content-Length for full)
    let total_size = if is_partial {
        // Parse Content-Range: bytes 1000-9999/10000
        response
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split('/').next_back())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(actual_offset + response.content_length().unwrap_or(0))
    } else {
        response.content_length().unwrap_or(0)
    };

    // Set up progress bar
    let progress = if show_progress && total_size > 0 {
        let pb = ProgressBar::new(total_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{prefix:.bold} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("█▓▒░  "),
        );
        if actual_offset > 0 {
            pb.set_prefix("Resuming");
            pb.set_position(actual_offset);
        } else {
            pb.set_prefix("Downloading");
        }
        Some(pb)
    } else {
        None
    };

    // Open temp file (append if resuming, create if not)
    let mut temp_file = if actual_offset > 0 {
        let file = OpenOptions::new().append(true).open(&temp_path)?;
        BufWriter::new(file)
    } else {
        BufWriter::new(File::create(&temp_path)?)
    };

    // Download with progress tracking
    let mut downloaded: u64 = actual_offset;

    let mut reader = BufReader::new(response);
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        temp_file.write_all(&buffer[..bytes_read])?;
        downloaded += bytes_read as u64;

        if let Some(ref pb) = progress {
            pb.set_position(downloaded);
        }
    }

    temp_file.flush()?;
    drop(temp_file);

    if let Some(ref pb) = progress {
        pb.finish_with_message("done");
    }

    // Verify SHA256 of the complete file
    let actual_sha256 = file_sha256(&temp_path)?;
    if actual_sha256 != expected_sha256 {
        // Clean up the temp file
        let _ = std::fs::remove_file(&temp_path);
        return Err(NxvError::ChecksumMismatch {
            expected: expected_sha256.to_string(),
            actual: actual_sha256,
        });
    }

    // If URL ends with .zst, decompress
    if url.ends_with(".zst") {
        decompress_zstd(&temp_path, dest, show_progress)?;
        let _ = std::fs::remove_file(&temp_path);
    } else {
        // Move temp file to destination
        std::fs::rename(&temp_path, dest)?;
    }

    Ok(())
}

/// Decompress a zstd-compressed file.
pub fn decompress_zstd<P: AsRef<Path>, Q: AsRef<Path>>(
    src: P,
    dest: Q,
    show_progress: bool,
) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();

    let input = File::open(src)?;
    let input_size = input.metadata()?.len();

    let progress = if show_progress {
        let pb = ProgressBar::new(input_size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{prefix:.bold} [{bar:40.green/cyan}] {bytes}/{total_bytes}")
                .unwrap()
                .progress_chars("█▓▒░  "),
        );
        pb.set_prefix("Decompressing");
        Some(pb)
    } else {
        None
    };

    let mut decoder = zstd::Decoder::new(BufReader::new(input))?;
    let mut output = BufWriter::new(File::create(dest)?);

    let mut buffer = [0u8; 8192];
    let mut total_read = 0u64;

    loop {
        let bytes_read = decoder.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        output.write_all(&buffer[..bytes_read])?;
        total_read += bytes_read as u64;

        if let Some(ref pb) = progress {
            // Progress is based on input bytes processed (approximate)
            pb.set_position(total_read.min(input_size));
        }
    }

    output.flush()?;

    if let Some(ref pb) = progress {
        pb.finish_with_message("done");
    }

    Ok(())
}

/// Compress a file with zstd.
#[allow(dead_code)]
pub fn compress_zstd<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dest: Q, level: i32) -> Result<()> {
    let src = src.as_ref();
    let dest = dest.as_ref();

    let input = BufReader::new(File::open(src)?);
    let output = BufWriter::new(File::create(dest)?);

    let mut encoder = zstd::Encoder::new(output, level)?;
    std::io::copy(&mut BufReader::new(input), &mut encoder)?;
    encoder.finish()?;

    Ok(())
}

/// Calculate SHA256 hash of a file.
pub fn file_sha256<P: AsRef<Path>>(path: P) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_file_sha256() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let hash = file_sha256(&path).unwrap();
        // SHA256 of "hello world" (no newline)
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_file_sha256_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        std::fs::write(&path, b"").unwrap();

        let hash = file_sha256(&path).unwrap();
        // SHA256 of empty string
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_file_sha256_binary_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("binary.bin");
        std::fs::write(&path, [0x00, 0xFF, 0xAB, 0xCD]).unwrap();

        // Should not panic on binary content
        let hash = file_sha256(&path).unwrap();
        assert_eq!(hash.len(), 64); // SHA256 is 64 hex chars
    }

    #[test]
    fn test_file_sha256_large_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.bin");
        // Create a 100KB file
        let content: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &content).unwrap();

        let hash = file_sha256(&path).unwrap();
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_file_sha256_nonexistent_file() {
        let result = file_sha256("/nonexistent/path/file.txt");
        assert!(result.is_err());
    }

    #[test]
    fn test_compress_decompress_zstd() {
        let dir = tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let compressed = dir.path().join("compressed.zst");
        let decompressed = dir.path().join("decompressed.txt");

        let content = "Hello, this is test content for compression!";
        std::fs::write(&original, content).unwrap();

        // Compress
        compress_zstd(&original, &compressed, 3).unwrap();
        assert!(compressed.exists());

        // Decompress
        decompress_zstd(&compressed, &decompressed, false).unwrap();

        // Verify content
        let result = std::fs::read_to_string(&decompressed).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn test_compress_zstd_different_levels() {
        let dir = tempdir().unwrap();
        let original = dir.path().join("original.txt");
        let content = "a".repeat(10_000); // Highly compressible
        std::fs::write(&original, &content).unwrap();

        // Test low compression level
        let low_compressed = dir.path().join("low.zst");
        compress_zstd(&original, &low_compressed, 1).unwrap();

        // Test high compression level
        let high_compressed = dir.path().join("high.zst");
        compress_zstd(&original, &high_compressed, 19).unwrap();

        // Higher level should produce smaller (or equal) file
        let low_size = std::fs::metadata(&low_compressed).unwrap().len();
        let high_size = std::fs::metadata(&high_compressed).unwrap().len();
        assert!(high_size <= low_size);
    }

    #[test]
    fn test_compress_zstd_empty_file() {
        let dir = tempdir().unwrap();
        let original = dir.path().join("empty.txt");
        let compressed = dir.path().join("empty.zst");
        let decompressed = dir.path().join("decompressed.txt");

        std::fs::write(&original, b"").unwrap();
        compress_zstd(&original, &compressed, 3).unwrap();
        decompress_zstd(&compressed, &decompressed, false).unwrap();

        let result = std::fs::read(&decompressed).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_decompress_zstd_invalid_data() {
        let dir = tempdir().unwrap();
        let invalid = dir.path().join("invalid.zst");
        let dest = dir.path().join("output.txt");

        // Write invalid zstd data
        std::fs::write(&invalid, b"this is not zstd data").unwrap();

        let result = decompress_zstd(&invalid, &dest, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_retry_with_backoff_succeeds_first_try() {
        let result: std::result::Result<i32, &str> = retry_with_backoff(3, false, || Ok(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_retry_with_backoff_succeeds_after_retry() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);

        let result: std::result::Result<i32, &str> = retry_with_backoff(3, false, || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                Err("transient error")
            } else {
                Ok(42)
            }
        });

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // 2 failures + 1 success
    }

    #[test]
    fn test_retry_with_backoff_fails_after_max_retries() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);

        let result: std::result::Result<i32, &str> = retry_with_backoff(2, false, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err("persistent error")
        });

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "persistent error");
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // 3 attempts (0, 1, 2 retries)
    }

    #[test]
    fn test_retry_with_backoff_zero_retries() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);

        let result: std::result::Result<i32, &str> = retry_with_backoff(0, false, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err("error")
        });

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1); // Only 1 attempt with 0 retries
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::tempdir;

    proptest! {
        /// Compression and decompression should be lossless for any data.
        #[test]
        fn compress_decompress_roundtrip(content in prop::collection::vec(any::<u8>(), 0..1000)) {
            let dir = tempdir().unwrap();
            let original = dir.path().join("original.bin");
            let compressed = dir.path().join("compressed.zst");
            let decompressed = dir.path().join("decompressed.bin");

            std::fs::write(&original, &content).unwrap();
            compress_zstd(&original, &compressed, 3).unwrap();
            decompress_zstd(&compressed, &decompressed, false).unwrap();

            let result = std::fs::read(&decompressed).unwrap();
            prop_assert_eq!(result, content);
        }

        /// SHA256 should always produce a 64-character hex string.
        #[test]
        fn sha256_always_64_chars(content in prop::collection::vec(any::<u8>(), 0..500)) {
            let dir = tempdir().unwrap();
            let path = dir.path().join("test.bin");
            std::fs::write(&path, &content).unwrap();

            let hash = file_sha256(&path).unwrap();
            prop_assert_eq!(hash.len(), 64);
            prop_assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        }

        /// Same content should always produce the same hash.
        #[test]
        fn sha256_deterministic(content in prop::collection::vec(any::<u8>(), 0..500)) {
            let dir = tempdir().unwrap();
            let path1 = dir.path().join("file1.bin");
            let path2 = dir.path().join("file2.bin");

            std::fs::write(&path1, &content).unwrap();
            std::fs::write(&path2, &content).unwrap();

            let hash1 = file_sha256(&path1).unwrap();
            let hash2 = file_sha256(&path2).unwrap();
            prop_assert_eq!(hash1, hash2);
        }
    }
}
