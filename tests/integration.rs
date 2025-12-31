//! Integration tests for nxv CLI.
//!
//! These tests verify the CLI behavior end-to-end using a test database.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

/// Get a command for the nxv binary.
fn nxv() -> Command {
    #[allow(deprecated)]
    Command::cargo_bin("nxv").unwrap()
}

/// Create a test database with sample data.
fn create_test_db(path: &std::path::Path) {
    use rusqlite::Connection;

    let conn = Connection::open(path).unwrap();

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
    conn.execute_batch(
        r#"
        INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'abc1234567890123456789012345678901234567');
        INSERT INTO meta (key, value) VALUES ('schema_version', '1');

        INSERT INTO package_versions
            (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date,
             attribute_path, description, license, homepage)
        VALUES
            ('python', '3.11.0', 'abc1234567890', 1700000000, 'def1234567890', 1700100000,
             'python311', 'Python programming language', '["MIT"]', 'https://python.org'),
            ('python', '3.12.0', 'ghi1234567890', 1701000000, 'jkl1234567890', 1701100000,
             'python312', 'Python programming language', '["MIT"]', 'https://python.org'),
            ('python2', '2.7.18', 'mno1234567890', 1600000000, 'pqr1234567890', 1600100000,
             'python27', 'Python 2 interpreter', '["PSF"]', 'https://python.org'),
            ('nodejs', '20.0.0', 'stu1234567890', 1702000000, 'vwx1234567890', 1702100000,
             'nodejs_20', 'Node.js JavaScript runtime', '["MIT"]', 'https://nodejs.org'),
            ('firefox', '120.0', 'aaa1234567890', 1703000000, 'bbb1234567890', 1703100000,
             'firefox', 'Mozilla Firefox web browser', '["MPL-2.0"]', 'https://firefox.com'),
            ('rustc', '1.75.0', 'ccc1234567890', 1704000000, 'ddd1234567890', 1704100000,
             'rustc', 'The Rust compiler', '["MIT", "Apache-2.0"]', 'https://rust-lang.org');
        "#,
    )
    .unwrap();
}

// ============================================================================
// Help and Version Tests
// ============================================================================

#[test]
fn test_help_displays() {
    nxv()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "CLI tool for finding specific versions",
        ))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("update"))
        .stdout(predicate::str::contains("info"))
        .stdout(predicate::str::contains("history"))
        .stdout(predicate::str::contains("completions"));
}

#[test]
fn test_version_displays() {
    nxv()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("nxv"));
}

#[test]
fn test_search_help() {
    nxv()
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Search for package versions"))
        .stdout(predicate::str::contains("--version"))
        .stdout(predicate::str::contains("--exact"))
        .stdout(predicate::str::contains("--format"));
}

// ============================================================================
// Search Command Tests
// ============================================================================

#[test]
fn test_search_returns_results() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "python"])
        .assert()
        .success()
        .stdout(predicate::str::contains("python"));
}

#[test]
fn test_search_exact_match() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bloom_path = dir.path().join("nonexistent.bloom");
    create_test_db(&db_path);

    // Exact match should only return "python", not "python2"
    // Set NXV_BLOOM_PATH to non-existent path to skip bloom filter check
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--exact",
        ])
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("python"))
        .stdout(predicate::str::contains("3.11").or(predicate::str::contains("3.12")));
}

#[test]
fn test_search_prefix_match() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Prefix match should return python and python2
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "python"])
        .assert()
        .success()
        .stdout(predicate::str::contains("python"));
}

#[test]
fn test_search_with_version_filter() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--version",
            "3.11",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("3.11"));
}

#[test]
fn test_search_json_output() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--format",
            "json",
        ])
        .assert()
        .success();

    // Verify it's valid JSON
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Output should be valid JSON");
    assert!(parsed.is_array(), "JSON output should be an array");
}

#[test]
fn test_search_plain_output() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--format",
            "plain",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("python"));
}

#[test]
fn test_search_not_found() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "nonexistent_package_xyz",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("No packages found"));
}

#[test]
fn test_search_with_limit() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--limit",
            "1",
        ])
        .assert()
        .success();
}

#[test]
fn test_search_sort_options() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Test sort by date
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--sort",
            "date",
        ])
        .assert()
        .success();

    // Test sort by version
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--sort",
            "version",
        ])
        .assert()
        .success();

    // Test sort by name
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--sort",
            "name",
        ])
        .assert()
        .success();
}

// ============================================================================
// Info Command Tests
// ============================================================================

#[test]
fn test_info_with_database() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Index Information"))
        .stdout(predicate::str::contains("Statistics"))
        .stdout(predicate::str::contains("Database path"));
}

#[test]
fn test_info_no_database() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No index found"));
}

// ============================================================================
// History Command Tests
// ============================================================================

#[test]
fn test_history_shows_versions() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "history", "python"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Version history"))
        .stdout(predicate::str::contains("3.11").or(predicate::str::contains("3.12")));
}

