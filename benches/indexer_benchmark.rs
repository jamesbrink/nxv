//! Benchmarks for indexer performance.
//!
//! These benchmarks measure:
//! - Database insertion rate (packages/second)
//! - Batch insert performance at various sizes
//!
//! Note: Actual package extraction from nixpkgs requires a real nixpkgs checkout
//! and is not benchmarked here. Use `nxv index` with verbose output for real
//! extraction rate measurements.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rusqlite::Connection;
use tempfile::tempdir;

/// Simulated package data for benchmarking.
struct SimulatedPackage {
    name: String,
    version: String,
    first_commit_hash: String,
    first_commit_date: i64,
    last_commit_hash: String,
    last_commit_date: i64,
    attribute_path: String,
    description: Option<String>,
    license: Option<String>,
    homepage: Option<String>,
    maintainers: Option<String>,
    platforms: Option<String>,
}

impl SimulatedPackage {
    fn new(index: usize) -> Self {
        Self {
            name: format!("package{}", index),
            version: format!("1.{}.0", index % 100),
            first_commit_hash: format!("abc{:040}", index),
            first_commit_date: 1700000000 + index as i64,
            last_commit_hash: format!("def{:040}", index),
            last_commit_date: 1700100000 + index as i64,
            attribute_path: format!("packages.package{}", index),
            description: Some(format!("Description for package {}", index)),
            license: Some(r#"["MIT"]"#.to_string()),
            homepage: Some("https://example.com".to_string()),
            maintainers: Some(r#"["maintainer"]"#.to_string()),
            platforms: Some(r#"["x86_64-linux", "aarch64-linux"]"#.to_string()),
        }
    }
}

/// Create a benchmark database with schema.
fn create_benchmark_db() -> (tempfile::TempDir, Connection) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("bench.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS package_versions (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            version TEXT NOT NULL,
            first_commit_hash TEXT NOT NULL,
            first_commit_date INTEGER NOT NULL,
            last_commit_hash TEXT NOT NULL,
            last_commit_date INTEGER NOT NULL,
            attribute_path TEXT NOT NULL,
            description TEXT,
            license TEXT,
            homepage TEXT,
            maintainers TEXT,
            platforms TEXT,
            UNIQUE(attribute_path, version, first_commit_hash)
        );

        CREATE INDEX IF NOT EXISTS idx_packages_name ON package_versions(name);
        CREATE INDEX IF NOT EXISTS idx_packages_name_version ON package_versions(name, version, first_commit_date);
        CREATE INDEX IF NOT EXISTS idx_packages_attr ON package_versions(attribute_path);
        CREATE INDEX IF NOT EXISTS idx_packages_first_date ON package_versions(first_commit_date DESC);
        CREATE INDEX IF NOT EXISTS idx_packages_last_date ON package_versions(last_commit_date DESC);

        CREATE VIRTUAL TABLE IF NOT EXISTS package_versions_fts
        USING fts5(name, description, content=package_versions, content_rowid=id);

        CREATE TRIGGER IF NOT EXISTS package_versions_ai AFTER INSERT ON package_versions BEGIN
            INSERT INTO package_versions_fts(rowid, name, description)
            VALUES (new.id, new.name, new.description);
        END;
        "#,
    )
    .unwrap();

    (dir, conn)
}

/// Insert packages one at a time (baseline).
fn insert_packages_one_by_one(conn: &Connection, packages: &[SimulatedPackage]) {
    let mut stmt = conn
        .prepare_cached(
            r#"
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date,
                 attribute_path, description, license, homepage, maintainers, platforms)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .unwrap();

    for pkg in packages {
        stmt.execute(rusqlite::params![
            pkg.name,
            pkg.version,
            pkg.first_commit_hash,
            pkg.first_commit_date,
            pkg.last_commit_hash,
            pkg.last_commit_date,
            pkg.attribute_path,
            pkg.description,
            pkg.license,
            pkg.homepage,
            pkg.maintainers,
            pkg.platforms,
        ])
        .unwrap();
    }
}

/// Insert packages in a transaction (much faster).
fn insert_packages_batch(conn: &Connection, packages: &[SimulatedPackage]) {
    let tx = conn.unchecked_transaction().unwrap();

    {
        let mut stmt = conn
            .prepare_cached(
                r#"
                INSERT INTO package_versions
                    (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date,
                     attribute_path, description, license, homepage, maintainers, platforms)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .unwrap();

        for pkg in packages {
            stmt.execute(rusqlite::params![
                pkg.name,
                pkg.version,
                pkg.first_commit_hash,
                pkg.first_commit_date,
                pkg.last_commit_hash,
                pkg.last_commit_date,
                pkg.attribute_path,
                pkg.description,
                pkg.license,
                pkg.homepage,
                pkg.maintainers,
                pkg.platforms,
            ])
            .unwrap();
        }
    }

    tx.commit().unwrap();
}

fn bench_insertion_rate(c: &mut Criterion) {
    let mut group = c.benchmark_group("insertion_rate");

    // Test different batch sizes
    for size in [100, 1000, 5000].iter() {
        let packages: Vec<SimulatedPackage> = (0..*size).map(SimulatedPackage::new).collect();

        group.throughput(Throughput::Elements(*size as u64));

        group.bench_with_input(BenchmarkId::new("one_by_one", size), size, |b, _| {
            b.iter_with_setup(create_benchmark_db, |(_dir, conn)| {
                insert_packages_one_by_one(&conn, &packages);
            });
        });

        group.bench_with_input(BenchmarkId::new("batch", size), size, |b, _| {
            b.iter_with_setup(create_benchmark_db, |(_dir, conn)| {
                insert_packages_batch(&conn, &packages);
            });
        });
    }

    group.finish();
}

fn bench_simulated_commits(c: &mut Criterion) {
    let mut group = c.benchmark_group("simulated_commits");

    // Simulate processing multiple commits, each with many packages
    // This mimics the indexer workflow

    let packages_per_commit = 50000; // Typical nixpkgs has ~80k packages
    let commits = 10;

    group.throughput(Throughput::Elements((packages_per_commit * commits) as u64));

    group.bench_function("process_commits", |b| {
        b.iter_with_setup(
            || {
                let (dir, conn) = create_benchmark_db();
                let all_packages: Vec<Vec<SimulatedPackage>> = (0..commits)
                    .map(|commit_idx| {
                        (0..packages_per_commit)
                            .map(|pkg_idx| {
                                SimulatedPackage::new(commit_idx * packages_per_commit + pkg_idx)
                            })
                            .collect()
                    })
                    .collect();
                (dir, conn, all_packages)
            },
            |(_dir, conn, all_packages)| {
                for packages in all_packages {
                    insert_packages_batch(&conn, &packages);
                }
            },
        );
    });

    group.finish();
}

criterion_group!(benches, bench_insertion_rate, bench_simulated_commits);
criterion_main!(benches);
