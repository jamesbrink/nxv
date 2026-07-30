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

#[cfg(unix)]
fn install_fake_nix_commands(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let script = r#"#!/bin/sh
{
  printf 'program=%s\n' "$(basename "$0")"
  printf 'allow_insecure=%s\n' "${NIXPKGS_ALLOW_INSECURE:-}"
  printf 'arg=%s\n' "$@"
} > "$NXV_TEST_CAPTURE"
exit "${NXV_TEST_EXIT_CODE:-0}"
"#;

    for program in ["nix", "nix-shell"] {
        let path = dir.join(program);
        std::fs::write(&path, script).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }
}

#[cfg(unix)]
fn prepend_path(dir: &std::path::Path) -> std::ffi::OsString {
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).unwrap()
}

/// Creates a SQLite test database at the given path populated with a schema and sample package rows.
///
/// The database will contain `meta` and `package_versions` tables, relevant indexes,
/// an FTS5 virtual table for full-text search, and sample package/version entries
/// (e.g., Python, Node.js, Firefox, rustc) useful for integration tests.
///
/// # Examples
///
/// ```
/// use tempfile::tempdir;
/// let tmp = tempdir().unwrap();
/// let db_path = tmp.path().join("test.db");
/// create_test_db(&db_path);
/// assert!(db_path.exists());
/// ```
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
            known_vulnerabilities TEXT,
            UNIQUE(attribute_path, version, first_commit_hash)
        );

        CREATE INDEX IF NOT EXISTS idx_packages_name ON package_versions(name);
        CREATE INDEX IF NOT EXISTS idx_packages_name_version ON package_versions(name, version, first_commit_date);
        CREATE INDEX IF NOT EXISTS idx_packages_attr ON package_versions(attribute_path);
        -- Match the production schema so higher-level search tests exercise the
        -- covering candidate index rather than only the scan fallback.
        CREATE INDEX IF NOT EXISTS idx_packages_search_nocase ON package_versions(
            attribute_path COLLATE NOCASE,
            version COLLATE NOCASE,
            (LENGTH(attribute_path) - LENGTH(REPLACE(attribute_path, '.', ''))),
            last_commit_date DESC,
            first_commit_date DESC
        );
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
            ('python-3.11.0', '3.11.0', 'abc1234567890', 1700000000, 'def1234567890000000000000000000000000000', 1700100000,
             'python', 'Python programming language', '["MIT"]', 'https://python.org'),
            ('python-3.11.0', '3.11.0', 'abc1234567890', 1700000000, 'def1234567890000000000000000000000000000', 1700100000,
             'python311', 'Python programming language', '["MIT"]', 'https://python.org'),
            ('python-3.12.0', '3.12.0', 'ghi1234567890', 1701000000, 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1', 1701100000,
             'python312', 'Python programming language', '["MIT"]', 'https://python.org'),
            ('python2-2.7.18', '2.7.18', 'mno1234567890', 1600000000, 'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2', 1600100000,
             'python2', 'Python 2 interpreter', '["PSF"]', 'https://python.org'),
            ('nodejs-20.0.0', '20.0.0', 'stu1234567890', 1702000000, 'c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3', 1702100000,
             'nodejs', 'Node.js JavaScript runtime', '["MIT"]', 'https://nodejs.org'),
            ('jq-1.7.1', '1.7.1', 'eee1234567890', 1702000000, 'e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4', 1702100000,
             'jq', 'Command-line JSON processor', '["MIT"]', 'https://jqlang.github.io/jq/'),
            ('jquake-1.8.5', '1.8.5', 'fff1234567890', 1705000000, 'f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5f5', 1705100000,
             'jquake', 'A newer prefix sibling', '["MIT"]', 'https://example.com/jquake'),
            ('firefox-120.0', '120.0', 'aaa1234567890', 1703000000, 'bbb1234567890000000000000000000000000000', 1703100000,
             'firefox', 'Mozilla Firefox web browser', '["MPL-2.0"]', 'https://firefox.com'),
            ('rustc-1.75.0', '1.75.0', 'ccc1234567890', 1704000000, 'ddd1234567890000000000000000000000000000', 1704100000,
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
        .stdout(predicate::str::contains("Nix Version Index"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("update"))
        .stdout(predicate::str::contains("sync"))
        .stdout(predicate::str::contains("info"))
        .stdout(predicate::str::contains("stats"))
        .stdout(predicate::str::contains("history"))
        .stdout(predicate::str::contains("completions"));
}

// ============================================================================
// Run Command Tests
// ============================================================================

#[test]
fn test_run_help_describes_shell_and_multi_package_options() {
    nxv()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Open a shell"))
        .stdout(predicate::str::contains("--version"))
        .stdout(predicate::str::contains("--with"))
        .stdout(predicate::str::contains("--all-depths"));
}

#[cfg(unix)]
#[test]
fn test_run_launches_one_modern_shell_with_multiple_packages() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bin_path = dir.path().join("bin");
    let capture_path = dir.path().join("capture.txt");
    create_test_db(&db_path);
    std::fs::create_dir(&bin_path).unwrap();
    install_fake_nix_commands(&bin_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "run",
            "python",
            "3.11",
            "--with",
            "nodejs@20",
        ])
        .env("PATH", prepend_path(&bin_path))
        .env("NXV_TEST_CAPTURE", &capture_path)
        .assert()
        .success()
        .stderr(
            predicate::str::contains("Resolved python@3.11 -> python 3.11.0").and(
                predicate::str::contains("Resolved nodejs@20 -> nodejs 20.0.0"),
            ),
        );

    let capture = std::fs::read_to_string(capture_path).unwrap();
    assert!(capture.contains("program=nix\n"));
    assert!(capture.contains("arg=shell\n"));
    assert!(capture.contains("arg=nixpkgs/def1234567890000000000000000000000000000#python\n"));
    assert!(capture.contains("arg=nixpkgs/c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3#nodejs\n"));
}

#[cfg(unix)]
#[test]
fn test_run_prefers_an_exact_attribute_over_newer_prefix_siblings() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bin_path = dir.path().join("bin");
    let capture_path = dir.path().join("capture.txt");
    create_test_db(&db_path);
    std::fs::create_dir(&bin_path).unwrap();
    install_fake_nix_commands(&bin_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "run",
            "python",
            "3.11",
            "--with",
            "jq",
        ])
        .env("PATH", prepend_path(&bin_path))
        .env("NXV_TEST_CAPTURE", &capture_path)
        .assert()
        .success()
        .stderr(predicate::str::contains("Resolved jq -> jq 1.7.1"));

    let capture = std::fs::read_to_string(capture_path).unwrap();
    assert!(capture.contains("arg=nixpkgs/e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4e4#jq\n"));
    assert!(!capture.contains("jquake"));
}

#[cfg(unix)]
#[test]
fn test_run_resolves_through_remote_backend() {
    use mockito::Matcher;

    let dir = tempdir().unwrap();
    let bin_path = dir.path().join("bin");
    let capture_path = dir.path().join("capture.txt");
    std::fs::create_dir(&bin_path).unwrap();
    install_fake_nix_commands(&bin_path);

    let mut server = mockito::Server::new();
    let response = serde_json::json!({
        "data": [{
            "id": 1,
            "name": "python3",
            "version": "3.11.9",
            "first_commit_hash": "remote-first",
            "first_commit_date": "2024-01-01T00:00:00Z",
            "last_commit_hash": "0123456789abcdef0123456789abcdef01234567",
            "last_commit_date": "2024-02-01T00:00:00Z",
            "attribute_path": "python311",
            "description": "Python",
            "license": ["Python-2.0"],
            "homepage": "https://python.org",
            "maintainers": [],
            "platforms": ["x86_64-linux"],
            "source_path": null,
            "known_vulnerabilities": null
        }],
        "meta": {
            "total": 1,
            "limit": 1,
            "offset": 0,
            "has_more": false
        }
    });
    let _search_mock = server
        .mock("GET", "/api/v1/search")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("q".into(), "python".into()),
            Matcher::UrlEncoded("version".into(), "3.11".into()),
            Matcher::UrlEncoded("limit".into(), "1".into()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(response.to_string())
        .create();

    nxv()
        .args(["run", "python", "3.11"])
        .env("NXV_API_URL", server.url())
        .env("PATH", prepend_path(&bin_path))
        .env("NXV_TEST_CAPTURE", &capture_path)
        .assert()
        .success();

    let capture = std::fs::read_to_string(capture_path).unwrap();
    assert!(capture.contains("program=nix\n"));
    assert!(capture.contains("arg=nixpkgs/0123456789abcdef0123456789abcdef01234567#python311\n"));
}

#[cfg(unix)]
#[test]
fn test_run_uses_one_legacy_shell_for_mixed_eras() {
    use rusqlite::Connection;

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bin_path = dir.path().join("bin");
    let capture_path = dir.path().join("capture.txt");
    create_test_db(&db_path);
    Connection::open(&db_path)
        .unwrap()
        .execute(
            "INSERT INTO package_versions
             (name, version, first_commit_hash, first_commit_date, last_commit_hash,
              last_commit_date, attribute_path, description)
             VALUES ('legacy-1.0', '1.0', 'old-first', 1500000000, 'fedcba9876543210fedcba9876543210fedcba98',
                     1500000100, 'legacy', 'Legacy test package')",
            [],
        )
        .unwrap();
    std::fs::create_dir(&bin_path).unwrap();
    install_fake_nix_commands(&bin_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "run",
            "legacy",
            "1.0",
            "--exact",
            "--with",
            "nodejs@20",
        ])
        .env("PATH", prepend_path(&bin_path))
        .env("NXV_TEST_CAPTURE", &capture_path)
        .assert()
        .success();

    let capture = std::fs::read_to_string(capture_path).unwrap();
    assert!(capture.contains("program=nix-shell\n"));
    assert!(capture.contains("arg=-p\n"));
    assert!(capture.contains("archive/fedcba9876543210fedcba9876543210fedcba98.tar.gz"));
    assert!(capture.contains("archive/c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3.tar.gz"));
}