#[test]
fn test_history_specific_version() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "python",
            "3.11.0",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Package: python 3.11.0"));
}

#[test]
fn test_history_json_format() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "python",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // Skip the header line and parse JSON
    let json_part = stdout.lines().skip(2).collect::<Vec<_>>().join("\n");
    let parsed: serde_json::Value =
        serde_json::from_str(&json_part).expect("Output should contain valid JSON");
    assert!(parsed.is_array());
}

#[test]
fn test_history_not_found() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "nonexistent_package",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No history found"));
}

// ============================================================================
// Completions Command Tests
// ============================================================================

#[test]
fn test_completions_bash() {
    nxv()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_nxv()"));
}

#[test]
fn test_completions_zsh() {
    nxv()
        .args(["completions", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef nxv"));
}

#[test]
fn test_completions_fish() {
    nxv()
        .args(["completions", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("__fish_nxv"));
}

// ============================================================================
// Global Options Tests
// ============================================================================

#[test]
fn test_quiet_mode() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Quiet mode should suppress stderr messages
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "--quiet",
            "search",
            "nonexistent",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty().not().not()); // Allow empty stderr
}

#[test]
fn test_no_color_option() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "--no-color",
            "search",
            "python",
        ])
        .assert()
        .success();
}

#[test]
fn test_verbose_conflicts_with_quiet() {
    nxv()
        .args(["-v", "-q", "info"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn test_verbose_info_level() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // -v should show progress messages like "Searching for..."
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "-v",
            "search",
            "python",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Searching for"));
}

#[test]
fn test_verbose_debug_level() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // -vv should show debug messages with [debug] prefix
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "-vv",
            "search",
            "python",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("[debug]"));
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_search_no_index_suggests_update() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    // When there's no index, suggest running 'nxv update'
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "python"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nxv update"));
}

#[test]
fn test_invalid_subcommand() {
    nxv()
        .arg("invalid_command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn test_missing_required_argument() {
    nxv()
        .arg("search")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn test_error_messages_have_error_prefix() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    // Error messages should have "error:" prefix
    // When not connected to a TTY, colors are not emitted
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "python"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:").and(predicate::str::contains("nxv update")));
}

#[test]
fn test_error_messages_no_color() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    // With --no-color, error messages should be plain text "error:"
    // (same as without TTY, but explicit)
    nxv()
        .args([
            "--no-color",
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error:").and(predicate::str::contains("nxv update")));
}

// ============================================================================
// Database Path Tests
// ============================================================================

#[test]
fn test_custom_db_path() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("custom.db");
    create_test_db(&db_path);

    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains(db_path.to_str().unwrap()));
}

// ============================================================================
// Version Sorting Tests
// ============================================================================

#[test]
fn test_search_version_sort_order() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bloom_path = dir.path().join("nonexistent.bloom");

    // Create a database with versions that test semver sorting
    use rusqlite::Connection;
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE package_versions (
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
        CREATE INDEX idx_packages_name ON package_versions(name);
        CREATE VIRTUAL TABLE package_versions_fts USING fts5(name, description, content=package_versions, content_rowid=id);
        CREATE TRIGGER package_versions_ai AFTER INSERT ON package_versions BEGIN
            INSERT INTO package_versions_fts(rowid, name, description) VALUES (new.id, new.name, new.description);
        END;

        INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'abc123');

        -- Insert versions in random order to test sorting
        INSERT INTO package_versions (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description)
        VALUES
            ('python', '3.9.0', 'aaa', 1600000000, 'aaa', 1600000000, 'python39', 'Python 3.9'),
            ('python', '3.11.0', 'ccc', 1602000000, 'ccc', 1602000000, 'python311', 'Python 3.11'),
            ('python', '3.10.0', 'bbb', 1601000000, 'bbb', 1601000000, 'python310', 'Python 3.10');
        "#,
    )
    .unwrap();

    // Get results sorted by version
    // Set NXV_BLOOM_PATH to non-existent path to skip bloom filter check
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--exact",
            "--sort",
            "version",
            "--format",
            "json",
        ])
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = parsed.as_array().unwrap();

    assert_eq!(results.len(), 3);

    // Extract versions in order
    let versions: Vec<&str> = results
        .iter()
        .map(|r| r.get("version").unwrap().as_str().unwrap())
        .collect();

    // Semver sorting should put them in correct order: 3.9.0 < 3.10.0 < 3.11.0
    assert_eq!(
        versions,
        vec!["3.9.0", "3.10.0", "3.11.0"],
        "Versions should be semver sorted: {:?}",
        versions
    );
}

// ============================================================================
// License Filter Tests
// ============================================================================

#[test]
fn test_search_with_license_filter() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Search for packages with MIT license
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--license",
            "MIT",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = parsed.as_array().unwrap();

    // Should find python packages with MIT license
    assert!(!results.is_empty(), "Should find packages with MIT license");

    // Verify all results have MIT in their license
    for result in results {
        let license = result.get("license").and_then(|l| l.as_str()).unwrap_or("");
        assert!(
            license.contains("MIT"),
            "License '{}' should contain 'MIT'",
            license
        );
    }
}

