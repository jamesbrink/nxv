//! Path utilities for nxv data storage.

use std::path::{Path, PathBuf};

/// Default database filename (slim/optimized - one row per attr+version).
pub const INDEX_DB_FILENAME: &str = "index.db";

/// Full database filename (complete history with all version ranges).
#[allow(dead_code)] // Used by download logic when full_history variant is requested
pub const INDEX_FULL_DB_FILENAME: &str = "index-full.db";

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

/// Get the path to the SQLite index database (slim/default).
pub fn get_index_path() -> PathBuf {
    get_data_dir().join(INDEX_DB_FILENAME)
}

/// Get the path to the full SQLite index database (complete history).
#[allow(dead_code)] // Used by download logic when full_history variant is requested
pub fn get_full_index_path() -> PathBuf {
    get_data_dir().join(INDEX_FULL_DB_FILENAME)
}

/// Derives the bloom filter path from the database path.
///
/// The bloom filter is stored as a sibling file to the database with `.bloom` extension.
/// For example, if the database is at `/var/lib/nxv/index.db`, the bloom filter
/// will be at `/var/lib/nxv/index.bloom`.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use nxv::paths::get_bloom_path_for_db;
///
/// let db_path = PathBuf::from("/var/lib/nxv/index.db");
/// let bloom_path = get_bloom_path_for_db(&db_path);
/// assert_eq!(bloom_path, PathBuf::from("/var/lib/nxv/index.bloom"));
///
/// let db_path = PathBuf::from("my-index.db");
/// let bloom_path = get_bloom_path_for_db(&db_path);
/// assert_eq!(bloom_path, PathBuf::from("my-index.bloom"));
/// ```
pub fn get_bloom_path_for_db<P: AsRef<Path>>(db_path: P) -> PathBuf {
    let db_path = db_path.as_ref();
    db_path.with_extension("bloom")
}

/// Expands a leading `~` in a path to the user's home directory.
///
/// This handles the common case where shell tilde expansion doesn't occur,
/// such as when using `--arg=~/path` syntax instead of `--arg ~/path`.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use nxv::paths::expand_tilde;
///
/// // Paths starting with ~ get expanded
/// let path = PathBuf::from("~/Documents");
/// let expanded = expand_tilde(&path);
/// assert!(expanded.to_str().unwrap().contains("Documents"));
/// assert!(!expanded.to_str().unwrap().starts_with("~"));
///
/// // Other paths are unchanged
/// let path = PathBuf::from("/tmp/foo");
/// assert_eq!(expand_tilde(&path), path);
/// ```
#[cfg_attr(not(feature = "indexer"), allow(dead_code))]
pub fn expand_tilde<P: AsRef<Path>>(path: P) -> PathBuf {
    let path = path.as_ref();
    if let Ok(stripped) = path.strip_prefix("~")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    path.to_path_buf()
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
    fn test_get_bloom_path_for_db_derives_from_db_path() {
        let db_path = PathBuf::from("/var/lib/nxv/index.db");
        let bloom_path = get_bloom_path_for_db(&db_path);
        assert_eq!(bloom_path, PathBuf::from("/var/lib/nxv/index.bloom"));
    }

    #[test]
    fn test_get_bloom_path_for_db_handles_different_names() {
        let db_path = PathBuf::from("/tmp/custom.db");
        let bloom_path = get_bloom_path_for_db(&db_path);
        assert_eq!(bloom_path, PathBuf::from("/tmp/custom.bloom"));
    }

    #[test]
    fn test_get_bloom_path_for_db_relative_path() {
        let db_path = PathBuf::from("data/index.db");
        let bloom_path = get_bloom_path_for_db(&db_path);
        assert_eq!(bloom_path, PathBuf::from("data/index.bloom"));
    }

    #[test]
    fn test_expand_tilde_with_home() {
        let path = PathBuf::from("~/Documents/test");
        let expanded = expand_tilde(&path);
        assert!(!expanded.to_str().unwrap().starts_with("~"));
        assert!(expanded.to_str().unwrap().ends_with("Documents/test"));
    }

    #[test]
    fn test_expand_tilde_absolute_path_unchanged() {
        let path = PathBuf::from("/tmp/foo/bar");
        assert_eq!(expand_tilde(&path), path);
    }

    #[test]
    fn test_expand_tilde_relative_path_unchanged() {
        let path = PathBuf::from("./relative/path");
        assert_eq!(expand_tilde(&path), path);
    }

    #[test]
    fn test_expand_tilde_just_tilde() {
        let path = PathBuf::from("~");
        let expanded = expand_tilde(&path);
        // Should expand to home directory
        assert!(!expanded.to_str().unwrap().starts_with("~"));
    }
}