#[cfg(unix)]
#[test]
fn test_run_resolves_all_packages_before_launching() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bin_path = dir.path().join("bin");
    let capture_path = dir.path().join("capture.txt");
    create_test_db(&db_path);
    std::fs::create_dir(&bin_path).unwrap();
    install_fake_nix_commands(&bin_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "run",
            "python",
            "--with",
            "definitely-missing",
        ])
        .env("PATH", prepend_path(&bin_path))
        .env("NXV_TEST_CAPTURE", &capture_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unable to resolve package specification `definitely-missing`",
        ));

    assert!(
        !capture_path.exists(),
        "Nix launched before all queries resolved"
    );
}

#[cfg(unix)]
#[test]
fn test_run_propagates_shell_exit_status_and_insecure_environment() {
    use rusqlite::Connection;

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bin_path = dir.path().join("bin");
    let capture_path = dir.path().join("capture.txt");
    create_test_db(&db_path);
    Connection::open(&db_path)
        .unwrap()
        .execute(
            "UPDATE package_versions
             SET known_vulnerabilities = '[\"CVE-test\"]'
             WHERE attribute_path = 'firefox'",
            [],
        )
        .unwrap();
    std::fs::create_dir(&bin_path).unwrap();
    install_fake_nix_commands(&bin_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "run",
            "firefox",
            "--exact",
        ])
        .env("PATH", prepend_path(&bin_path))
        .env("NXV_TEST_CAPTURE", &capture_path)
        .env("NXV_TEST_EXIT_CODE", "42")
        .assert()
        .code(42)
        .stderr(predicate::str::contains("known vulnerabilities"));

    let capture = std::fs::read_to_string(capture_path).unwrap();
    assert!(capture.contains("allow_insecure=1\n"));
    assert!(capture.contains("arg=--impure\n"));
}

#[cfg(unix)]
#[test]
fn test_run_reports_actionable_error_when_nix_is_unavailable() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let empty_path = dir.path().join("empty-bin");
    create_test_db(&db_path);
    std::fs::create_dir(&empty_path).unwrap();

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "run",
            "python311",
            "3.11",
            "--exact",
        ])
        .env("PATH", &empty_path)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("`nix`")
                .and(predicate::str::contains("not found on PATH"))
                .and(predicate::str::contains("install Nix")),
        );
}

#[cfg(unix)]
#[test]
fn test_run_reports_actionable_error_when_nix_shell_is_unavailable() {
    use rusqlite::Connection;

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let empty_path = dir.path().join("empty-bin");
    create_test_db(&db_path);
    Connection::open(&db_path)
        .unwrap()
        .execute(
            "INSERT INTO package_versions
             (name, version, first_commit_hash, first_commit_date, last_commit_hash,
              last_commit_date, attribute_path, description)
             VALUES ('legacy-1.0', '1.0', 'old-first', 1500000000,
                     'fedcba9876543210fedcba9876543210fedcba98',
                     1500000100, 'legacy', 'Legacy test package')",
            [],
        )
        .unwrap();
    std::fs::create_dir(&empty_path).unwrap();

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "run",
            "legacy",
            "1.0",
            "--exact",
        ])
        .env("PATH", &empty_path)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("`nix-shell`")
                .and(predicate::str::contains("not found on PATH"))
                .and(predicate::str::contains("install Nix")),
        );
}

#[test]
fn test_update_help_is_application_only() {
    nxv()
        .args(["update", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("latest application release"))
        .stdout(predicate::str::contains("nxv sync"))
        .stdout(predicate::str::contains("--force").not());
}

#[test]
fn test_sync_help_owns_index_options() {
    nxv()
        .args(["sync", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("package index"))
        .stdout(predicate::str::contains("--force"))
        .stdout(predicate::str::contains("--manifest-url"))
        .stdout(predicate::str::contains("--skip-verify"))
        .stdout(predicate::str::contains("--public-key"));
}

#[test]
fn test_update_rejects_legacy_index_flags_with_sync_guidance() {
    nxv()
        .args(["update", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nxv sync"));
}

#[test]
fn test_update_rejects_retired_no_self_update_environment() {
    nxv()
        .arg("update")
        .env("NXV_NO_SELF_UPDATE", "1")
        .assert()
        .failure()
        .stderr(predicate::str::contains("NXV_NO_SELF_UPDATE is retired"))
        .stderr(predicate::str::contains("nxv sync"));
}

#[test]
fn test_update_checks_application_release_without_touching_index() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    std::fs::write(&db_path, b"index sentinel").unwrap();

    let mut server = mockito::Server::new();
    let release = serde_json::json!({
        "tag_name": format!("v{}", env!("CARGO_PKG_VERSION")),
        "assets": []
    });
    let _release_mock = server
        .mock("GET", "/repos/utensils/nxv/releases/latest")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(release.to_string())
        .create();

    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "update"])
        .env("NXV_GITHUB_API_BASE", server.url())
        .env("NXV_NO_SELF_UPDATE", "0")
        .assert()
        .success()
        .stderr(predicate::str::contains("Already up to date"));

    assert_eq!(std::fs::read(&db_path).unwrap(), b"index sentinel");
}

#[test]
fn test_version_displays() {
    nxv()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("nxv"))
        // Version should contain semantic version from Cargo.toml
        .stdout(predicate::str::is_match(r"\d+\.\d+\.\d+").unwrap());
}

#[test]
fn test_version_format() {
    // Test that --version output has expected format
    let output = nxv().arg("--version").output().expect("Failed to run nxv");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Must contain "nxv" and a version number
    assert!(
        stdout.contains("nxv"),
        "Version output should contain 'nxv'"
    );
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "Version output should contain package version from Cargo.toml"
    );

    // If NXV_GIT_REV is set (Nix builds), version should include git rev in parens
    // Format: "nxv X.Y.Z (abc1234)"
    // When not set (cargo builds), format is just: "nxv X.Y.Z"
    // Both are valid, so we just verify the base format works
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
        .stdout(predicate::str::contains("--all-depths"))
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

/// Regression for #52: `--exact` used to be silently dropped whenever a
/// version filter was present, so `nxv search python 3.11 --exact` returned
/// `python311` too. Both invocation styles (positional and `-V`) are covered
/// because they feed the same `SearchOptions`.
#[test]
fn test_search_exact_with_version_filter_restricts_to_exact_attr() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bloom_path = dir.path().join("nonexistent.bloom");
    create_test_db(&db_path);

    // `python` and `python311` both carry 3.11.0; --exact must keep only `python`.
    for version_args in [vec!["3.11"], vec!["-V", "3.11"]] {
        let mut args = vec![
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--exact",
            "--format",
            "json",
            "--limit",
            "0",
        ];
        args.extend(version_args.iter().copied());

        let output = nxv()
            .args(&args)
            .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        let results: Vec<serde_json::Value> =
            serde_json::from_slice(&output).expect("search --format json must emit a JSON array");

        assert!(
            !results.is_empty(),
            "expected the exact `python` 3.11 row, got nothing ({version_args:?})"
        );
        for r in &results {
            assert_eq!(
                r["attribute_path"], "python",
                "--exact leaked a non-exact attr with a version filter ({version_args:?})"
            );
        }
    }
}