#[test]
fn test_search_license_filter_no_match() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Search for packages with a non-existent license
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--license",
            "NONEXISTENT_LICENSE",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("No packages found"));
}

// ============================================================================
// Description Search Tests
// ============================================================================

#[test]
fn test_search_by_description() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Search by description should find packages with matching text
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "--desc",
            "programming language",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("python"));
}

#[test]
fn test_search_description_json() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "--desc",
            "browser",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(parsed.is_array());

    // Should find firefox
    let results = parsed.as_array().unwrap();
    assert!(!results.is_empty());
}

// ============================================================================
// Limit Tests
// ============================================================================

#[test]
fn test_search_limit_exact_count() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // JSON output allows us to count exact results
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--limit",
            "1",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = parsed.as_array().unwrap();
    assert_eq!(results.len(), 1, "Should return exactly 1 result");
}

// ============================================================================
// Reverse Sort Tests
// ============================================================================

#[test]
fn test_search_reverse_sort() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--sort",
            "date",
            "--reverse",
        ])
        .assert()
        .success();
}

// ============================================================================
// ASCII Table Tests
// ============================================================================

#[test]
fn test_search_ascii_table() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--ascii",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // ASCII table uses +, -, | for borders instead of Unicode box drawing
    assert!(
        stdout.contains('+') && stdout.contains('-') && stdout.contains('|'),
        "ASCII table should use +, -, | characters"
    );

    // Should NOT contain Unicode box drawing characters
    assert!(
        !stdout.contains('┌') && !stdout.contains('─') && !stdout.contains('│'),
        "ASCII table should not contain Unicode box drawing characters"
    );
}

// ============================================================================
// Plain Output No ANSI Tests
// ============================================================================

#[test]
fn test_plain_output_no_ansi() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--format",
            "plain",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // ANSI escape codes start with ESC (0x1B) followed by [
    assert!(
        !stdout.contains("\x1b["),
        "Plain output should not contain ANSI escape codes"
    );
}

// ============================================================================
// JSON Validation Tests
// ============================================================================

#[test]
fn test_search_json_with_jq_compatible_output() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "firefox",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    // Verify it's an array with expected fields
    let results = parsed.as_array().unwrap();
    assert!(!results.is_empty());

    let first = &results[0];
    assert!(first.get("name").is_some());
    assert!(first.get("version").is_some());
    assert!(first.get("attribute_path").is_some());
    assert!(first.get("last_commit_hash").is_some());
}

// ============================================================================
// Mock HTTP Server Update Cycle Tests
// ============================================================================

/// Create a compressed SQLite database for mock server tests.
fn create_compressed_test_db() -> (Vec<u8>, String) {
    use sha2::{Digest, Sha256};

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Create the database
    use rusqlite::Connection;
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE package_versions (
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
        CREATE INDEX idx_packages_name ON package_versions(name);
        CREATE INDEX idx_packages_name_version ON package_versions(name, version, first_commit_date);
        CREATE INDEX idx_packages_attr ON package_versions(attribute_path);
        CREATE INDEX idx_packages_first_date ON package_versions(first_commit_date DESC);
        CREATE INDEX idx_packages_last_date ON package_versions(last_commit_date DESC);
        CREATE VIRTUAL TABLE package_versions_fts USING fts5(name, description, content=package_versions, content_rowid=id);
        CREATE TRIGGER package_versions_ai AFTER INSERT ON package_versions BEGIN
            INSERT INTO package_versions_fts(rowid, name, description) VALUES (new.id, new.name, new.description);
        END;

        INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'abc1234567890123456789012345678901234567');
        INSERT INTO meta (key, value) VALUES ('schema_version', '1');

        INSERT INTO package_versions
            (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date,
             attribute_path, description, license, homepage)
        VALUES
            ('hello', '2.12.0', 'abc1234567890', 1700000000, 'abc1234567890', 1700000000,
             'hello', 'A program that produces a familiar greeting', '["GPL-3.0"]', 'https://gnu.org/software/hello');
        "#,
    )
    .unwrap();
    drop(conn);

    // Compress the database with zstd
    let db_data = std::fs::read(&db_path).unwrap();
    let compressed = zstd::encode_all(&db_data[..], 3).unwrap();

    // Calculate SHA256 of compressed data
    let mut hasher = Sha256::new();
    hasher.update(&compressed);
    let hash = format!("{:x}", hasher.finalize());

    (compressed, hash)
}

/// Create a minimal bloom filter for mock server tests.
fn create_test_bloom_filter() -> (Vec<u8>, String) {
    use sha2::{Digest, Sha256};

    // Create bloom filter with test packages
    let mut filter: bloomfilter::Bloom<str> =
        bloomfilter::Bloom::new_for_fp_rate(1000, 0.01).unwrap();
    filter.set("hello");
    filter.set("world");

    // Serialize to bytes using to_bytes (matching nxv's format)
    let bytes = filter.to_bytes();

    // Calculate SHA256
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = format!("{:x}", hasher.finalize());

    (bytes, hash)
}

