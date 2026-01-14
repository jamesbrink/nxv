//! Blob hash cache for static file maps.
//!
//! Caches `StaticFileMap` results keyed by blob SHA to avoid re-parsing the same
//! `all-packages.nix` content across different commits. Since many commits share
//! the same blob (no changes to all-packages.nix), this dramatically reduces
//! the number of parses needed (~500K commits → ~30K unique blobs).

use crate::error::{NxvError, Result};
use crate::index::static_analysis::StaticFileMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Helper to create an indexer config error.
fn cache_error(msg: impl Into<String>) -> NxvError {
    NxvError::Config(msg.into())
}

/// Cache version - increment when cache format changes.
const CACHE_VERSION: u32 = 1;

/// Blob cache for static file maps.
///
/// Stores parsed `StaticFileMap` results keyed by git blob OID (SHA).
/// This allows reusing parse results across commits that share the same
/// blob content for `all-packages.nix`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BlobCache {
    /// Cache version for compatibility checking.
    version: u32,

    /// Blob OID (hex string) -> StaticFileMap
    entries: HashMap<String, StaticFileMap>,

    /// Number of cache hits during this session.
    #[serde(skip)]
    hits: usize,

    /// Number of cache misses during this session.
    #[serde(skip)]
    misses: usize,

    /// Path to the cache file.
    #[serde(skip)]
    cache_path: Option<PathBuf>,
}

impl Default for BlobCache {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            entries: HashMap::new(),
            hits: 0,
            misses: 0,
            cache_path: None,
        }
    }
}