/// The complementary case from the issue: `python` was never at 2.7.x, so an
/// exact search must return zero rows rather than the `python2` sibling.
#[test]
fn test_search_exact_with_version_filter_returns_empty_when_unmatched() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let bloom_path = dir.path().join("nonexistent.bloom");
    create_test_db(&db_path);

    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "2.7",
            "--exact",
            "--format",
            "json",
            "--limit",
            "0",
        ])
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).expect("stdout must be UTF-8");

    // An empty result set writes nothing to stdout and reports "No packages
    // found" on stderr instead of emitting `[]` — pre-existing behavior,
    // unrelated to #52. Accept an empty stdout or an empty array, never a
    // populated one.
    if !stdout.trim().is_empty() {
        let results: Vec<serde_json::Value> = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("non-empty stdout must be a JSON array ({e}): {stdout}"));
        assert!(
            results.is_empty(),
            "python was never at 2.7.x; got {results:?}"
        );
    }

    // Belt and braces: the `python2` sibling does hold 2.7.18, so it would be
    // returned by the buggy prefix path. It must appear in neither stream.
    let stderr = String::from_utf8(output.stderr).expect("stderr must be UTF-8");
    assert!(
        !stdout.contains("python2") && !stderr.contains("python2"),
        "--exact leaked the `python2` sibling:\nstdout: {stdout}\nstderr: {stderr}"
    );
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
fn test_version_search_scopes_by_default_and_all_depths_preserves_legacy_search() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO package_versions \
         (name, version, first_commit_hash, first_commit_date, last_commit_hash, \
          last_commit_date, attribute_path, description) \
         VALUES ('icontract', '2.7.3', 'new-first', 1800000000, 'new-last', \
                 1800100000, 'python314Packages.icontract', 'Python library')",
        [],
    )
    .unwrap();
    drop(conn);

    let run = |extra: &[&str]| {
        let mut args = vec![
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "2.7",
            "--format",
            "json",
            "--limit",
            "0",
        ];
        args.extend_from_slice(extra);
        let output = nxv()
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<Vec<serde_json::Value>>(&output).unwrap()
    };

    let relevant = run(&[]);
    assert_eq!(relevant[0]["attribute_path"], "python2");
    assert_eq!(relevant.len(), 1);

    let by_date = run(&["--sort", "date"]);
    assert_eq!(by_date[0]["attribute_path"], "python2");
    assert_eq!(by_date.len(), 1);

    let all_depths = run(&["--all-depths", "--sort", "date"]);
    assert_eq!(
        all_depths[0]["attribute_path"],
        "python314Packages.icontract"
    );
    assert_eq!(all_depths[1]["attribute_path"], "python2");
}

#[test]
fn test_version_miss_explains_scope_and_suggests_nearby_versions() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO package_versions \
         (name, version, first_commit_hash, first_commit_date, last_commit_hash, \
          last_commit_date, attribute_path, description) \
         VALUES ('itables', '2.7.3', 'new-first', 1800000000, 'new-last', \
                 1800100000, 'python314Packages.itables', 'Python library')",
        [],
    )
    .unwrap();
    drop(conn);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "2.7.3",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "No direct attribute matching 'python' has version prefix '2.7.3'",
        ))
        .stderr(predicate::str::contains("python2 2.7.18"))
        .stderr(predicate::str::contains(
            "Nested matches exist. Re-run with --all-depths",
        ));

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "2.7.3",
            "--all-depths",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("python314Packages.itables"));
}

#[test]
fn test_all_depths_requires_version_and_conflicts_with_exact() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "--all-depths",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--all-depths requires a version filter",
        ));

    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "python",
            "2.7",
            "--all-depths",
            "--exact",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with '--exact'"));
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

/// The JSON row shape documented in `src/skill/SKILL.md`. `license`,
/// `maintainers`, `platforms` and `known_vulnerabilities` are stored in SQLite
/// as JSON-array *strings*; that storage detail must never reach the wire
/// (issue #54), so they serialize as real arrays or `null`.
#[test]
fn test_search_json_array_fields_are_not_stringified() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "rustc",
            "--exact",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let row = &parsed.as_array().unwrap()[0];

    assert_eq!(
        row.get("license").unwrap(),
        &serde_json::json!(["MIT", "Apache-2.0"]),
        "multi-element license must be a real array, not a stringified one"
    );

    // Columns the fixture leaves NULL stay null rather than becoming `[]`.
    for field in ["maintainers", "platforms", "known_vulnerabilities"] {
        assert!(
            row.get(field).unwrap().is_null(),
            "{field} should be null when the column is NULL"
        );
    }

    // Scalar fields are unaffected by the array treatment.
    assert_eq!(row.get("attribute_path").unwrap(), "rustc");
    assert_eq!(row.get("homepage").unwrap(), "https://rust-lang.org");
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
// Stats Command Tests
// ============================================================================

#[test]
fn test_stats_with_database() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "stats"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Index Information"))
        .stdout(predicate::str::contains("Statistics"))
        .stdout(predicate::str::contains("Database path"));
}

#[test]
fn test_stats_no_database() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "stats"])
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

    // Regression for #33: --format json must produce pure JSON on stdout
    // (no human-readable "Version history for: ..." header).
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
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Output should be valid JSON with no header");
    assert!(parsed.is_array(), "JSON output should be an array");
    assert!(
        !stdout.contains("Version history for:"),
        "JSON output must not contain human-readable header"
    );
}

#[test]
fn test_history_full_json_format() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Regression for #33: --full --format json must also produce pure JSON.
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "python",
            "--full",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("--full --format json should be pure JSON");
    assert!(parsed.is_array());
    assert!(!stdout.contains("Version history for:"));
}

#[test]
fn test_history_full_resolves_exact_attribute_path() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Regression for #55 and #64: the installable attribute path can differ
    // from the upstream derivation name. `--full` must accept the same
    // attribute-path identifier as the compact history view.
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "python312",
            "--full",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&stdout).expect("--full --format json should be pure JSON");
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row["attribute_path"] == "python312"));
    assert!(rows.iter().any(|row| row["name"] == "python-3.12.0"));
}

#[test]
fn test_history_specific_version_json() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Regression for #33: history with a specific version + --format json
    // must produce pure JSON (was previously printing "Package: foo X.Y" first).
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "python",
            "3.11.0",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Specific-version JSON should parse cleanly");
    assert!(parsed.is_array());
    assert!(!stdout.contains("Package:"));
    assert!(!stdout.contains("To use this version:"));
}

#[test]
fn test_history_not_found_json() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Regression for #33: not-found path with --format json must still
    // produce parseable JSON (an empty array), not the human-readable
    // "No history found" message that breaks downstream `jq`.
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "nonexistent_package",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Not-found JSON should be an empty array");
    assert_eq!(parsed.as_array().map(|a| a.len()), Some(0));
    assert!(!stdout.contains("No history found"));
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

#[test]
fn test_info_json_pure() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Regression for #33: `nxv info ... --format json` must be pure JSON.
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "info",
            "python",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("info --format json should be pure JSON");
    assert!(parsed.is_array());
}

#[test]
fn test_info_not_found_json() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Regression for #33: not-found path on `info` with --format json must
    // emit `[]`, not the human-readable "Package 'X' not found." message.
    let output = nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "info",
            "nonexistent_package_xyz",
            "--format",
            "json",
        ])
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Not-found JSON should be an empty array");
    assert_eq!(parsed.as_array().map(|a| a.len()), Some(0));
    assert!(!stdout.contains("not found"));
}