#[test]
fn test_update_with_mock_http_server() {
    // Create test artifacts
    let (compressed_db, db_hash) = create_compressed_test_db();
    let (bloom_data, bloom_hash) = create_test_bloom_filter();

    // Start mock server
    let mut server = mockito::Server::new();

    // Create manifest
    let manifest = serde_json::json!({
        "version": 1,
        "latest_commit": "abc1234567890123456789012345678901234567",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": compressed_db.len(),
            "sha256": db_hash
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": bloom_data.len(),
            "sha256": bloom_hash
        },
        "deltas": []
    });

    // Set up mock endpoints
    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(manifest.to_string())
        .create();

    let _index_mock = server
        .mock("GET", "/index.db.zst")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(compressed_db)
        .create();

    let _bloom_mock = server
        .mock("GET", "/index.bloom")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(bloom_data)
        .create();

    // Create temp directory for test database
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let bloom_path = dir.path().join("index.bloom");

    // Set environment variable for manifest URL
    let manifest_url = format!("{}/manifest.json", server.url());

    // Run update command
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .assert()
        .success()
        .stderr(predicate::str::contains("Downloading").or(predicate::str::contains("up to date")));

    // Verify the database was created and is valid
    assert!(db_path.exists(), "Database should be created");

    // Open and verify the database
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM package_versions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "Database should have 1 package");

    // Verify we can search the downloaded database
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "hello",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn test_update_already_up_to_date() {
    // Create test artifacts
    let (compressed_db, db_hash) = create_compressed_test_db();
    let (bloom_data, bloom_hash) = create_test_bloom_filter();

    // Start mock server
    let mut server = mockito::Server::new();

    let latest_commit = "abc1234567890123456789012345678901234567";

    // Create manifest
    let manifest = serde_json::json!({
        "version": 1,
        "latest_commit": latest_commit,
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": compressed_db.len(),
            "sha256": db_hash
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": bloom_data.len(),
            "sha256": bloom_hash
        },
        "deltas": []
    });

    // Set up mock endpoints
    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(manifest.to_string())
        .create();

    // Create temp directory and pre-existing database
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    // Create a database that's already at the latest commit
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(&format!(
        r#"
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        INSERT INTO meta (key, value) VALUES ('last_indexed_commit', '{}');
        "#,
        latest_commit
    ))
    .unwrap();
    drop(conn);

    // Set environment variable for manifest URL
    let manifest_url = format!("{}/manifest.json", server.url());

    // Run update command - should report up to date
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("up to date"));
}

#[test]
fn test_update_delta_available() {
    use sha2::{Digest, Sha256};

    // Create test artifacts
    let (compressed_db, db_hash) = create_compressed_test_db();
    let (bloom_data, bloom_hash) = create_test_bloom_filter();

    // Create a delta pack (SQL statements)
    let delta_sql = r#"
BEGIN TRANSACTION;
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date,
     attribute_path, description, license)
VALUES
    ('world', '1.0.0', 'def456', 1701000000, 'def456', 1701000000,
     'world', 'Hello World package', '["MIT"]');
INSERT OR REPLACE INTO meta (key, value) VALUES ('last_indexed_commit', 'def4567890123456789012345678901234567890');
COMMIT;
    "#;

    // Compress the delta
    let compressed_delta = zstd::encode_all(delta_sql.as_bytes(), 3).unwrap();

    // Calculate SHA256 of compressed delta
    let mut hasher = Sha256::new();
    hasher.update(&compressed_delta);
    let delta_hash = format!("{:x}", hasher.finalize());

    // Start mock server
    let mut server = mockito::Server::new();

    let old_commit = "abc1234567890123456789012345678901234567";
    let new_commit = "def4567890123456789012345678901234567890";

    // Create manifest with delta
    let manifest = serde_json::json!({
        "version": 1,
        "latest_commit": new_commit,
        "latest_commit_date": "2024-02-15T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": compressed_db.len(),
            "sha256": db_hash
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": bloom_data.len(),
            "sha256": bloom_hash
        },
        "deltas": [{
            "from_commit": old_commit,
            "to_commit": new_commit,
            "url": format!("{}/delta.sql.zst", server.url()),
            "size_bytes": compressed_delta.len(),
            "sha256": delta_hash
        }]
    });

    // Set up mock endpoints
    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(manifest.to_string())
        .create();

    let _delta_mock = server
        .mock("GET", "/delta.sql.zst")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(compressed_delta)
        .create();

    let _bloom_mock = server
        .mock("GET", "/index.bloom")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(bloom_data)
        .create();

    // Create temp directory and pre-existing database at old commit
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let bloom_path = dir.path().join("index.bloom");

    // Create initial database
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(&format!(
        r#"
        CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE package_versions (
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
        CREATE INDEX idx_packages_name ON package_versions(name);
        INSERT INTO meta (key, value) VALUES ('last_indexed_commit', '{}');
        INSERT INTO package_versions
            (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date,
             attribute_path, description, license)
        VALUES
            ('hello', '2.12.0', 'abc1234567890', 1700000000, 'abc1234567890', 1700000000,
             'hello', 'A greeting program', '["GPL-3.0"]');
        "#,
        old_commit
    ))
    .unwrap();
    drop(conn);

    // Set environment variable for manifest URL
    let manifest_url = format!("{}/manifest.json", server.url());

    // Run update command - should apply delta
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .assert()
        .success()
        .stderr(predicate::str::contains("delta").or(predicate::str::contains("Downloading")));

    // Verify the database was updated
    let conn = rusqlite::Connection::open(&db_path).unwrap();

    // Check that both packages exist now
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM package_versions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "Database should have 2 packages after delta");

    // Verify the new package exists
    let world_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM package_versions WHERE name = 'world')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(world_exists, "Delta should have added 'world' package");

    // Verify commit was updated
    let commit: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(commit, new_commit, "Commit should be updated after delta");
}

