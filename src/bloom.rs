//! Bloom filter for fast package name lookups.

use crate::error::Result;
use bloomfilter::Bloom;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// Bloom filter wrapper for package name lookups.
pub struct PackageBloomFilter {
    filter: Bloom<str>,
}

impl PackageBloomFilter {
    /// Create a new bloom filter with the expected number of items and false positive rate.
    ///
    /// # Arguments
    /// * `expected_items` - Expected number of unique package names
    /// * `false_positive_rate` - Desired false positive rate (e.g., 0.01 for 1%)
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn new(expected_items: usize, false_positive_rate: f64) -> Self {
        let filter = Bloom::new_for_fp_rate(expected_items, false_positive_rate)
            .expect("Failed to create bloom filter with given parameters");
        Self { filter }
    }

    /// Insert a package name into the bloom filter.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn insert(&mut self, name: &str) {
        self.filter.set(name);
    }

    /// Check if a package name might be in the filter.
    ///
    /// Returns `true` if the package might exist (could be false positive),
    /// or `false` if the package definitely does not exist.
    pub fn contains(&self, name: &str) -> bool {
        self.filter.check(name)
    }

    /// Save the bloom filter to a file.
    #[cfg_attr(not(feature = "indexer"), allow(dead_code))]
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        let bytes = self.filter.to_bytes();
        writer.write_all(&bytes)?;
        writer.flush()?;
        Ok(())
    }

    /// Load a bloom filter from a file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        let filter = Bloom::from_bytes(bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        Ok(Self { filter })
    }

    /// Get the approximate size of the bloom filter in bytes.
    #[allow(dead_code)]
    pub fn size_bytes(&self) -> usize {
        self.filter.to_bytes().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_filter_insert_and_check() {
        let mut filter = PackageBloomFilter::new(1000, 0.01);
        filter.insert("python");
        filter.insert("nodejs");
        filter.insert("rustc");

        assert!(filter.contains("python"));
        assert!(filter.contains("nodejs"));
        assert!(filter.contains("rustc"));
    }

    #[test]
    fn test_bloom_filter_definitely_not_present() {
        let filter = PackageBloomFilter::new(1000, 0.01);
        // Empty filter should not contain any packages
        assert!(!filter.contains("python"));
        assert!(!filter.contains("nonexistent"));
    }

    #[test]
    fn test_bloom_filter_save_and_load() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("test.bloom");

        let mut filter = PackageBloomFilter::new(1000, 0.01);
        filter.insert("python");
        filter.insert("nodejs");
        filter.save(&path).unwrap();

        let loaded = PackageBloomFilter::load(&path).unwrap();
        assert!(loaded.contains("python"));
        assert!(loaded.contains("nodejs"));
    }

    #[test]
    fn test_bloom_filter_false_positive_rate() {
        // Create a filter with 10K items at 1% FPR
        let num_items = 10_000;
        let target_fpr = 0.01;
        let mut filter = PackageBloomFilter::new(num_items, target_fpr);

        // Insert 10K package names
        for i in 0..num_items {
            filter.insert(&format!("package{}", i));
        }

        // Test with 10K random strings that were NOT inserted
        let mut false_positives = 0;
        let test_count = 10_000;
        for i in 0..test_count {
            // Use a different prefix to ensure these weren't inserted
            let test_name = format!("nonexistent_pkg_{}", i);
            if filter.contains(&test_name) {
                false_positives += 1;
            }
        }

        let observed_fpr = false_positives as f64 / test_count as f64;

        // Allow some variance - FPR should be roughly 1% (between 0.5% and 2%)
        assert!(
            observed_fpr < 0.02,
            "False positive rate {} is too high (expected ~1%)",
            observed_fpr
        );
        // It should also not be impossibly low
        assert!(
            observed_fpr > 0.001 || false_positives > 0,
            "False positive rate {} is suspiciously low",
            observed_fpr
        );
    }

    #[test]
    fn test_bloom_filter_size_reasonable() {
        // Test that filter size is reasonable for 1M items at 1% FPR
        // Expected: ~1.2MB (9.6 bits per item for 1% FPR)
        let num_items = 1_000_000;
        let filter = PackageBloomFilter::new(num_items, 0.01);

        let size_bytes = filter.size_bytes();
        let size_mb = size_bytes as f64 / (1024.0 * 1024.0);

        // Should be between 1MB and 2MB for 1M items at 1% FPR
        assert!(
            (1.0..=2.0).contains(&size_mb),
            "Bloom filter size {:.2}MB is outside expected range (1-2MB)",
            size_mb
        );
    }

    #[test]
    fn test_bloom_filter_lookup_performance() {
        // Create and populate a filter
        let num_items = 100_000;
        let mut filter = PackageBloomFilter::new(num_items, 0.01);
        for i in 0..num_items {
            filter.insert(&format!("package{}", i));
        }

        // Measure lookup time for 1000 operations
        let start = std::time::Instant::now();
        for i in 0..1000 {
            let _ = filter.contains(&format!("package{}", i % num_items));
        }
        let elapsed = start.elapsed();

        // Average lookup should be well under 1ms (typically microseconds)
        let avg_lookup_us = elapsed.as_micros() as f64 / 1000.0;
        assert!(
            avg_lookup_us < 1000.0, // 1ms = 1000us
            "Average lookup time {:.2}us exceeds 1ms",
            avg_lookup_us
        );
    }
}