#[test]
#[cfg(unix)]
fn test_broken_pipe_exits_cleanly() {
    use std::io::Read;
    use std::process::{Command as StdCommand, Stdio};

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    create_test_db(&db_path);

    // Regression for #33: a downstream consumer closing stdin early must
    // not cause nxv to panic from inside `println!`. Spawn nxv with stdout
    // piped, drop the read end immediately, then assert nxv exits without
    // a Rust panic backtrace.
    let bin = assert_cmd::cargo::cargo_bin!("nxv");
    let mut child = StdCommand::new(bin)
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "history",
            "python",
            "--full",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn nxv");

    // Drop stdout immediately to close the pipe → next println! in nxv hits EPIPE.
    drop(child.stdout.take());

    let status = child.wait().expect("wait nxv");
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr);
    }

    assert!(
        !stderr.contains("panicked at"),
        "nxv panicked on broken pipe (regressed #33): {stderr}"
    );
    // SIGPIPE-terminated processes have status.code() == None on Unix, or
    // 141 (128+13) when wrapped by a shell. A clean 0 is also acceptable
    // if nxv finished writing before the pipe closed.
    let code = status.code();
    assert!(
        code.is_none() || code == Some(0) || code == Some(141),
        "unexpected exit on broken pipe: code={code:?}, stderr={stderr}"
    );
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
        .args(["-v", "-q", "stats"])
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
fn test_search_no_index_suggests_sync() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("nonexistent.db");

    // When there's no index, suggest running 'nxv sync'.
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "python"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nxv sync"));
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
        .stderr(predicate::str::contains("error:").and(predicate::str::contains("nxv sync")));
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
        .stderr(predicate::str::contains("error:").and(predicate::str::contains("nxv sync")));
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
        .args(["--db-path", db_path.to_str().unwrap(), "stats"])
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
            known_vulnerabilities TEXT,
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
            ('python-3.9.0', '3.9.0', 'aaa', 1600000000, 'aaa', 1600000000, 'python', 'Python 3.9'),
            ('python-3.11.0', '3.11.0', 'ccc', 1602000000, 'ccc', 1602000000, 'python', 'Python 3.11'),
            ('python-3.10.0', '3.10.0', 'bbb', 1601000000, 'bbb', 1601000000, 'python', 'Python 3.10');
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

    // Verify all results have MIT in their license. `license` is a real JSON
    // array, never a stringified one.
    for result in results {
        let license = result.get("license").expect("license field present");
        let entries = license
            .as_array()
            .unwrap_or_else(|| panic!("license should be a JSON array, got {}", license));
        assert!(
            entries
                .iter()
                .any(|l| l.as_str().is_some_and(|l| l.contains("MIT"))),
            "License {} should contain 'MIT'",
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
            known_vulnerabilities TEXT,
            UNIQUE(attribute_path, version, first_commit_hash)
        );
        CREATE INDEX idx_packages_name ON package_versions(name);
        CREATE INDEX idx_packages_name_version ON package_versions(name, version, first_commit_date);
        CREATE INDEX idx_packages_attr ON package_versions(attribute_path);
        CREATE INDEX idx_packages_search_nocase ON package_versions(
            attribute_path COLLATE NOCASE,
            version COLLATE NOCASE,
            (LENGTH(attribute_path) - LENGTH(REPLACE(attribute_path, '.', ''))),
            last_commit_date DESC,
            first_commit_date DESC
        );
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
    let hash = base16ct::lower::encode_string(&hasher.finalize());

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
    let hash = base16ct::lower::encode_string(&hasher.finalize());

    (bytes, hash)
}

#[test]
fn test_sync_with_mock_http_server() {
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

    let _release_mock = server
        .mock("GET", "/repos/utensils/nxv/releases/latest")
        .with_status(500)
        .expect(0)
        .create();

    // Create temp directory for test database
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let bloom_path = dir.path().join("index.bloom");

    // Set environment variable for manifest URL
    let manifest_url = format!("{}/manifest.json", server.url());

    // Run sync command
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
            "--manifest-url",
            &manifest_url,
        ])
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .env("NXV_GITHUB_API_BASE", server.url())
        .env("NXV_API_URL", "http://127.0.0.1:1")
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
fn test_sync_already_up_to_date() {
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

    // Run sync command - should report up to date
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("up to date"));
}

#[test]
fn test_sync_delta_available() {
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
    let delta_hash = base16ct::lower::encode_string(&hasher.finalize());

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
            known_vulnerabilities TEXT,
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

    // Run sync command - should apply delta
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
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
fn test_sync_network_error_handling() {
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

    // Run sync command - should fail gracefully
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn test_sync_checksum_mismatch() {
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

    // Run sync command - should fail due to checksum mismatch
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
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
fn test_sync_unreachable_server() {
    // Use a URL that points to a non-existent/unreachable host
    // Using localhost with a port that's unlikely to be in use
    let manifest_url = "http://127.0.0.1:59999/manifest.json";

    // Create temp directory
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    // Run sync command - should fail gracefully with network error
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
            "--manifest-url",
            manifest_url,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn test_sync_incompatible_index_guides_app_update_without_calling_release_api() {
    let mut server = mockito::Server::new();
    let manifest = serde_json::json!({
        "version": 1,
        "min_version": 999,
        "latest_commit": "abc123",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {
            "url": format!("{}/index.db.zst", server.url()),
            "size_bytes": 1,
            "sha256": "00"
        },
        "bloom_filter": {
            "url": format!("{}/index.bloom", server.url()),
            "size_bytes": 1,
            "sha256": "00"
        },
        "deltas": []
    });
    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(manifest.to_string())
        .create();
    let _release_mock = server
        .mock("GET", "/repos/utensils/nxv/releases/latest")
        .with_status(500)
        .expect(0)
        .create();

    let dir = tempdir().unwrap();
    nxv()
        .args([
            "--db-path",
            dir.path().join("index.db").to_str().unwrap(),
            "sync",
            "--skip-verify",
            "--manifest-url",
            &format!("{}/manifest.json", server.url()),
        ])
        .env("NXV_GITHUB_API_BASE", server.url())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Run `nxv update`"))
        .stderr(predicate::str::contains("retry `nxv sync`"));
}

// ============================================================================
// Signature Verification Tests
// ============================================================================

#[test]
fn test_sync_fails_without_signature_when_verify_enabled() {
    // This test verifies that manifest signature verification is enforced by default.
    // When no signature file is available, sync should fail unless --skip-verify is used.

    // Create test artifacts
    let (compressed_db, db_hash) = create_compressed_test_db();
    let (bloom_data, bloom_hash) = create_test_bloom_filter();

    // Start mock server that serves manifest but NO .minisig file
    let mut server = mockito::Server::new();

    let manifest = format!(
        r#"{{
        "version": 2,
        "latest_commit": "abc123def456",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {{
            "url": "{}/index.db.zst",
            "size_bytes": {},
            "sha256": "{}"
        }},
        "bloom_filter": {{
            "url": "{}/index.bloom",
            "size_bytes": {},
            "sha256": "{}"
        }},
        "deltas": []
    }}"#,
        server.url(),
        compressed_db.len(),
        db_hash,
        server.url(),
        bloom_data.len(),
        bloom_hash
    );

    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&manifest)
        .create();

    // Signature file returns 404 - simulating no signature available
    let _signature_mock = server
        .mock("GET", "/manifest.json.minisig")
        .with_status(404)
        .create();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let manifest_url = format!("{}/manifest.json", server.url());

    // Sync should FAIL because signature verification is enabled by default
    // and no signature is available
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--manifest-url",
            &manifest_url,
        ])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Manifest signature not found")
                .or(predicate::str::contains("--skip-verify")),
        );

    // Database should NOT be created since update failed
    assert!(
        !db_path.exists(),
        "Database should not be created when signature verification fails"
    );
}

#[test]
fn test_sync_skip_verify_shows_warning() {
    // This test verifies that --skip-verify allows updates but shows a warning

    // Create test artifacts
    let (compressed_db, db_hash) = create_compressed_test_db();
    let (bloom_data, bloom_hash) = create_test_bloom_filter();

    // Start mock server
    let mut server = mockito::Server::new();

    let manifest = format!(
        r#"{{
        "version": 2,
        "latest_commit": "abc123def456",
        "latest_commit_date": "2024-01-15T12:00:00Z",
        "full_index": {{
            "url": "{}/index.db.zst",
            "size_bytes": {},
            "sha256": "{}"
        }},
        "bloom_filter": {{
            "url": "{}/index.bloom",
            "size_bytes": {},
            "sha256": "{}"
        }},
        "deltas": []
    }}"#,
        server.url(),
        compressed_db.len(),
        db_hash,
        server.url(),
        bloom_data.len(),
        bloom_hash
    );

    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&manifest)
        .create();

    let _db_mock = server
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

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let bloom_path = dir.path().join("index.bloom");
    let manifest_url = format!("{}/manifest.json", server.url());

    // Update with --skip-verify should succeed but show a warning
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
            "--manifest-url",
            &manifest_url,
        ])
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Skipping manifest signature verification",
        ));

    // Database should be created
    assert!(
        db_path.exists(),
        "Database should be created with --skip-verify"
    );
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

    // Stats should work offline
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "stats"])
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
        .stderr(predicate::str::contains("nxv sync"));
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
            "sync",
            "--skip-verify",
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

    // Run sync command - should fail
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
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

    // Run sync command - should fail
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
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