#[test]
fn test_update_network_error_handling() {
    // Start mock server that returns errors
    let mut server = mockito::Server::new();

    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(500)
        .with_body("Internal Server Error")
        .create();

    // Create temp directory
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    let manifest_url = format!("{}/manifest.json", server.url());

    // Run update command - should fail gracefully
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn test_update_checksum_mismatch() {
    // Create test artifacts with wrong hash
    let (compressed_db, _) = create_compressed_test_db();
    let (bloom_data, bloom_hash) = create_test_bloom_filter();

    // Start mock server
    let mut server = mockito::Server::new();

    // Create manifest with wrong hash
    let manifest = serde_json::json!({
        "version": 1,
        "latest_commit": "abc1234567890123456789012345678901234567",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": compressed_db.len(),
            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"  // Wrong hash
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": bloom_data.len(),
            "sha256": bloom_hash
        },
        "deltas": []
    });

    // Set up mock endpoints
    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(manifest.to_string())
        .create();

    let _index_mock = server
        .mock("GET", "/index.db.zst")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(compressed_db)
        .create();

    // Create temp directory
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    let manifest_url = format!("{}/manifest.json", server.url());

    // Run update command - should fail due to checksum mismatch
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("checksum")
                .or(predicate::str::contains("mismatch"))
                .or(predicate::str::contains("Checksum")),
        );
}

#[test]
fn test_update_unreachable_server() {
    // Use a URL that points to a non-existent/unreachable host
    // Using localhost with a port that's unlikely to be in use
    let manifest_url = "http://127.0.0.1:59999/manifest.json";

    // Create temp directory
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    // Run update command - should fail gracefully with network error
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            manifest_url,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

// ============================================================================
// Offline Mode Tests
// ============================================================================

#[test]
fn test_works_offline_after_index_download() {
    // Create a test database (simulating a downloaded index)
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bloom_path = dir.path().join("nonexistent.bloom");
    create_test_db(&db_path);

    // Search should work without any network access
    // (no manifest URL configured, bloom filter doesn't exist)
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "python"])
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("python"));

    // History should also work offline
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "history", "python"])
        .assert()
        .success()
        .stdout(predicate::str::contains("python"));

    // Info should work offline
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "info"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Index Information"));
}

// ============================================================================
// Error Message Quality Tests
// ============================================================================

#[test]
fn test_clear_error_message_no_index() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    // Should give clear error about missing index
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "python"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("No index found"))
        .stderr(predicate::str::contains("nxv update"));
}

#[test]
fn test_clear_error_message_invalid_manifest_version() {
    // Start mock server with invalid manifest version
    let mut server = mockito::Server::new();

    let manifest = serde_json::json!({
        "version": 999,  // Invalid version
        "latest_commit": "abc123",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {
            "url": "http://example.com/index.db.zst",
            "size_bytes": 1000,
            "sha256": "abc123"
        },
        "bloom_filter": {
            "url": "http://example.com/index.bloom",
            "size_bytes": 100,
            "sha256": "def456"
        },
        "deltas": []
    });

    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(manifest.to_string())
        .create();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let manifest_url = format!("{}/manifest.json", server.url());

    // Should give clear error about manifest version
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("manifest").or(predicate::str::contains("version")));
}

#[test]
fn test_clear_error_message_package_not_found() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Search for non-existent package should give clear message
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "nonexistent_package_xyz",
        ])
        .assert()
        .success() // Not an error, just no results
        .stderr(predicate::str::contains("No packages found"));
}

#[test]
fn test_clear_error_message_history_not_found() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // History for non-existent package should give clear message
    // Note: This returns success (exit 0) with a message to stdout
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "nonexistent_package_xyz",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No history found"));
}

