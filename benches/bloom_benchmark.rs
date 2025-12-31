//! Benchmarks for bloom filter operations.

use bloomfilter::Bloom;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use tempfile::tempdir;

/// Create a bloom filter with the specified number of items.
fn create_bloom_filter(num_items: usize) -> Bloom<String> {
    let mut filter: Bloom<String> =
        Bloom::new_for_fp_rate(num_items, 0.01).expect("Failed to create bloom filter");

    for i in 0..num_items {
        let name = format!("package{}", i);
        filter.set(&name);
    }

    filter
}

fn bench_bloom_contains(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_contains");

    for size in [10000, 100000, 500000].iter() {
        let filter = create_bloom_filter(*size);

        // Benchmark positive lookup (item exists)
        let positive_key = "package5000".to_string();
        group.bench_with_input(BenchmarkId::new("positive", size), size, |b, _| {
            b.iter(|| {
                let result = filter.check(black_box(&positive_key));
                black_box(result)
            });
        });

        // Benchmark negative lookup (item doesn't exist)
        let negative_key = "nonexistent_package_xyz".to_string();
        group.bench_with_input(BenchmarkId::new("negative", size), size, |b, _| {
            b.iter(|| {
                let result = filter.check(black_box(&negative_key));
                black_box(result)
            });
        });
    }

    group.finish();
}

fn bench_bloom_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_insert");

    for size in [10000, 100000].iter() {
        let insert_key = "new_package".to_string();
        group.bench_with_input(BenchmarkId::new("single", size), size, |b, _| {
            let mut filter: Bloom<String> = Bloom::new_for_fp_rate(*size, 0.01).unwrap();
            b.iter(|| {
                filter.set(black_box(&insert_key));
            });
        });
    }

    group.finish();
}

fn bench_bloom_save_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_save_load");

    for size in [10000, 100000, 500000].iter() {
        let filter = create_bloom_filter(*size);
        let dir = tempdir().unwrap();
        let path = dir.path().join("bloom.bin");

        // Save the filter first
        let bytes = filter.to_bytes();
        let file = File::create(&path).unwrap();
        let mut writer = BufWriter::new(file);
        writer.write_all(&bytes).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let file_size = std::fs::metadata(&path).unwrap().len();

        group.bench_with_input(
            BenchmarkId::new(format!("load_{}KB", file_size / 1024), size),
            size,
            |b, _| {
                b.iter(|| {
                    let file = File::open(&path).unwrap();
                    let mut reader = BufReader::new(file);
                    let mut bytes = Vec::new();
                    reader.read_to_end(&mut bytes).unwrap();
                    let loaded: Bloom<String> = Bloom::from_bytes(bytes).unwrap();
                    black_box(loaded)
                });
            },
        );
    }

    group.finish();
}

fn bench_bloom_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("bloom_creation");

    for size in [10000, 100000, 500000].iter() {
        group.bench_with_input(BenchmarkId::new("new", size), size, |b, size| {
            b.iter(|| {
                let filter: Bloom<String> = Bloom::new_for_fp_rate(*size, 0.01).unwrap();
                black_box(filter)
            });
        });

        group.bench_with_input(BenchmarkId::new("populate", size), size, |b, size| {
            b.iter(|| {
                let filter = create_bloom_filter(*size);
                black_box(filter)
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_bloom_contains,
    bench_bloom_insert,
    bench_bloom_save_load,
    bench_bloom_creation
);
criterion_main!(benches);