/// Full delta sync workflow test:
/// 1. Initial download creates index with initial packages
/// 2. New delta becomes available with new packages
/// 3. Apply delta sync
/// 4. Verify search returns both old and new packages
#[test]
fn test_full_delta_sync_workflow() {
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
    let initial_db_hash = base16ct::lower::encode_string(&initial_db_hasher.finalize());

    let mut initial_bloom_hasher = Sha256::new();
    initial_bloom_hasher.update(&initial_bloom_data);
    let initial_bloom_hash = base16ct::lower::encode_string(&initial_bloom_hasher.finalize());

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

    // Run initial sync
    nxv()
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
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

    // --- PHASE 2: Delta sync ---

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
    let delta_hash = base16ct::lower::encode_string(&delta_hasher.finalize());

    // Create updated bloom filter (contains both firefox and nodejs)
    let updated_bloom_data = {
        let mut filter = bloomfilter::Bloom::new_for_fp_rate(10000, 0.01).unwrap();
        filter.set(&"firefox");
        filter.set(&"nodejs");
        filter.to_bytes()
    };

    let mut updated_bloom_hasher = Sha256::new();
    updated_bloom_hasher.update(&updated_bloom_data);
    let updated_bloom_hash = base16ct::lower::encode_string(&updated_bloom_hasher.finalize());

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

    // Run delta sync
    nxv()
        .env("NXV_BLOOM_PATH", bloom_path.to_str().unwrap())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "sync",
            "--skip-verify",
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
// Indexer Integration Tests (mock release bucket; require `indexer` feature)
// ============================================================================

/// Build a packages.json document with `filler` synthetic packages plus the
/// monitor's sentinel packages and the given thunderbird version, brotli
/// compressed like the real artifacts. Counts are sized to clear the
/// 2020-era absolute floor (50k).
#[cfg(feature = "indexer")]
fn fixture_packages_json_br(thunderbird_version: &str, filler: usize) -> Vec<u8> {
    use std::io::Write;

    let mut json = String::with_capacity(filler * 120 + 4096);
    json.push_str("{\"packages\":{");
    for i in 0..filler {
        json.push_str(&format!(
            "\"pkg{i}\":{{\"name\":\"pkg{i}-1.0\",\"pname\":\"pkg{i}\",\"version\":\"1.0\",\"system\":\"x86_64-linux\",\"meta\":{{}}}},"
        ));
    }
    json.push_str(&format!(
        "\"thunderbird\":{{\"pname\":\"thunderbird\",\"version\":\"{thunderbird_version}\",\"system\":\"x86_64-linux\",\"meta\":{{\"description\":\"Mail client\"}}}},"
    ));
    json.push_str(
        "\"firefox\":{\"pname\":\"firefox\",\"version\":\"100.0\",\"system\":\"x86_64-linux\",\"meta\":{}},\
         \"nh\":{\"pname\":\"nh\",\"version\":\"4.0.0\",\"system\":\"x86_64-linux\",\"meta\":{}},\
         \"python313Packages.requests\":{\"pname\":\"requests\",\"version\":\"2.32.3\",\"system\":\"x86_64-linux\",\"meta\":{}}"
    );
    json.push_str("},\"version\":\"2\"}");

    let mut compressed = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut compressed, 4096, 3, 22);
        writer.write_all(json.as_bytes()).unwrap();
    }
    compressed
}

/// Stand up a mock release bucket with two releases (thunderbird 142.0 then
/// 143.0) and return (server, db_path guard dir, commit hashes).
#[cfg(feature = "indexer")]
fn mock_release_bucket(server: &mut mockito::Server) -> (Vec<mockito::Mock>, String, String) {
    let commit1 = "a".repeat(40);
    let commit2 = "b".repeat(40);
    // Recent dates: the run report marks stale observations unhealthy
    // (head lag), and the absolute count floor is year-dependent.
    let date1 = (chrono::Utc::now() - chrono::Duration::days(3))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let date2 = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();

    let listing = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>nixpkgs/nixpkgs-25.05pre100000.{c1short}</Key>
    <LastModified>2025-01-01T00:00:00.000Z</LastModified>
    <ETag>"x"</ETag>
    <ChecksumAlgorithm>CRC64NVME</ChecksumAlgorithm>
    <Size>1</Size>
  </Contents>
  <CommonPrefixes><Prefix>nixpkgs/nixpkgs-25.05pre100000.{c1short}/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>nixpkgs/nixpkgs-25.05pre100100.{c2short}/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>nixpkgs/nixpkgs-0.5/</Prefix></CommonPrefixes>
</ListBucketResult>"#,
        c1short = &commit1[..12],
        c2short = &commit2[..12],
    );

    let mocks = vec![
        server
            .mock("GET", "/")
            .match_query(mockito::Matcher::Regex("prefix=nixpkgs".to_string()))
            .with_body(listing)
            .create(),
        server
            .mock(
                "GET",
                format!(
                    "/nixpkgs/nixpkgs-25.05pre100000.{}/git-revision",
                    &commit1[..12]
                )
                .as_str(),
            )
            .with_header("Last-Modified", date1.as_str())
            .with_body(&commit1)
            .create(),
        server
            .mock(
                "GET",
                format!(
                    "/nixpkgs/nixpkgs-25.05pre100100.{}/git-revision",
                    &commit2[..12]
                )
                .as_str(),
            )
            .with_header("Last-Modified", date2.as_str())
            .with_body(&commit2)
            .create(),
        server
            .mock(
                "GET",
                format!(
                    "/nixpkgs/nixpkgs-25.05pre100000.{}/packages.json.br",
                    &commit1[..12]
                )
                .as_str(),
            )
            .with_body(fixture_packages_json_br("142.0", 132_000))
            .create(),
        server
            .mock(
                "GET",
                format!(
                    "/nixpkgs/nixpkgs-25.05pre100100.{}/packages.json.br",
                    &commit2[..12]
                )
                .as_str(),
            )
            .with_body(fixture_packages_json_br("143.0", 132_500))
            .create(),
    ];

    (mocks, commit1, commit2)
}

/// End-to-end: plan + ingest two mock releases, then verify the data through
/// the real CLI — the DESIGN.md regression fixtures.
#[test]
#[cfg(feature = "indexer")]
fn test_index_end_to_end_with_mock_bucket() {
    let mut server = mockito::Server::new();
    let (_mocks, commit1, commit2) = mock_release_bucket(&mut server);

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    nxv()
        .env("NXV_RELEASES_URL", server.url())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--channel",
            "nixpkgs-unstable",
        ])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success()
        .stderr(predicate::str::contains("status: healthy"));

    // Regression fixture (issue #23): nh must be present.
    nxv()
        .args(["--db-path", db_path.to_str().unwrap(), "search", "-e", "nh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("4.0.0"));

    // Regression fixture (issue #5): nested attrs present and searchable.
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "search",
            "-e",
            "python313Packages.requests",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("2.32.3"));

    // Range semantics (issue #21 class): both observed versions exist, each
    // bounded by REAL observed commits — 142.0 must not extend past the
    // release where 143.0 replaced it, and a never-observed version must
    // not exist.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let (first, last): (String, String) = conn
        .query_row(
            "SELECT first_commit_hash, last_commit_hash FROM package_versions \
             WHERE attribute_path = 'thunderbird' AND version = '142.0'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(first, commit1);
    assert_eq!(
        last, commit1,
        "142.0 was only observed at the first release"
    );

    let (first, last): (String, String) = conn
        .query_row(
            "SELECT first_commit_hash, last_commit_hash FROM package_versions \
             WHERE attribute_path = 'thunderbird' AND version = '143.0'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(first, commit2);
    assert_eq!(last, commit2);

    let phantom: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM package_versions \
             WHERE attribute_path = 'thunderbird' AND version = '144.0'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(phantom, 0, "never-observed versions must not be fabricated");

    // Ledger: both releases ingested with attr counts.
    let ingested: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM releases WHERE status = 'ingested'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ingested, 2);

    let search_index_present: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_packages_search_nocase'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        search_index_present,
        "a small index run must preserve the normally-maintained covering index"
    );

    // Bloom filter must resolve dotted attrs (written next to the DB).
    let bloom_path = dir.path().join("index.bloom");
    assert!(bloom_path.exists(), "bloom filter should be written");

    // Simulate an interrupted bulk run after its transactional marker+drop.
    // The idempotent no-work run below must heal this state even though it has
    // no package observations to write.
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         INSERT OR REPLACE INTO meta (key, value)
         VALUES ('search_index_dropped', '1');
         DROP INDEX idx_packages_search_nocase;
         COMMIT;",
    )
    .unwrap();

    // Idempotence: a second run ingests nothing, repairs the covering index,
    // and stays healthy.
    nxv()
        .env("NXV_RELEASES_URL", server.url())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--channel",
            "nixpkgs-unstable",
        ])
        .timeout(std::time::Duration::from_secs(60))
        .assert()
        .success()
        .stderr(
            predicate::str::contains("rebuilding search index")
                .and(predicate::str::contains("nothing to ingest")),
        );

    let recovered: (bool, i64) = conn
        .query_row(
            "SELECT
                 EXISTS(
                     SELECT 1 FROM sqlite_master
                     WHERE type = 'index' AND name = 'idx_packages_search_nocase'
                 ),
                 (SELECT COUNT(*) FROM meta WHERE key = 'search_index_dropped')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        recovered.0,
        "the no-work run must rebuild the covering index"
    );
    assert_eq!(
        recovered.1, 0,
        "the no-work recovery must clear the drop marker"
    );
}