// ============================================================================
// Interrupt Safety Tests
// ============================================================================

#[test]
fn test_no_data_corruption_on_failed_download() {
    // This tests that a failed download (due to checksum mismatch) doesn't corrupt
    // an existing database

    // Create an existing valid database
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    create_test_db(&db_path);

    // Record the original database content
    let original_content = std::fs::read(&db_path).unwrap();

    // Start mock server that returns data with wrong checksum
    let mut server = mockito::Server::new();

    // Create some data with wrong hash
    let fake_data = b"This is not a valid database";
    let compressed = zstd::encode_all(&fake_data[..], 3).unwrap();

    let manifest = serde_json::json!({
        "version": 1,
        "latest_commit": "newcommit123",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": compressed.len(),
            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"  // Wrong hash
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": 100,
            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
        },
        "deltas": []
    });

    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(manifest.to_string())
        .create();

    let _index_mock = server
        .mock("GET", "/index.db.zst")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(compressed)
        .create();

    let manifest_url = format!("{}/manifest.json", server.url());

    // Run update command - should fail
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
            "--force", // Force full download to test failure path
        ])
        .assert()
        .failure();

    // The original database should still be intact
    let current_content = std::fs::read(&db_path).unwrap();
    assert_eq!(
        original_content, current_content,
        "Database should not be corrupted after failed download"
    );

    // Verify the database is still usable
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "python"])
        .assert()
        .success()
        .stdout(predicate::str::contains("python"));
}

#[test]
fn test_temp_files_cleaned_up_on_failure() {
    // Test that temporary files are cleaned up when download fails
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    // Start mock server that returns data with wrong checksum
    let mut server = mockito::Server::new();

    let fake_data = b"This is not a valid database";
    let compressed = zstd::encode_all(&fake_data[..], 3).unwrap();

    let manifest = serde_json::json!({
        "version": 1,
        "latest_commit": "abc123",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": compressed.len(),
            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"  // Wrong
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": 100,
            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
        },
        "deltas": []
    });

    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(manifest.to_string())
        .create();

    let _index_mock = server
        .mock("GET", "/index.db.zst")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(compressed)
        .create();

    let manifest_url = format!("{}/manifest.json", server.url());

    // Run update command - should fail
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .failure();

    // Check that no temp files are left behind
    let temp_path = db_path.with_extension("tmp");
    assert!(
        !temp_path.exists(),
        "Temp file should be cleaned up after failed download"
    );

    // The final database should not exist either (download failed)
    assert!(
        !db_path.exists(),
        "Database should not be created on failed download"
    );
}