#[allow(dead_code)]
impl BlobCache {
    /// Create a new empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new cache with a persistence path.
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            cache_path: Some(path.into()),
            ..Self::default()
        }
    }

    /// Load cache from disk, or create a new one if it doesn't exist.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            debug!(?path, "Cache file doesn't exist, creating new cache");
            return Ok(Self::with_path(path));
        }

        match Self::load(path) {
            Ok(cache) => {
                info!(
                    ?path,
                    entries = cache.entries.len(),
                    "Loaded blob cache from disk"
                );
                Ok(cache)
            }
            Err(e) => {
                warn!(?path, error = %e, "Failed to load cache, renaming corrupt file");
                // Rename corrupt file to preserve evidence
                let corrupt_path = path.with_extension("corrupt");
                if let Err(rename_err) = std::fs::rename(path, &corrupt_path) {
                    warn!(
                        ?corrupt_path,
                        error = %rename_err,
                        "Failed to rename corrupt cache file"
                    );
                } else {
                    info!(?corrupt_path, "Renamed corrupt cache file");
                }
                Ok(Self::with_path(path))
            }
        }
    }

    /// Load cache from a file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|e| {
            cache_error(format!(
                "Failed to open cache file {}: {}",
                path.display(),
                e
            ))
        })?;
        let reader = BufReader::new(file);

        // Use serde_json for deserialization
        let mut cache: Self = serde_json::from_reader(reader).map_err(|e| {
            cache_error(format!(
                "Failed to decode cache file {}: {}",
                path.display(),
                e
            ))
        })?;

        // Version check
        if cache.version != CACHE_VERSION {
            return Err(cache_error(format!(
                "Cache version mismatch: expected {}, got {}",
                CACHE_VERSION, cache.version
            )));
        }

        cache.cache_path = Some(path.to_path_buf());
        Ok(cache)
    }

    /// Save cache to disk.
    pub fn save(&self) -> Result<()> {
        let path = self
            .cache_path
            .as_ref()
            .ok_or_else(|| cache_error("Cannot save cache: no path configured"))?;

        self.save_to(path.clone())
    }

    /// Save cache to a specific path.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        // Use unique temp filename with PID and timestamp to avoid race conditions
        // when multiple range workers save concurrently
        let pid = std::process::id();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_filename = format!(
            "{}.tmp.{}.{}",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("cache"),
            pid,
            timestamp
        );
        let temp_path = path.with_file_name(&temp_filename);

        let file = File::create(&temp_path).map_err(|e| {
            cache_error(format!(
                "Failed to create cache temp file {}: {}",
                temp_path.display(),
                e
            ))
        })?;
        let writer = BufWriter::new(file);

        // Use serde_json for serialization
        serde_json::to_writer(writer, self).map_err(|e| {
            // Clean up temp file on error
            let _ = std::fs::remove_file(&temp_path);
            cache_error(format!("Failed to encode cache: {}", e))
        })?;

        // Atomic replace: remove target first for Windows compatibility
        // (std::fs::rename fails on Windows if target exists)
        // Note: There's still a small race window here, but the unique temp
        // filename prevents concurrent writers from clobbering each other's
        // temp files. The last writer wins, which is acceptable for a cache.
        if path.exists()
            && let Err(e) = std::fs::remove_file(path)
        {
            // Another process may have already removed it; log and continue
            debug!(
                path = %path.display(),
                error = %e,
                "Failed to remove old cache file (may have been removed by another worker)"
            );
        }
        if let Err(e) = std::fs::rename(&temp_path, path) {
            // Clean up temp file on rename failure
            let _ = std::fs::remove_file(&temp_path);
            return Err(cache_error(format!(
                "Failed to rename cache file {} -> {}: {}",
                temp_path.display(),
                path.display(),
                e
            )));
        }

        info!(
            ?path,
            entries = self.entries.len(),
            "Saved blob cache to disk"
        );
        Ok(())
    }

    /// Get a cached entry by blob OID.
    pub fn get(&mut self, blob_oid: &str) -> Option<&StaticFileMap> {
        if self.entries.contains_key(blob_oid) {
            self.hits += 1;
            self.entries.get(blob_oid)
        } else {
            self.misses += 1;
            None
        }
    }

    /// Insert a new entry into the cache.
    pub fn insert(&mut self, blob_oid: impl Into<String>, map: StaticFileMap) {
        self.entries.insert(blob_oid.into(), map);
    }

    /// Get or parse with lazy content loading.
    ///
    /// This is the preferred API when you want to avoid reading blob content
    /// unless necessary. The closure is only called on cache miss.
    ///
    /// # Arguments
    /// * `blob_oid` - Git blob OID (SHA) as hex string
    /// * `base_path` - Base path for resolving relative paths
    /// * `content_fn` - Closure that loads content only on cache miss
    ///
    /// # Example
    /// ```ignore
    /// cache.get_or_parse_with("abc123", "pkgs/top-level", || {
    ///     repo.read_blob(commit, "pkgs/top-level/all-packages.nix")
    ///         .map(|(_, content)| content)
    /// })?;
    /// ```
    pub fn get_or_parse_with<F>(
        &mut self,
        blob_oid: &str,
        base_path: &str,
        content_fn: F,
    ) -> Result<&StaticFileMap>
    where
        F: FnOnce() -> Result<String>,
    {
        // Check if we already have this blob cached
        if self.entries.contains_key(blob_oid) {
            self.hits += 1;
            debug!(blob_oid, "Cache hit for blob");
            return Ok(self.entries.get(blob_oid).unwrap());
        }

        // Cache miss - load content via closure and parse
        self.misses += 1;
        debug!(blob_oid, "Cache miss, loading and parsing all-packages.nix");

        let content = content_fn()?;
        let map = super::static_analysis::parse_all_packages(&content, base_path)?;
        self.entries.insert(blob_oid.to_string(), map);
        Ok(self.entries.get(blob_oid).unwrap())
    }

    /// Get or parse: returns cached entry or parses content and caches result.
    ///
    /// This is a convenience wrapper for when content is already loaded.
    /// Prefer `get_or_parse_with` when content loading can be deferred.
    ///
    /// # Arguments
    /// * `blob_oid` - Git blob OID (SHA) as hex string
    /// * `content` - Content of all-packages.nix (only used if not cached)
    /// * `base_path` - Base path for resolving relative paths
    pub fn get_or_parse(
        &mut self,
        blob_oid: &str,
        content: &str,
        base_path: &str,
    ) -> Result<&StaticFileMap> {
        self.get_or_parse_with(blob_oid, base_path, || Ok(content.to_string()))
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
        }
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all cached entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.hits = 0;
        self.misses = 0;
    }

    /// Set the cache path.
    pub fn set_path(&mut self, path: impl Into<PathBuf>) {
        self.cache_path = Some(path.into());
    }
}

/// Cache statistics.
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    /// Number of cached entries.
    pub entries: usize,
    /// Number of cache hits during session.
    pub hits: usize,
    /// Number of cache misses during session.
    pub misses: usize,
}

impl CacheStats {
    /// Calculate hit ratio.
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        self.hits as f64 / total as f64
    }
}