/// A truncated artifact must fail its release without polluting the table,
/// and the release must stay retryable.
#[test]
#[cfg(feature = "indexer")]
fn test_index_truncated_snapshot_fails_release_without_polluting() {
    let mut server = mockito::Server::new();
    let commit1 = "c".repeat(40);

    let listing = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <IsTruncated>false</IsTruncated>
  <CommonPrefixes><Prefix>nixpkgs/nixpkgs-25.05pre100000.{short}/</Prefix></CommonPrefixes>
</ListBucketResult>"#,
        short = &commit1[..12],
    );
    let _m1 = server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Regex("prefix=nixpkgs".to_string()))
        .with_body(listing)
        .create();
    let recent = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let _m2 = server
        .mock(
            "GET",
            format!(
                "/nixpkgs/nixpkgs-25.05pre100000.{}/git-revision",
                &commit1[..12]
            )
            .as_str(),
        )
        .with_header("Last-Modified", recent.as_str())
        .with_body(&commit1)
        .create();

    let full = fixture_packages_json_br("142.0", 132_000);
    let _m3 = server
        .mock(
            "GET",
            format!(
                "/nixpkgs/nixpkgs-25.05pre100000.{}/packages.json.br",
                &commit1[..12]
            )
            .as_str(),
        )
        .with_body(&full[..full.len() / 2])
        .create();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    nxv()
        .env("NXV_RELEASES_URL", server.url())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--channel",
            "nixpkgs-unstable",
        ])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .success() // not --strict: failures are recorded, run completes
        .stderr(predicate::str::contains("FAILED"));

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM package_versions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0, "a failed snapshot must never write rows");

    let (status, error): (String, Option<String>) = conn
        .query_row("SELECT status, error FROM releases", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(status, "failed");
    assert!(error.is_some());
}

/// Known malformed upstream snapshots are quarantined as skipped so scheduled
/// publishing does not stay red forever, while unknown bad snapshots still hit
/// the normal gate-failure path.
#[test]
#[cfg(feature = "indexer")]
fn test_index_known_bad_snapshot_is_skipped_without_fetching_packages() {
    let mut server = mockito::Server::new();
    let release_name = "nixos-24.11pre657868.4a8e77c70685";
    let short = "4a8e77c70685";
    let commit = format!("{short}{}", "0".repeat(28));

    let listing = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <IsTruncated>false</IsTruncated>
  <CommonPrefixes><Prefix>nixos/unstable-small/{release_name}/</Prefix></CommonPrefixes>
</ListBucketResult>"#,
    );
    let _m1 = server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Regex(
            "prefix=nixos%2Funstable-small".to_string(),
        ))
        .with_body(listing)
        .create();
    let recent = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let _m2 = server
        .mock(
            "GET",
            format!("/nixos/unstable-small/{release_name}/git-revision").as_str(),
        )
        .with_header("Last-Modified", recent.as_str())
        .with_body(&commit)
        .create();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    nxv()
        .env("NXV_RELEASES_URL", server.url())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--channel",
            "nixos-unstable-small",
        ])
        .timeout(std::time::Duration::from_secs(60))
        .assert()
        .success()
        .stderr(predicate::str::contains(format!(
            "skipping {release_name}: known malformed upstream snapshot"
        )))
        .stderr(predicate::str::contains("status: healthy"));

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let (status, error): (String, Option<String>) = conn
        .query_row("SELECT status, error FROM releases", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(status, "skipped");
    assert!(
        error
            .as_deref()
            .is_some_and(|msg| msg.contains("known malformed upstream snapshot"))
    );
}

/// --strict turns failed releases into a non-zero exit.
#[test]
#[cfg(feature = "indexer")]
fn test_index_strict_fails_on_gate_violation() {
    let mut server = mockito::Server::new();
    let commit1 = "d".repeat(40);

    let listing = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <IsTruncated>false</IsTruncated>
  <CommonPrefixes><Prefix>nixpkgs/nixpkgs-25.05pre100000.{short}/</Prefix></CommonPrefixes>
</ListBucketResult>"#,
        short = &commit1[..12],
    );
    let _m1 = server
        .mock("GET", "/")
        .match_query(mockito::Matcher::Regex("prefix=nixpkgs".to_string()))
        .with_body(listing)
        .create();
    let recent = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let _m2 = server
        .mock(
            "GET",
            format!(
                "/nixpkgs/nixpkgs-25.05pre100000.{}/git-revision",
                &commit1[..12]
            )
            .as_str(),
        )
        .with_header("Last-Modified", recent.as_str())
        .with_body(&commit1)
        .create();
    // Far too few packages: trips the absolute count floor.
    let _m3 = server
        .mock(
            "GET",
            format!(
                "/nixpkgs/nixpkgs-25.05pre100000.{}/packages.json.br",
                &commit1[..12]
            )
            .as_str(),
        )
        .with_body(fixture_packages_json_br("142.0", 100))
        .create();

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index.db");

    nxv()
        .env("NXV_RELEASES_URL", server.url())
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "index",
            "--channel",
            "nixpkgs-unstable",
            "--strict",
        ])
        .timeout(std::time::Duration::from_secs(120))
        .assert()
        .failure()
        .stderr(predicate::str::contains("GATE FAILED"));
}

// ============================================================================
// Signing Workflow Tests (Indexer Feature)
// ============================================================================