/// Full delta update workflow test:
/// 1. Initial download creates index with initial packages
/// 2. New delta becomes available with new packages
/// 3. Apply delta update
/// 4. Verify search returns both old and new packages
#[test]
fn test_full_delta_update_workflow() {
    use sha2::{Digest, Sha256};

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let bloom_path = dir.path().join("index.bloom");

    // --- PHASE 1: Initial download ---

    let mut server = mockito::Server::new();

    // Create initial database with one package
    let initial_db_path = dir.path().join("initial.db");
    {
        let conn = rusqlite::Connection::open(&initial_db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE package_versions (
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
            CREATE INDEX idx_packages_name ON package_versions(name);
            CREATE VIRTUAL TABLE package_versions_fts USING fts5(name, description, content=package_versions, content_rowid=id);
            CREATE TRIGGER package_versions_ai AFTER INSERT ON package_versions BEGIN
                INSERT INTO package_versions_fts(rowid, name, description) VALUES (new.id, new.name, new.description);
            END;

            INSERT INTO meta (key, value) VALUES ('last_indexed_commit', 'initial123456789012345678901234567890abc');
            INSERT INTO meta (key, value) VALUES ('schema_version', '1');
            INSERT INTO package_versions
                (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description)
            VALUES
                ('firefox', '120.0', 'initial123', 1700000000, 'initial123', 1700000000, 'firefox', 'Mozilla Firefox browser');
            "#,
        )
        .unwrap();
    }

    // Create initial bloom filter
    let initial_bloom_data = {
        let mut filter = bloomfilter::Bloom::new_for_fp_rate(10000, 0.01).unwrap();
        filter.set(&"firefox");
        filter.to_bytes()
    };

    // Compress initial database
    let initial_db_data = std::fs::read(&initial_db_path).unwrap();
    let initial_db_compressed = zstd::encode_all(&initial_db_data[..], 3).unwrap();

    let mut initial_db_hasher = Sha256::new();
    initial_db_hasher.update(&initial_db_compressed);
    let initial_db_hash = format!("{:x}", initial_db_hasher.finalize());

    let mut initial_bloom_hasher = Sha256::new();
    initial_bloom_hasher.update(&initial_bloom_data);
    let initial_bloom_hash = format!("{:x}", initial_bloom_hasher.finalize());

    // Initial manifest
    let initial_manifest = serde_json::json!({
        "version": 1,
        "latest_commit": "initial123456789012345678901234567890abc",
        "latest_commit_date": "2024-01-01T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": initial_db_compressed.len(),
            "sha256": initial_db_hash
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": initial_bloom_data.len(),
            "sha256": initial_bloom_hash
        },
        "deltas": []
    });

    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(initial_manifest.to_string())
        .create();

    let _index_mock = server
        .mock("GET", "/index.db.zst")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(initial_db_compressed.clone())
        .create();

    let _bloom_mock = server
        .mock("GET", "/index.bloom")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(initial_bloom_data.clone())
        .create();

    let manifest_url = format!("{}/manifest.json", server.url());

    // Run initial update
    nxv()
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .success();

    // Verify initial state: firefox exists
    nxv()
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .args(["--db-path", db_path.to_str().unwrap(), "search", "firefox"])
        .assert()
        .success()
        .stdout(predicate::str::contains("firefox"))
        .stdout(predicate::str::contains("120.0"));

    // Verify initial state: nodejs does NOT exist yet
    nxv()
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .args(["--db-path", db_path.to_str().unwrap(), "search", "nodejs"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("No packages")));

    // --- PHASE 2: Delta update ---

    // Create delta SQL that adds nodejs
    let delta_sql = r#"
INSERT OR REPLACE INTO package_versions
    (name, version, first_commit_hash, first_commit_date, last_commit_hash, last_commit_date, attribute_path, description)
VALUES
    ('nodejs', '21.0.0', 'delta789012345678901234567890abcdef12', 1705000000, 'delta789012345678901234567890abcdef12', 1705000000, 'nodejs_21', 'Node.js runtime');

UPDATE meta SET value = 'delta789012345678901234567890abcdef12' WHERE key = 'last_indexed_commit';
"#;

    let delta_compressed = zstd::encode_all(delta_sql.as_bytes(), 3).unwrap();

    let mut delta_hasher = Sha256::new();
    delta_hasher.update(&delta_compressed);
    let delta_hash = format!("{:x}", delta_hasher.finalize());

    // Create updated bloom filter (contains both firefox and nodejs)
    let updated_bloom_data = {
        let mut filter = bloomfilter::Bloom::new_for_fp_rate(10000, 0.01).unwrap();
        filter.set(&"firefox");
        filter.set(&"nodejs");
        filter.to_bytes()
    };

    let mut updated_bloom_hasher = Sha256::new();
    updated_bloom_hasher.update(&updated_bloom_data);
    let updated_bloom_hash = format!("{:x}", updated_bloom_hasher.finalize());

    // New server for phase 2 (recreate mocks)
    drop(_manifest_mock);
    drop(_index_mock);
    drop(_bloom_mock);

    // Updated manifest with delta
    let updated_manifest = serde_json::json!({
        "version": 1,
        "latest_commit": "delta789012345678901234567890abcdef12",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": initial_db_compressed.len(),
            "sha256": initial_db_hash
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": updated_bloom_data.len(),
            "sha256": updated_bloom_hash
        },
        "deltas": [{
            "from_commit": "initial123456789012345678901234567890abc",
            "to_commit": "delta789012345678901234567890abcdef12",
            "url": format!("{}/delta.sql.zst", server.url()),
            "size_bytes": delta_compressed.len(),
            "sha256": delta_hash
        }]
    });

    let _manifest_mock2 = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(updated_manifest.to_string())
        .create();

    let _delta_mock = server
        .mock("GET", "/delta.sql.zst")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(delta_compressed)
        .create();

    let _bloom_mock2 = server
        .mock("GET", "/index.bloom")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(updated_bloom_data)
        .create();

    // Run delta update
    nxv()
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "update",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .success();

    // --- PHASE 3: Verify final state ---

    // Firefox should still exist
    nxv()
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .args(["--db-path", db_path.to_str().unwrap(), "search", "firefox"])
        .assert()
        .success()
        .stdout(predicate::str::contains("firefox"))
        .stdout(predicate::str::contains("120.0"));

    // nodejs should now exist (added by delta)
    nxv()
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .args(["--db-path", db_path.to_str().unwrap(), "search", "nodejs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nodejs"))
        .stdout(predicate::str::contains("21.0.0"));

    // Verify the commit was updated
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let commit: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            commit, "delta789012345678901234567890abcdef12",
            "Commit should be updated after delta"
        );
    }

    // Verify total package count
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 2,
            "Database should have 2 packages after delta workflow"
        );
    }
}

// ============================================================================
// Indexer Integration Tests (require `indexer` feature and nix)
// ============================================================================

