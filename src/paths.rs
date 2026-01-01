//! Path utilities for nxv data storage.

use std::path::PathBuf;

/// Get the data directory for nxv.
///
/// Uses XDG base directory specification on Linux/macOS:
/// - Linux: `~/.local/share/nxv`
/// - macOS: `~/Library/Application Support/nxv`
/// - Windows: `%APPDATA%\nxv`
pub fn get_data_dir() -> PathBuf {
    dirs::data_dir().map(|d| d.join("nxv")).unwrap_or_else(|| {
        // Fallback to current directory if data_dir is not available
        PathBuf::from(".nxv")
    })
}

/// Get the path to the SQLite index database.
pub fn get_index_path() -> PathBuf {
    get_data_dir().join("index.db")
}

/// Provides the filesystem path to the nxv bloom filter file.
///
/// If the `NXV_BLOOM_PATH` environment variable is set, its value is used as the path.
/// Otherwise the file is located at `<data_dir>/index.bloom`, where `data_dir` is returned by `get_data_dir()`.
///
/// # Examples
///
/// ```
/// std::env::remove_var("NXV_BLOOM_PATH");
/// let path = crate::paths::get_bloom_path();
/// assert!(path.ends_with("index.bloom"));
///
/// std::env::set_var("NXV_BLOOM_PATH", "/tmp/custom.bloom");
/// let overridden = crate::paths::get_bloom_path();
/// assert_eq!(overridden, std::path::PathBuf::from("/tmp/custom.bloom"));
/// ```
pub fn get_bloom_path() -> PathBuf {
    std::env::var("NXV_BLOOM_PATH")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| get_data_dir().join("index.bloom"))
}

/// Ensures the nxv data directory exists, creating it and any missing parents if necessary.
///
/// This creates the directory returned by `get_data_dir()` when it does not already exist.
/// I/O errors from creating directories are returned to the caller.
///
/// # Examples
///
/// ```
/// // Create the data directory if needed; propagate or assert success in tests.
/// assert!(nxv::paths::ensure_data_dir().is_ok());
/// ```
#[cfg_attr(not(feature = "indexer"), allow(dead_code))]
pub fn ensure_data_dir() -> std::io::Result<()> {
    let data_dir = get_data_dir();
    if !data_dir.exists() {
        std::fs::create_dir_all(&data_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_data_dir_returns_valid_path() {
        let data_dir = get_data_dir();
        // Should end with "nxv" or ".nxv"
        let name = data_dir.file_name().unwrap().to_str().unwrap();
        assert!(name == "nxv" || name == ".nxv");
    }

    #[test]
    fn test_get_index_path_is_in_data_dir() {
        let data_dir = get_data_dir();
        let index_path = get_index_path();
        assert!(index_path.starts_with(&data_dir));
        assert_eq!(
            index_path.file_name().unwrap().to_str().unwrap(),
            "index.db"
        );
    }

    #[test]
    fn test_get_bloom_path_is_in_data_dir() {
        let data_dir = get_data_dir();
        let bloom_path = get_bloom_path();
        assert!(bloom_path.starts_with(&data_dir));
        assert_eq!(
            bloom_path.file_name().unwrap().to_str().unwrap(),
            "index.bloom"
        );
    }
}