/// Test the keygen command generates valid keypair files.
#[test]
#[cfg_attr(not(feature = "indexer"), ignore)]
fn test_keygen_generates_keypair() {
    let dir = tempdir().unwrap();
    let sk_path = dir.path().join("test.key");
    let pk_path = dir.path().join("test.pub");

    // Generate keypair
    nxv()
        .args([
            "keygen",
            "--secret-key",
            sk_path.to_str().unwrap(),
            "--public-key",
            pk_path.to_str().unwrap(),
            "--comment",
            "test signing key",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Generated keypair"))
        .stderr(predicate::str::contains("Secret key:"))
        .stderr(predicate::str::contains("Public key:"));

    // Verify files exist and have expected content
    assert!(sk_path.exists(), "Secret key should be created");
    assert!(pk_path.exists(), "Public key should be created");

    let sk_content = std::fs::read_to_string(&sk_path).unwrap();
    assert!(
        sk_content.contains("untrusted comment:"),
        "Secret key should have comment"
    );

    let pk_content = std::fs::read_to_string(&pk_path).unwrap();
    assert!(
        pk_content.contains("untrusted comment:"),
        "Public key should have comment"
    );
    assert!(
        pk_content.contains("RW"),
        "Public key should contain RW prefix"
    );
}

/// Test that keygen fails when files already exist without --force.
#[test]
#[cfg_attr(not(feature = "indexer"), ignore)]
fn test_keygen_refuses_overwrite_without_force() {
    let dir = tempdir().unwrap();
    let sk_path = dir.path().join("test.key");
    let pk_path = dir.path().join("test.pub");

    // Create existing files
    std::fs::write(&sk_path, "existing key content").unwrap();
    std::fs::write(&pk_path, "existing pub content").unwrap();

    // keygen should fail without --force
    nxv()
        .args([
            "keygen",
            "--secret-key",
            sk_path.to_str().unwrap(),
            "--public-key",
            pk_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    // Files should be unchanged
    let sk_content = std::fs::read_to_string(&sk_path).unwrap();
    assert_eq!(sk_content, "existing key content");
}

/// Test that keygen with --force overwrites existing files.
#[test]
#[cfg_attr(not(feature = "indexer"), ignore)]
fn test_keygen_force_overwrites() {
    let dir = tempdir().unwrap();
    let sk_path = dir.path().join("test.key");
    let pk_path = dir.path().join("test.pub");

    // Create existing files
    std::fs::write(&sk_path, "existing key content").unwrap();
    std::fs::write(&pk_path, "existing pub content").unwrap();

    // keygen with --force should succeed
    nxv()
        .args([
            "keygen",
            "--force",
            "--secret-key",
            sk_path.to_str().unwrap(),
            "--public-key",
            pk_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Files should be overwritten with real keys
    let sk_content = std::fs::read_to_string(&sk_path).unwrap();
    assert!(
        sk_content.contains("untrusted comment:"),
        "Secret key should be a valid key format"
    );
}

/// Test the full publish with signing workflow.
#[test]
#[cfg_attr(not(feature = "indexer"), ignore)]
fn test_publish_with_signing() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let output_dir = dir.path().join("publish");
    let sk_path = dir.path().join("signing.key");
    let pk_path = dir.path().join("signing.pub");

    // Create a test database
    create_test_db(&db_path);

    // Generate signing keypair
    nxv()
        .args([
            "keygen",
            "--secret-key",
            sk_path.to_str().unwrap(),
            "--public-key",
            pk_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Publish with signing
    nxv()
        .args([
            "--db-path",
            db_path.to_str().unwrap(),
            "publish",
            "--output",
            output_dir.to_str().unwrap(),
            "--url-prefix",
            "https://example.com/releases",
            "--artifact-name-prefix",
            "run-123-",
            "--sign",
            "--secret-key",
            sk_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Signing manifest"))
        .stderr(predicate::str::contains("manifest.json.minisig"));

    // Verify all artifacts exist
    assert!(output_dir.join("index.db.zst").exists());
    assert!(output_dir.join("bloom.bin").exists());
    assert!(output_dir.join("manifest.json").exists());
    assert!(
        output_dir.join("manifest.json.minisig").exists(),
        "Signature file should be created"
    );

    // Verify signature file format
    let sig_content = std::fs::read_to_string(output_dir.join("manifest.json.minisig")).unwrap();
    assert!(
        sig_content.contains("untrusted comment:"),
        "Signature should have untrusted comment"
    );
    assert!(
        sig_content.contains("trusted comment:"),
        "Signature should have trusted comment"
    );

    let manifest_content = std::fs::read_to_string(output_dir.join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&manifest_content).unwrap();
    assert_eq!(
        manifest["full_index"]["url"],
        "https://example.com/releases/run-123-index.db.zst"
    );
    assert_eq!(
        manifest["bloom_filter"]["url"],
        "https://example.com/releases/run-123-bloom.bin"
    );
}

#[test]
fn test_publish_index_workflow_keeps_manifest_as_stable_pointer() {
    let workflow_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".github/workflows/publish-index.yml");
    let Ok(workflow) = std::fs::read_to_string(&workflow_path) else {
        eprintln!(
            "skipping workflow assertion because {} is absent from this source tree",
            workflow_path.display()
        );
        return;
    };

    assert!(
        workflow.contains("curl -sSfL \"${INDEX_URL_PREFIX}/manifest.json\""),
        "publish-index should discover the current index from the manifest"
    );
    assert!(
        workflow.contains("--artifact-name-prefix \"${INDEX_ASSET_PREFIX}\""),
        "published manifests should point at run-scoped immutable assets"
    );
    assert!(
        workflow.contains(
            "cp publish/index.db.zst \"${immutable_upload_dir}/${INDEX_ASSET_PREFIX}index.db.zst\""
        ) && workflow.contains(
            "cp publish/bloom.bin \"${immutable_upload_dir}/${INDEX_ASSET_PREFIX}bloom.bin\""
        ),
        "payload basenames should be staged with run-scoped asset names before upload"
    );
    assert!(
        workflow.contains("\"${immutable_upload_dir}/${INDEX_ASSET_PREFIX}index.db.zst\"")
            && workflow.contains("\"${immutable_upload_dir}/${INDEX_ASSET_PREFIX}bloom.bin\""),
        "index payload should upload from paths whose basenames are the run-scoped asset names"
    );
    assert!(
        !workflow.contains("\"publish/index.db.zst#${INDEX_ASSET_PREFIX}index.db.zst\""),
        "gh release upload treats # suffixes as labels, not asset filenames"
    );
    assert!(
        workflow
            .contains("replace_stable_asset publish/manifest.json.minisig manifest.json.minisig"),
        "signature should be replaced before manifest.json"
    );
    assert!(
        workflow.contains("replace_stable_asset publish/manifest.json manifest.json"),
        "manifest.json should be the final stable pointer replacement"
    );
    assert!(
        workflow.contains("clients may see a brief")
            && workflow.contains("signature mismatch until manifest.json is replaced"),
        "workflow should document the non-atomic manifest/signature replacement window"
    );
    assert!(
        workflow.contains("for attempt in {1..12}")
            && workflow.contains("Manifest pointer has not propagated yet")
            && workflow.contains("Published manifest did not point at this run's immutable assets after waiting for propagation"),
        "manifest verification should retry to tolerate GitHub release/CDN propagation delay"
    );
    assert!(
        workflow.find("replace_stable_asset publish/manifest.json.minisig manifest.json.minisig")
            < workflow.find("replace_stable_asset publish/manifest.json manifest.json"),
        "manifest.json must be replaced after its signature"
    );
}

/// End-to-end test: keygen → publish with sign → update with custom public key.
///
/// This is the critical security path test that verifies:
/// 1. Generated keys work for signing
/// 2. Signed manifests are served correctly
/// 3. Update with --public-key validates against the correct key
#[test]
#[cfg_attr(not(feature = "indexer"), ignore)]
fn test_end_to_end_signed_manifest_verification() {
    let dir = tempdir().unwrap();
    let source_db_path = dir.path().join("source.db");
    let output_dir = dir.path().join("publish");
    let sk_path = dir.path().join("signing.key");
    let pk_path = dir.path().join("signing.pub");
    let client_db_path = dir.path().join("client.db");

    // Step 1: Create a source database
    create_test_db(&source_db_path);

    // Step 2: Generate signing keypair
    nxv()
        .args([
            "keygen",
            "--secret-key",
            sk_path.to_str().unwrap(),
            "--public-key",
            pk_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Step 3: Publish with signing
    nxv()
        .args([
            "--db-path",
            source_db_path.to_str().unwrap(),
            "publish",
            "--output",
            output_dir.to_str().unwrap(),
            "--sign",
            "--secret-key",
            sk_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Verify all artifacts exist
    assert!(output_dir.join("index.db.zst").exists());
    assert!(output_dir.join("bloom.bin").exists());
    assert!(output_dir.join("manifest.json").exists());
    assert!(output_dir.join("manifest.json.minisig").exists());

    // Step 4: Start mock server serving the published artifacts
    let mut server = mockito::Server::new();

    // Read the manifest and update URLs to point to mock server
    let manifest_content = std::fs::read_to_string(output_dir.join("manifest.json")).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&manifest_content).unwrap();

    // Update URLs in manifest to point to our mock server
    manifest["full_index"]["url"] = serde_json::json!(format!("{}/index.db.zst", server.url()));
    manifest["bloom_filter"]["url"] = serde_json::json!(format!("{}/bloom.bin", server.url()));

    let updated_manifest = serde_json::to_string(&manifest).unwrap();

    // Re-sign the modified manifest (URLs changed)
    // We need to sign the modified manifest content
    let temp_manifest_path = dir.path().join("temp_manifest.json");
    std::fs::write(&temp_manifest_path, &updated_manifest).unwrap();

    // Sign using minisign crate directly (since we can't call nxv publish for just signing)
    let sk_content = std::fs::read_to_string(&sk_path).unwrap();
    let sk_box = minisign::SecretKeyBox::from_string(&sk_content).unwrap();
    let sk = sk_box.into_unencrypted_secret_key().unwrap();

    use std::io::Cursor;
    let mut cursor = Cursor::new(updated_manifest.as_bytes());
    let sig_box = minisign::sign(
        None,
        &sk,
        &mut cursor,
        Some("test signature"),
        Some("test trusted comment"),
    )
    .unwrap();
    let signature = sig_box.to_string();

    // Set up mock endpoints
    let _manifest_mock = server
        .mock("GET", "/manifest.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&updated_manifest)
        .create();

    let _signature_mock = server
        .mock("GET", "/manifest.json.minisig")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body(&signature)
        .create();

    let db_data = std::fs::read(output_dir.join("index.db.zst")).unwrap();
    let _db_mock = server
        .mock("GET", "/index.db.zst")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(db_data)
        .create();

    let bloom_data = std::fs::read(output_dir.join("bloom.bin")).unwrap();
    let _bloom_mock = server
        .mock("GET", "/bloom.bin")
        .with_status(200)
        .with_header("content-type", "application/octet-stream")
        .with_body(bloom_data)
        .create();

    let manifest_url = format!("{}/manifest.json", server.url());

    // Step 5: Sync using the public key - this should SUCCEED
    nxv()
        .args([
            "--db-path",
            client_db_path.to_str().unwrap(),
            "sync",
            "--manifest-url",
            &manifest_url,
            "--public-key",
            pk_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("Index downloaded successfully"));

    // Verify the database was created
    assert!(
        client_db_path.exists(),
        "Database should be created after successful signed sync"
    );

    // Step 6: Try with WRONG public key - this should FAIL
    let wrong_pk_path = dir.path().join("wrong.pub");
    let wrong_sk_path = dir.path().join("wrong.key");

    nxv()
        .args([
            "keygen",
            "--secret-key",
            wrong_sk_path.to_str().unwrap(),
            "--public-key",
            wrong_pk_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let client_db_path2 = dir.path().join("client2.db");

    nxv()
        .args([
            "--db-path",
            client_db_path2.to_str().unwrap(),
            "sync",
            "--manifest-url",
            &manifest_url,
            "--public-key",
            wrong_pk_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("signature verification failed"));

    // Verify database was NOT created with wrong key
    assert!(
        !client_db_path2.exists(),
        "Database should NOT be created when signature verification fails"
    );
}

// ---------------------------------------------------------------------------
// `nxv skill` tests
// ---------------------------------------------------------------------------

/// The embedded skill template, as installed by `nxv skill install`.
const SKILL_TEMPLATE: &str = include_str!("../src/skill/SKILL.md");

#[test]
fn test_skill_show_prints_template() {
    nxv()
        .args(["skill", "show"])
        .assert()
        .success()
        .stdout(predicate::eq(SKILL_TEMPLATE));
}

#[test]
fn test_skill_list_names_agents() {
    nxv()
        .args(["skill", "list", "--ascii"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude"))
        .stdout(predicate::str::contains("copilot"))
        .stdout(predicate::str::contains("openclaw"));
}

// User-scope tests override HOME, which `dirs::home_dir()` only honors on
// unix (Windows resolves the profile dir via the known-folder API). The
// project-scope tests below cover all platforms.
#[cfg(unix)]
#[test]
fn test_skill_install_user_scope_explicit_agent() {
    let home = tempdir().unwrap();
    nxv()
        .env("HOME", home.path())
        .args(["skill", "install", "claude"])
        .assert()
        .success();
    let installed = home.path().join(".claude/skills/nxv/SKILL.md");
    assert_eq!(std::fs::read_to_string(installed).unwrap(), SKILL_TEMPLATE);
}

#[cfg(unix)]
#[test]
fn test_skill_install_detected_agents_requires_explicit_flag() {
    let home = tempdir().unwrap();
    std::fs::create_dir(home.path().join(".codex")).unwrap();
    nxv()
        .env("HOME", home.path())
        .args(["skill", "install", "--detected"])
        .assert()
        .success()
        .stderr(predicate::str::contains(".codex"));
    assert!(home.path().join(".codex/skills/nxv/SKILL.md").exists());
    // Undetected agents must not have directories conjured for them.
    assert!(!home.path().join(".cursor").exists());
    assert!(!home.path().join(".agents").exists());
}

#[cfg(unix)]
#[test]
fn test_skill_install_detected_with_no_agents_fails_without_writes() {
    let home = tempdir().unwrap();
    nxv()
        .env("HOME", home.path())
        .args(["skill", "install", "--detected"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no supported agents were detected",
        ))
        .stderr(predicate::str::contains(
            "use `agents` for the generic Agent Skills directory",
        ));
    assert!(!home.path().join(".agents").exists());
}

#[test]
fn test_skill_install_project_without_target_fails_without_writes() {
    let proj = tempdir().unwrap();
    nxv()
        .args(["skill", "install", "--dir", proj.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
    assert!(!proj.path().join(".claude").exists());
    assert!(!proj.path().join(".agents").exists());
}

#[test]
fn test_skill_install_project_all_is_explicitly_broad() {
    let proj = tempdir().unwrap();
    nxv()
        .args([
            "skill",
            "install",
            "--all",
            "--dir",
            proj.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(proj.path().join(".claude/skills/nxv/SKILL.md").exists());
    assert!(proj.path().join(".agents/skills/nxv/SKILL.md").exists());
    assert!(proj.path().join(".pi/skills/nxv/SKILL.md").exists());
    assert!(proj.path().join(".github/skills/nxv/SKILL.md").exists());
}

#[cfg(unix)]
#[test]
fn test_skill_install_project_detected_maps_only_detected_agents() {
    let home = tempdir().unwrap();
    let proj = tempdir().unwrap();
    std::fs::create_dir(home.path().join(".codex")).unwrap();
    nxv()
        .env("HOME", home.path())
        .args([
            "skill",
            "install",
            "--detected",
            "--dir",
            proj.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("codex"));
    assert!(proj.path().join(".agents/skills/nxv/SKILL.md").exists());
    assert!(!proj.path().join(".claude").exists());
    assert!(!proj.path().join(".github").exists());
}

#[test]
fn test_skill_install_project_copilot_uses_github_dir() {
    let proj = tempdir().unwrap();
    nxv()
        .args([
            "skill",
            "install",
            "copilot",
            "--dir",
            proj.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(proj.path().join(".github/skills/nxv/SKILL.md").exists());
    assert!(!proj.path().join(".claude").exists());
}

#[test]
fn test_skill_install_dedupes_shared_paths() {
    let proj = tempdir().unwrap();
    nxv()
        .args([
            "skill",
            "install",
            "codex",
            "cursor",
            "--dir",
            proj.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("(codex, cursor)"));
    assert!(proj.path().join(".agents/skills/nxv/SKILL.md").exists());
}

#[test]
fn test_skill_install_overwrites_existing() {
    let proj = tempdir().unwrap();
    let target_dir = proj.path().join(".claude/skills/nxv");
    std::fs::create_dir_all(&target_dir).unwrap();
    std::fs::write(target_dir.join("SKILL.md"), "stale garbage").unwrap();
    nxv()
        .args([
            "skill",
            "install",
            "claude",
            "--dir",
            proj.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(target_dir.join("SKILL.md")).unwrap(),
        SKILL_TEMPLATE
    );
}

#[test]
fn test_skill_uninstall_removes_only_nxv() {
    let proj = tempdir().unwrap();
    let dir_arg = proj.path().to_str().unwrap();

    nxv()
        .args(["skill", "install", "--all", "--dir", dir_arg])
        .assert()
        .success();

    // A sibling skill must survive the uninstall.
    let sibling = proj.path().join(".claude/skills/other");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("SKILL.md"), "other skill").unwrap();

    nxv()
        .args(["skill", "uninstall", "--dir", dir_arg])
        .assert()
        .success();

    assert!(!proj.path().join(".claude/skills/nxv").exists());
    assert!(!proj.path().join(".agents/skills/nxv").exists());
    assert!(sibling.join("SKILL.md").exists());
}

#[test]
fn test_skill_uninstall_keeps_non_empty_dir() {
    let proj = tempdir().unwrap();
    let dir_arg = proj.path().to_str().unwrap();

    nxv()
        .args(["skill", "install", "claude", "--dir", dir_arg])
        .assert()
        .success();

    let extra = proj.path().join(".claude/skills/nxv/notes.txt");
    std::fs::write(&extra, "user notes").unwrap();

    nxv()
        .args(["skill", "uninstall", "--dir", dir_arg])
        .assert()
        .success()
        .stderr(predicate::str::contains("Left non-empty directory"));

    assert!(!proj.path().join(".claude/skills/nxv/SKILL.md").exists());
    assert!(extra.exists());
}

#[test]
fn test_skill_uninstall_nothing_installed() {
    let proj = tempdir().unwrap();
    nxv()
        .args(["skill", "uninstall", "--dir", proj.path().to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicate::str::contains("No installed nxv skill"));
}

/// The checked-in skill copies are generated from src/skill/SKILL.md — keep
/// them byte-identical. Skipped when the copies are absent (the published
/// crate excludes .claude/ and .agents/).
#[test]
fn test_checked_in_skill_copies_match_template() {
    for rel in [".claude/skills/nxv/SKILL.md", ".agents/skills/nxv/SKILL.md"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        if !path.exists() {
            continue;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, SKILL_TEMPLATE,
            "{rel} is stale; regenerate with `cargo run -- skill install claude agents --dir .`"
        );
    }
}