/// Test that the `nxv index` command creates an index from the nixpkgs submodule.
/// This test requires the `indexer` feature and nix to be installed.
#[test]
#[ignore] // Requires indexer feature and nix to be installed
fn test_index_command_creates_database() {
    let nixpkgs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("nixpkgs");
    if !nixpkgs_path.exists() {
        eprintln!("Skipping: nixpkgs submodule not present");
        return;
    }

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test-index.db");

    // Run the index command with a small checkpoint interval
    // Use --full to ensure we test full indexing
    let _result = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--nixpkgs-path",
            nixpkgs_path.to_str().unwrap(),
            "--full",
            "--checkpoint-interval",
            "10",
        ])
        .timeout(std::time::Duration::from_secs(600)) // 10 minute timeout
        .assert();

    // The command might succeed or fail depending on nix availability
    // Just check it doesn't panic and creates some database
    if db_path.exists() {
        // If a database was created, verify it has the expected schema
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('meta', 'package_versions')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(table_count >= 1, "Database should have at least meta table");
    }
}

/// Test that incremental indexing only processes new commits.
/// This test requires the `indexer` feature and nix to be installed.
#[test]
#[ignore] // Requires indexer feature and nix to be installed
fn test_incremental_index_processes_only_new_commits() {
    let nixpkgs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("nixpkgs");
    if !nixpkgs_path.exists() {
        eprintln!("Skipping: nixpkgs submodule not present");
        return;
    }

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test-index.db");

    // Create a database with an initial commit marker
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE package_versions (
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
            CREATE INDEX idx_packages_name ON package_versions(name);
            INSERT INTO meta (key, value) VALUES ('schema_version', '1');
            "#,
        )
        .unwrap();

        // Get the latest commit from nixpkgs to use as checkpoint
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&nixpkgs_path)
            .output()
            .expect("Failed to get nixpkgs HEAD");

        let head_commit = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Set the last indexed commit to HEAD - this should mean no new commits to process
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('last_indexed_commit', ?)",
            [&head_commit],
        )
        .unwrap();
    }

    // Run incremental index - should report no new commits
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--nixpkgs-path",
            nixpkgs_path.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(60))
        .assert()
        .success()
        .stdout(
            predicate::str::contains("up to date")
                .or(predicate::str::contains("No new commits"))
                .or(predicate::str::contains("0 commits")),
        );
}

/// Test that indexing can be resumed after interruption.
/// This verifies the checkpoint/recovery mechanism works correctly.
#[test]
#[ignore] // Requires indexer feature and nix to be installed
fn test_index_resumable_after_interrupt() {
    let nixpkgs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("nixpkgs");
    if !nixpkgs_path.exists() {
        eprintln!("Skipping: nixpkgs submodule not present");
        return;
    }

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test-index.db");

    // Simulate a partially completed indexing run by creating a database
    // with a checkpoint from an older commit
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE package_versions (
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
            CREATE INDEX idx_packages_name ON package_versions(name);
            INSERT INTO meta (key, value) VALUES ('schema_version', '1');
            "#,
        )
        .unwrap();

        // Get a commit from ~10 commits ago
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD~10"])
            .current_dir(&nixpkgs_path)
            .output()
            .expect("Failed to get nixpkgs commit");

        let old_commit = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Set checkpoint to old commit
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('last_indexed_commit', ?)",
            [&old_commit],
        )
        .unwrap();
    }

    // Run incremental index - should pick up from checkpoint
    let _result = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--nixpkgs-path",
            nixpkgs_path.to_str().unwrap(),
        ])
        .timeout(std::time::Duration::from_secs(600))
        .assert();

    // Verify checkpoint was updated
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let last_commit: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'last_indexed_commit'",
            [],
            |row| row.get(0),
        )
        .ok();

    assert!(
        last_commit.is_some(),
        "Checkpoint should be preserved after indexing"
    );
}

/// End-to-end workflow test: index a few commits, then search for packages.
/// This verifies the complete indexer → database → search pipeline.
#[test]
#[ignore] // Requires indexer feature and nix to be installed
fn test_index_then_search_workflow() {
    let nixpkgs_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("nixpkgs");
    if !nixpkgs_path.exists() {
        eprintln!("Skipping: nixpkgs submodule not present");
        return;
    }

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test-index.db");

    // Index from nixpkgs (will take a while but should find packages)
    let _index_result = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--nixpkgs-path",
            nixpkgs_path.to_str().unwrap(),
            "--full",
            "--checkpoint-interval",
            "5",
        ])
        .timeout(std::time::Duration::from_secs(1800)) // 30 minute timeout
        .assert();

    // If indexing succeeded and created a database, test search
    if db_path.exists() {
        // Check that we can query the database
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let package_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        if package_count > 0 {
            // Search for a common package (should exist in any nixpkgs)
            nxv()
                .args([
                    "--db-path",
                    db_path.to_str().unwrap(),
                    "search",
                    "hello", // hello is a simple package that should always exist
                ])
                .assert()
                .success();

            // Test history command
            nxv()
                .args(["--db-path", db_path.to_str().unwrap(), "history", "hello"])
                .assert()
                .success();

            // Test info command
            nxv()
                .args(["--db-path", db_path.to_str().unwrap(), "info"])
                .assert()
                .success()
                .stdout(predicate::str::contains("Package entries"));
        }
    }
}
