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
                warn!(?path, error = %e, "Failed to load cache, creating new one");
                Ok(Self::with_path(path))
            }
        }
    }

    /// Load cache from a file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|e| {
            cache_error(format!("Failed to open cache file {}: {}", path.display(), e))
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
        let path = self.cache_path.as_ref().ok_or_else(|| {
            cache_error("Cannot save cache: no path configured")
        })?;

        self.save_to(path.clone())
    }

    /// Save cache to a specific path.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        // Write to temp file first, then rename for atomicity
        let temp_path = path.with_extension("tmp");
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
            cache_error(format!("Failed to encode cache: {}", e))
        })?;

        // Atomic rename
        std::fs::rename(&temp_path, path).map_err(|e| {
            cache_error(format!(
                "Failed to rename cache file {} -> {}: {}",
                temp_path.display(),
                path.display(),
                e
            ))
        })?;

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

    /// Get or parse: returns cached entry or parses content and caches result.
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
        // Check if we already have this blob cached
        if self.entries.contains_key(blob_oid) {
            self.hits += 1;
            debug!(blob_oid, "Cache hit for blob");
            return Ok(self.entries.get(blob_oid).unwrap());
        }

        // Parse and cache
        self.misses += 1;
        debug!(blob_oid, "Cache miss, parsing all-packages.nix");

        let map = super::static_analysis::parse_all_packages(content, base_path)?;
        self.entries.insert(blob_oid.to_string(), map);
        Ok(self.entries.get(blob_oid).unwrap())
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
}
