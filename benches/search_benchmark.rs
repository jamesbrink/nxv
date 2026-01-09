//! Benchmarks for search query performance.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rusqlite::Connection;
use std::hint::black_box;
use tempfile::tempdir;

/// Create a test database with sample data.
fn create_benchmark_db(num_packages: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("bench.db");

    let conn = Connection::open(&db_path).unwrap();

    // Create schema
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

    // Insert test data
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut stmt = conn.prepare(
            r#"
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date,
                 attribute_path, description, license, homepage)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        ).unwrap();

        for i in 0..num_packages {
            let name = format!("package{}", i);
            let version = format!("1.{}.0", i % 100);
            let attr_path = format!("packages.{}", name);
            let description = format!("Description for package {} with some searchable text", i);

            stmt.execute(rusqlite::params![
                name,
                version,
                format!("abc{:040}", i),
                1700000000 + i as i64,
                format!("def{:040}", i),
                1700100000 + i as i64,
                attr_path,
                description,
                r#"["MIT"]"#,
                "https://example.com"
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();

    (dir, db_path)
}

fn bench_search_by_name(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_by_name");

    for size in [1000, 10000, 50000].iter() {
        let (_dir, db_path) = create_benchmark_db(*size);
        let conn = Connection::open(&db_path).unwrap();

        group.bench_with_input(BenchmarkId::new("prefix", size), size, |b, _| {
            b.iter(|| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM package_versions WHERE name LIKE ? || '%' ORDER BY last_commit_date DESC LIMIT 50"
                ).unwrap();
                let results: Vec<String> = stmt
                    .query_map(["package1"], |row| row.get(1))
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .collect();
                black_box(results)
            });
        });

        group.bench_with_input(BenchmarkId::new("exact", size), size, |b, _| {
            b.iter(|| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM package_versions WHERE name = ? ORDER BY last_commit_date DESC LIMIT 50"
                ).unwrap();
                let results: Vec<String> = stmt
                    .query_map(["package100"], |row| row.get(1))
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .collect();
                black_box(results)
            });
        });
    }

    group.finish();
}

fn bench_search_fts(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_fts");

    for size in [1000, 10000, 50000].iter() {
        let (_dir, db_path) = create_benchmark_db(*size);
        let conn = Connection::open(&db_path).unwrap();

        group.bench_with_input(BenchmarkId::new("description", size), size, |b, _| {
            b.iter(|| {
                let mut stmt = conn
                    .prepare_cached(
                        r#"
                    SELECT pv.* FROM package_versions pv
                    JOIN package_versions_fts fts ON pv.id = fts.rowid
                    WHERE package_versions_fts MATCH ?
                    ORDER BY pv.last_commit_date DESC
                    LIMIT 50
                    "#,
                    )
                    .unwrap();
                let results: Vec<String> = stmt
                    .query_map(["searchable"], |row| row.get(1))
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .collect();
                black_box(results)
            });
        });
    }

    group.finish();
}

fn bench_index_loading(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_loading");

    for size in [1000, 10000].iter() {
        let (_dir, db_path) = create_benchmark_db(*size);

        group.bench_with_input(BenchmarkId::new("open_readonly", size), size, |b, _| {
            b.iter(|| {
                let conn = Connection::open_with_flags(
                    &db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )
                .unwrap();
                black_box(conn)
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_search_by_name,
    bench_search_fts,
    bench_index_loading
);
criterion_main!(benches);