impl std::fmt::Display for CacheStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} entries, {} hits, {} misses ({:.1}% hit rate)",
            self.entries,
            self.hits,
            self.misses,
            self.hit_ratio() * 100.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_cache_basic_operations() {
        let mut cache = BlobCache::new();

        // Initially empty
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);

        // Insert an entry
        let map = StaticFileMap::default();
        cache.insert("abc123", map);

        assert!(!cache.is_empty());
        assert_eq!(cache.len(), 1);

        // Get the entry (should be a hit)
        let result = cache.get("abc123");
        assert!(result.is_some());
        assert_eq!(cache.stats().hits, 1);

        // Miss for non-existent entry
        let result = cache.get("nonexistent");
        assert!(result.is_none());
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn test_cache_persistence() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("blob_cache.bin");

        // Create and populate cache
        {
            let mut cache = BlobCache::with_path(&cache_path);
            let map = StaticFileMap {
                file_to_attrs: [("foo.nix".to_string(), vec!["bar".to_string()])]
                    .into_iter()
                    .collect(),
                ..Default::default()
            };
            cache.insert("abc123", map);
            cache.save().unwrap();
        }

        // Load and verify
        {
            let mut cache = BlobCache::load(&cache_path).unwrap();
            assert_eq!(cache.len(), 1);

            let map = cache.get("abc123").unwrap();
            assert_eq!(
                map.file_to_attrs.get("foo.nix"),
                Some(&vec!["bar".to_string()])
            );
        }
    }

    #[test]
    fn test_cache_load_or_create() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("new_cache.bin");

        // Should create new cache if file doesn't exist
        let cache = BlobCache::load_or_create(&cache_path).unwrap();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_stats() {
        let mut cache = BlobCache::new();
        let map = StaticFileMap::default();
        cache.insert("abc123", map);

        // 2 hits
        cache.get("abc123");
        cache.get("abc123");

        // 1 miss
        cache.get("nonexistent");

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert!((stats.hit_ratio() - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_cache_version_mismatch() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("version_mismatch.json");

        // Write a cache with wrong version
        let bad_cache = serde_json::json!({
            "version": 9999,  // Wrong version
            "entries": {}
        });
        std::fs::write(&cache_path, bad_cache.to_string()).unwrap();

        // load() should fail with version mismatch
        let result = BlobCache::load(&cache_path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("version mismatch"));
    }

    #[test]
    fn test_cache_corrupt_json() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("corrupt.json");

        // Write invalid JSON
        std::fs::write(&cache_path, "{ invalid json }").unwrap();

        // load() should fail
        let result = BlobCache::load(&cache_path);
        assert!(result.is_err());

        // load_or_create() should recover and rename corrupt file
        let cache = BlobCache::load_or_create(&cache_path).unwrap();
        assert!(cache.is_empty());

        // Corrupt file should be renamed
        let corrupt_path = cache_path.with_extension("corrupt");
        assert!(corrupt_path.exists());
    }

    #[test]
    fn test_cache_save_without_path() {
        let cache = BlobCache::new();

        // save() without path should fail
        let result = cache.save();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("no path configured"));
    }

    #[test]
    fn test_cache_overwrite_existing() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("overwrite.json");

        // Create initial cache
        {
            let mut cache = BlobCache::with_path(&cache_path);
            cache.insert("first", StaticFileMap::default());
            cache.save().unwrap();
        }

        // Overwrite with new cache
        {
            let mut cache = BlobCache::with_path(&cache_path);
            cache.insert("second", StaticFileMap::default());
            cache.save().unwrap();
        }

        // Verify new cache replaced old
        let cache = BlobCache::load(&cache_path).unwrap();
        assert_eq!(cache.len(), 1);
        assert!(cache.entries.contains_key("second"));
        assert!(!cache.entries.contains_key("first"));
    }

    #[test]
    fn test_get_or_parse_with_lazy() {
        let mut cache = BlobCache::new();

        // First call should invoke the closure
        let mut called = false;
        let _result = cache.get_or_parse_with("abc123", "pkgs", || {
            called = true;
            // Return minimal valid Nix content
            Ok("{ }".to_string())
        });
        assert!(called, "Closure should be called on cache miss");

        // Second call should NOT invoke the closure (cache hit)
        let mut called_again = false;
        let _result = cache.get_or_parse_with("abc123", "pkgs", || {
            called_again = true;
            Ok("{ }".to_string())
        });
        assert!(!called_again, "Closure should NOT be called on cache hit");

        // Verify stats
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }
}
