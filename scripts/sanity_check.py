#!/usr/bin/env python3
"""
Sanity check script for nxv database integrity.

Validates internal data quality and verifies fixes for known issues.
See docs/indexer-all-packages-issue.md for context.

Usage:
    python scripts/sanity_check.py [--db PATH] [--nixpkgs PATH] [--verbose] [--json]
"""

import argparse
import json
import sqlite3
import subprocess
import sys
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path


@dataclass
class CheckResult:
    name: str
    passed: bool
    message: str
    details: dict = field(default_factory=dict)
    severity: str = "error"  # error, warning, info


@dataclass
class SanityReport:
    checks: list[CheckResult] = field(default_factory=list)
    passed: int = 0
    failed: int = 0
    warnings: int = 0

    def add(self, result: CheckResult):
        self.checks.append(result)
        if result.passed:
            self.passed += 1
        elif result.severity == "warning":
            self.warnings += 1
        else:
            self.failed += 1


# =============================================================================
# BASIC INTEGRITY CHECKS
# =============================================================================


def check_duplicates(conn: sqlite3.Connection) -> CheckResult:
    """Check for duplicate (attribute_path, version) pairs."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT attribute_path, version, COUNT(*) as cnt
        FROM package_versions
        GROUP BY attribute_path, version
        HAVING cnt > 1
        LIMIT 10
    """)
    duplicates = cursor.fetchall()

    if not duplicates:
        return CheckResult("no_duplicates", True, "No duplicate (attr, version) pairs")

    return CheckResult(
        "no_duplicates",
        False,
        f"Found {len(duplicates)} duplicate pairs",
        {
            "duplicates": [
                {"attr": d[0], "version": d[1], "count": d[2]} for d in duplicates
            ]
        },
    )


def check_date_integrity(conn: sqlite3.Connection) -> CheckResult:
    """Check that first_commit_date <= last_commit_date."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE first_commit_date > last_commit_date
    """)
    invalid_count = cursor.fetchone()[0]

    if invalid_count == 0:
        return CheckResult(
            "date_integrity", True, "All date ranges valid (first <= last)"
        )

    return CheckResult(
        "date_integrity",
        False,
        f"{invalid_count} rows have first_commit_date > last_commit_date",
    )


def check_schema_version(conn: sqlite3.Connection) -> CheckResult:
    """Check schema version is expected."""
    cursor = conn.cursor()
    cursor.execute("SELECT value FROM meta WHERE key = 'schema_version'")
    row = cursor.fetchone()

    if not row:
        return CheckResult("schema_version", False, "No schema_version in meta table")

    version = int(row[0])
    expected = 4
    passed = version == expected

    return CheckResult(
        "schema_version",
        passed,
        f"Schema version {version}" + ("" if passed else f" (expected {expected})"),
    )


def check_commit_hash_format(conn: sqlite3.Connection) -> CheckResult:
    """Check commit hashes are valid 40-char hex strings."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE first_commit_hash IS NOT NULL
          AND (LENGTH(first_commit_hash) != 40
               OR first_commit_hash GLOB '*[^0-9a-f]*')
    """)
    invalid = cursor.fetchone()[0]

    passed = invalid == 0
    return CheckResult(
        "commit_hash_format",
        passed,
        f"{invalid} invalid commit hashes" if not passed else "All commit hashes valid",
    )


def check_fts_sync(conn: sqlite3.Connection) -> CheckResult:
    """Check FTS index is in sync with main table."""
    cursor = conn.cursor()
    cursor.execute("SELECT COUNT(*) FROM package_versions")
    main_count = cursor.fetchone()[0]
    cursor.execute("SELECT COUNT(*) FROM package_versions_fts")
    fts_count = cursor.fetchone()[0]

    passed = main_count == fts_count
    return CheckResult("fts_sync", passed, f"FTS: {fts_count}, main: {main_count}")


# =============================================================================
# VERSION EXTRACTION CHECKS (Issue #21 fixes)
# =============================================================================


def check_version_source_distribution(conn: sqlite3.Connection) -> CheckResult:
    """Check version_source distribution is reasonable."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT version_source, COUNT(*) as cnt,
               ROUND(100.0 * COUNT(*) / (SELECT COUNT(*) FROM package_versions), 2) as pct
        FROM package_versions
        GROUP BY version_source
        ORDER BY cnt DESC
    """)
    distribution = {
        row[0] or "NULL": {"count": row[1], "pct": row[2]} for row in cursor.fetchall()
    }

    direct_pct = distribution.get("direct", {}).get("pct", 0)
    null_pct = distribution.get("NULL", {}).get("pct", 0) + distribution.get(
        "", {}
    ).get("pct", 0)

    passed = direct_pct >= 30 and null_pct < 5
    return CheckResult(
        "version_source_distribution",
        passed,
        f"direct={direct_pct}%, null={null_pct}%",
        {"distribution": distribution},
    )


def check_unknown_versions(conn: sqlite3.Connection) -> CheckResult:
    """Check rate of unknown/empty versions."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT COUNT(*) as total,
               SUM(CASE WHEN version = 'unknown' OR version IS NULL OR version = '' THEN 1 ELSE 0 END) as unknown
        FROM package_versions
    """)
    total, unknown = cursor.fetchone()
    pct = (unknown / total * 100) if total > 0 else 0

    passed = pct < 5
    return CheckResult(
        "unknown_versions",
        passed,
        f"{unknown}/{total} ({pct:.2f}%) unknown/empty",
        {"pct": pct},
    )


def check_wrapper_versions(conn: sqlite3.Connection) -> CheckResult:
    """Check that wrapper packages have version extraction working.

    This validates the fix for wrapper version extraction (Phase 1.1).
    Wrapped packages like neovim, weechat should now have versions via unwrapped fallback.
    """
    cursor = conn.cursor()

    # Packages that are known wrappers - check both wrapped and unwrapped
    wrapper_packages = [
        ("neovim", "neovim-unwrapped"),
        ("weechat", "weechat-unwrapped"),
        ("thunderbird", "thunderbird-unwrapped"),
        ("firefox", "firefox-unwrapped"),
    ]

    results = {}
    issues = []

    for wrapped, unwrapped in wrapper_packages:
        cursor.execute(
            """
            SELECT COUNT(*) FROM package_versions
            WHERE attribute_path = ? AND version != 'unknown' AND version != '' AND version IS NOT NULL
        """,
            (wrapped,),
        )
        wrapped_count = cursor.fetchone()[0]

        cursor.execute(
            """
            SELECT COUNT(*) FROM package_versions
            WHERE attribute_path = ? AND version != 'unknown' AND version != '' AND version IS NOT NULL
        """,
            (unwrapped,),
        )
        unwrapped_count = cursor.fetchone()[0]

        results[wrapped] = {"wrapped": wrapped_count, "unwrapped": unwrapped_count}

        # Both should have versions if the fix is working
        if wrapped_count == 0 and unwrapped_count > 0:
            issues.append(
                f"{wrapped} has 0 versions but {unwrapped} has {unwrapped_count}"
            )

    passed = len(issues) == 0
    return CheckResult(
        "wrapper_versions",
        passed,
        f"{len(issues)} wrapper issues"
        if issues
        else "Wrapper version extraction working",
        {"packages": results, "issues": issues},
        severity="warning" if not passed else "error",
    )


def check_name_based_extraction(conn: sqlite3.Connection) -> CheckResult:
    """Check that name-based version extraction is working.

    This validates Phase 1.2 - extracting version from package name.
    """
    cursor = conn.cursor()

    cursor.execute("""
        SELECT COUNT(*) FROM package_versions WHERE version_source = 'name'
    """)
    name_count = cursor.fetchone()[0]

    cursor.execute("SELECT COUNT(*) FROM package_versions")
    total = cursor.fetchone()[0]

    pct = (name_count / total * 100) if total > 0 else 0

    # Name-based extraction should be used for some packages
    passed = name_count > 0
    return CheckResult(
        "name_based_extraction",
        passed,
        f"{name_count} packages ({pct:.1f}%) use name-based version",
        {"count": name_count, "pct": pct},
    )


# =============================================================================
# ALL-PACKAGES.NIX HANDLING CHECKS (Issue #21 primary fix)
# =============================================================================


def check_callpackage_packages(conn: sqlite3.Connection) -> CheckResult:
    """Check that packages defined via callPackage in all-packages.nix are captured.

    These packages were missed before the diff parsing fix.
    """
    cursor = conn.cursor()

    # Major packages defined via callPackage in all-packages.nix
    callpackage_packages = [
        "thunderbird",
        "firefox",
        "chromium",
        "vscode",
        "slack",
        "discord",
        "spotify",
        "signal-desktop",
        "telegram-desktop",
        "git",
        "curl",
        "wget",
        "htop",
        "ripgrep",
        "fd",
        "bat",
        "docker",
        "podman",
        "kubectl",
        "terraform",
        "postgresql",
        "mysql",
        "redis",
        "mongodb",
    ]

    found = {}
    missing = []

    for pkg in callpackage_packages:
        cursor.execute(
            """
            SELECT COUNT(*) FROM package_versions
            WHERE attribute_path = ? AND version != 'unknown' AND version != ''
        """,
            (pkg,),
        )
        count = cursor.fetchone()[0]
        if count > 0:
            found[pkg] = count
        else:
            missing.append(pkg)

    # Allow some missing (they might not exist in older nixpkgs)
    passed = len(missing) <= len(callpackage_packages) * 0.3  # Max 30% missing
    return CheckResult(
        "callpackage_packages",
        passed,
        f"{len(found)}/{len(callpackage_packages)} callPackage packages found",
        {"found": found, "missing": missing},
    )


def check_first_commit_baseline(conn: sqlite3.Connection) -> CheckResult:
    """Check that the first indexed commit captured baseline packages.

    This validates Phase 4.4 - first commit should have many packages, not just changed files.
    Before fix: ~14 packages; After fix: ~6,483+ packages.
    """
    cursor = conn.cursor()

    # Get the earliest commit date
    cursor.execute("""
        SELECT MIN(first_commit_date) FROM package_versions
    """)
    earliest_date = cursor.fetchone()[0]

    if not earliest_date:
        return CheckResult("first_commit_baseline", False, "No packages in database")

    # Count packages at the earliest date
    cursor.execute(
        """
        SELECT COUNT(DISTINCT attribute_path) FROM package_versions
        WHERE first_commit_date = ?
    """,
        (earliest_date,),
    )
    first_commit_packages = cursor.fetchone()[0]

    # Should have captured thousands of packages on first commit
    passed = first_commit_packages >= 1000
    return CheckResult(
        "first_commit_baseline",
        passed,
        f"{first_commit_packages} packages in first commit (need >= 1000)",
        {
            "first_commit_packages": first_commit_packages,
            "earliest_date": earliest_date,
        },
    )


def check_upsert_working(conn: sqlite3.Connection) -> CheckResult:
    """Check that UPSERT is working (packages have date spans)."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT
            SUM(CASE WHEN first_commit_date = last_commit_date THEN 1 ELSE 0 END) as single,
            SUM(CASE WHEN first_commit_date < last_commit_date THEN 1 ELSE 0 END) as multi,
            COUNT(*) as total
        FROM package_versions
    """)
    single, multi, total = cursor.fetchone()
    multi_pct = (multi / total * 100) if total > 0 else 0

    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE (last_commit_date - first_commit_date) > (30 * 86400)
    """)
    long_lived = cursor.fetchone()[0]

    passed = multi_pct > 50
    return CheckResult(
        "upsert_working",
        passed,
        f"{multi_pct:.1f}% have date spans, {long_lived} >30 days",
        {"single": single, "multi": multi, "long_lived": long_lived},
    )


def check_continuous_coverage(conn: sqlite3.Connection) -> CheckResult:
    """Check for gaps in date coverage (missing months).

    Large gaps could indicate the all-packages.nix issue recurring.
    """
    cursor = conn.cursor()

    # Get monthly package counts
    cursor.execute("""
        SELECT
            strftime('%Y-%m', datetime(first_commit_date, 'unixepoch')) as month,
            COUNT(DISTINCT attribute_path) as new_packages
        FROM package_versions
        GROUP BY month
        ORDER BY month
    """)
    monthly = cursor.fetchall()

    if len(monthly) < 2:
        return CheckResult(
            "continuous_coverage", True, "Not enough data for coverage check"
        )

    # Check for sudden drops (>90% decrease from previous month)
    gaps = []
    prev_count = monthly[0][1]

    for month, count in monthly[1:]:
        if count < prev_count * 0.1 and prev_count > 100:  # >90% drop
            gaps.append({"month": month, "count": count, "prev": prev_count})
        prev_count = max(count, prev_count)  # Use max to avoid cascading

    passed = len(gaps) == 0
    return CheckResult(
        "continuous_coverage",
        passed,
        f"{len(gaps)} coverage gaps detected" if gaps else "No major coverage gaps",
        {"gaps": gaps, "months_checked": len(monthly)},
        severity="warning" if not passed else "error",
    )


# =============================================================================
# GIT VERIFICATION CHECKS (requires --nixpkgs)
# =============================================================================


def verify_commit_exists(nixpkgs_path: Path, commit_hash: str) -> bool:
    """Check if a commit exists in nixpkgs."""
    try:
        result = subprocess.run(
            ["git", "cat-file", "-t", commit_hash],
            cwd=nixpkgs_path,
            capture_output=True,
            timeout=5,
        )
        return result.returncode == 0 and result.stdout.decode().strip() == "commit"
    except Exception:
        return False


def get_commit_date(nixpkgs_path: Path, commit_hash: str) -> str | None:
    """Get commit date from git."""
    try:
        result = subprocess.run(
            ["git", "log", "-1", "--format=%ci", commit_hash],
            cwd=nixpkgs_path,
            capture_output=True,
            timeout=5,
        )
        if result.returncode == 0:
            return result.stdout.decode().strip()
    except Exception:
        pass
    return None


def check_commit_exists_in_git(
    conn: sqlite3.Connection, nixpkgs_path: Path
) -> CheckResult:
    """Spot check that commit hashes exist in nixpkgs git repo."""
    cursor = conn.cursor()

    # Sample 20 random commits
    cursor.execute("""
        SELECT DISTINCT first_commit_hash FROM package_versions
        WHERE first_commit_hash IS NOT NULL
        ORDER BY RANDOM()
        LIMIT 20
    """)
    commits = [row[0] for row in cursor.fetchall()]

    found = 0
    missing = []

    for commit in commits:
        if verify_commit_exists(nixpkgs_path, commit):
            found += 1
        else:
            missing.append(commit[:12])

    passed = len(missing) == 0
    return CheckResult(
        "commits_exist_in_git",
        passed,
        f"{found}/{len(commits)} commits verified"
        + (f", missing: {missing[:3]}" if missing else ""),
        {"found": found, "missing": missing},
    )


def check_commit_dates_match(
    conn: sqlite3.Connection, nixpkgs_path: Path
) -> CheckResult:
    """Verify that stored commit dates match actual git commit dates."""
    cursor = conn.cursor()

    # Sample 10 commits with dates
    cursor.execute("""
        SELECT first_commit_hash, first_commit_date, attribute_path
        FROM package_versions
        WHERE first_commit_hash IS NOT NULL
        ORDER BY RANDOM()
        LIMIT 10
    """)
    samples = cursor.fetchall()

    matched = 0
    mismatches = []

    for commit_hash, stored_date, _ in samples:
        git_date_str = get_commit_date(nixpkgs_path, commit_hash)
        if git_date_str:
            # Parse git date and compare with stored epoch
            try:
                # Git format: "2017-01-02 06:29:48 +0100"
                git_date = datetime.strptime(git_date_str[:19], "%Y-%m-%d %H:%M:%S")
                stored_dt = datetime.fromtimestamp(
                    stored_date, tz=timezone.utc
                ).replace(tzinfo=None)

                # Allow 24 hour tolerance for timezone differences
                diff = abs((git_date - stored_dt).total_seconds())
                if diff < 86400:  # Within 24 hours
                    matched += 1
                else:
                    mismatches.append(
                        {
                            "commit": commit_hash[:12],
                            "stored": stored_dt.isoformat(),
                            "git": git_date.isoformat(),
                            "diff_hours": diff / 3600,
                        }
                    )
            except Exception:
                pass

    passed = len(mismatches) == 0
    return CheckResult(
        "commit_dates_match",
        passed,
        f"{matched}/{len(samples)} dates verified",
        {"matched": matched, "mismatches": mismatches[:3]},
        severity="warning" if not passed else "error",
    )


def check_all_packages_nix_extraction(
    conn: sqlite3.Connection, nixpkgs_path: Path
) -> CheckResult:
    """Verify that packages from all-packages.nix changes are being captured.

    This is the core check for the Issue #21 fix.
    """
    cursor = conn.cursor()

    # Get a few commits from our database
    cursor.execute("""
        SELECT DISTINCT first_commit_hash
        FROM package_versions
        WHERE first_commit_hash IS NOT NULL
        ORDER BY first_commit_date DESC
        LIMIT 5
    """)
    commits = [row[0] for row in cursor.fetchall()]

    if not commits:
        return CheckResult("all_packages_extraction", False, "No commits to check")

    verified = 0
    issues = []

    for commit in commits:
        # Check if this commit touched all-packages.nix
        try:
            result = subprocess.run(
                ["git", "diff-tree", "--no-commit-id", "--name-only", "-r", commit],
                cwd=nixpkgs_path,
                capture_output=True,
                timeout=5,
            )
            if result.returncode != 0:
                continue

            files = result.stdout.decode().strip().split("\n")
            touched_all_packages = any("all-packages.nix" in f for f in files)

            if touched_all_packages:
                # This commit touched all-packages.nix - verify we captured packages from it
                cursor.execute(
                    """
                    SELECT COUNT(*) FROM package_versions
                    WHERE first_commit_hash = ? OR last_commit_hash = ?
                """,
                    (commit, commit),
                )
                pkg_count = cursor.fetchone()[0]

                if pkg_count > 0:
                    verified += 1
                else:
                    issues.append(
                        f"{commit[:12]}: touched all-packages.nix but 0 packages"
                    )
        except Exception as e:
            issues.append(f"{commit[:12]}: error checking - {e}")

    passed = len(issues) == 0
    return CheckResult(
        "all_packages_extraction",
        passed,
        f"{verified} all-packages.nix commits verified"
        if passed
        else f"{len(issues)} issues",
        {"verified": verified, "issues": issues},
    )


def check_infrastructure_files_handling(
    conn: sqlite3.Connection, nixpkgs_path: Path
) -> CheckResult:
    """Check that INFRASTRUCTURE_FILES (all-packages.nix, aliases.nix) are handled correctly.

    Verifies the diff parsing fix is working.
    """
    cursor = conn.cursor()

    # Get recent commits
    cursor.execute("""
        SELECT DISTINCT first_commit_hash, first_commit_date
        FROM package_versions
        ORDER BY first_commit_date DESC
        LIMIT 20
    """)
    recent = cursor.fetchall()

    infrastructure_commits = 0
    captured_from_infra = 0

    for commit, _ in recent:
        try:
            result = subprocess.run(
                ["git", "diff-tree", "--no-commit-id", "--name-only", "-r", commit],
                cwd=nixpkgs_path,
                capture_output=True,
                timeout=5,
            )
            if result.returncode != 0:
                continue

            files = result.stdout.decode().strip().split("\n")
            infra_files = [
                f for f in files if "all-packages.nix" in f or "aliases.nix" in f
            ]

            if infra_files:
                infrastructure_commits += 1
                cursor.execute(
                    """
                    SELECT COUNT(*) FROM package_versions
                    WHERE first_commit_hash = ?
                """,
                    (commit,),
                )
                if cursor.fetchone()[0] > 0:
                    captured_from_infra += 1
        except Exception:
            continue

    if infrastructure_commits == 0:
        return CheckResult(
            "infrastructure_files",
            True,
            "No infrastructure file commits in sample",
            severity="info",
        )

    pct = captured_from_infra / infrastructure_commits * 100
    passed = pct >= 80  # At least 80% of infra commits should have packages

    return CheckResult(
        "infrastructure_files",
        passed,
        f"{captured_from_infra}/{infrastructure_commits} ({pct:.0f}%) infra commits captured packages",
        {"infra_commits": infrastructure_commits, "captured": captured_from_infra},
    )


# =============================================================================
# EDGE CASE CHECKS
# =============================================================================


def check_nested_packages(conn: sqlite3.Connection) -> CheckResult:
    """Check that nested packages (python3Packages.*, nodePackages.*) are captured."""
    cursor = conn.cursor()

    nested_patterns = [
        ("python3Packages.%", "Python packages"),
        ("nodePackages.%", "Node packages"),
        ("perlPackages.%", "Perl packages"),
        ("rubyGems.%", "Ruby gems"),
        ("haskellPackages.%", "Haskell packages"),
    ]

    results = {}
    for pattern, name in nested_patterns:
        cursor.execute(
            """
            SELECT COUNT(DISTINCT attribute_path) FROM package_versions
            WHERE attribute_path LIKE ?
        """,
            (pattern,),
        )
        count = cursor.fetchone()[0]
        results[name] = count

    total = sum(results.values())
    passed = total > 100  # Should have at least some nested packages

    return CheckResult(
        "nested_packages",
        passed,
        f"{total} nested packages found",
        {"breakdown": results},
    )


def check_special_version_formats(conn: sqlite3.Connection) -> CheckResult:
    """Check that special version formats are handled correctly."""
    cursor = conn.cursor()

    # Check for various version formats
    checks = {
        "semver": "SELECT COUNT(*) FROM package_versions WHERE version GLOB '[0-9]*.[0-9]*.[0-9]*'",
        "date_iso": "SELECT COUNT(*) FROM package_versions WHERE version GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'",
        "date_compact": "SELECT COUNT(*) FROM package_versions WHERE version GLOB '[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]'",
        "with_suffix": "SELECT COUNT(*) FROM package_versions WHERE version GLOB '*[a-z]' AND version NOT GLOB '*unknown*'",
    }

    results = {}
    for name, query in checks.items():
        cursor.execute(query)
        results[name] = cursor.fetchone()[0]

    # Should have semver versions at minimum
    passed = results["semver"] > 0

    return CheckResult(
        "special_versions",
        passed,
        f"semver={results['semver']}, date={results['date_iso']}, compact={results['date_compact']}",
        {"formats": results},
    )


def check_store_paths(conn: sqlite3.Connection) -> CheckResult:
    """Check store paths are valid when present (only for post-2020 data)."""
    cursor = conn.cursor()

    # Store paths only populated for commits >= 2020-01-01
    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE store_path_x86_64_linux IS NOT NULL AND store_path_x86_64_linux != ''
    """)
    with_paths = cursor.fetchone()[0]

    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE store_path_x86_64_linux IS NOT NULL
          AND store_path_x86_64_linux != ''
          AND store_path_x86_64_linux NOT LIKE '/nix/store/%'
    """)
    invalid = cursor.fetchone()[0]

    passed = invalid == 0
    return CheckResult(
        "store_paths",
        passed,
        f"{with_paths} store paths, {invalid} invalid"
        if with_paths > 0
        else "No store paths yet (pre-2020 data)",
        {"with_paths": with_paths, "invalid": invalid},
    )


def check_date_range_reasonable(conn: sqlite3.Connection) -> CheckResult:
    """Check date range is reasonable."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT
            datetime(MIN(first_commit_date), 'unixepoch') as earliest,
            datetime(MAX(last_commit_date), 'unixepoch') as latest,
            ROUND((MAX(last_commit_date) - MIN(first_commit_date)) / 86400.0, 1) as days
        FROM package_versions
    """)
    earliest, latest, days = cursor.fetchone()

    passed = True
    issues = []

    if earliest:
        earliest_dt = datetime.strptime(earliest, "%Y-%m-%d %H:%M:%S")
        if earliest_dt.year < 2014:
            issues.append(f"earliest too old ({earliest})")
            passed = False

    if latest:
        latest_dt = datetime.strptime(latest, "%Y-%m-%d %H:%M:%S")
        if latest_dt.year > datetime.now().year + 1:
            issues.append(f"latest in future ({latest})")
            passed = False

    return CheckResult(
        "date_range",
        passed,
        f"{earliest[:10] if earliest else '?'} to {latest[:10] if latest else '?'} ({days} days)",
        {"earliest": earliest, "latest": latest, "days": days},
    )


# =============================================================================
# CRITICAL PACKAGES CHECK
# =============================================================================


def check_critical_packages(conn: sqlite3.Connection) -> CheckResult:
    """Check that critical packages have versions."""
    # Extended list covering different categories
    critical = {
        "core": ["hello", "coreutils", "bash", "gzip", "tar"],
        "browsers": ["firefox", "chromium"],
        "languages": ["python3", "nodejs", "rustc", "go", "gcc"],
        "tools": ["git", "curl", "vim", "ripgrep"],
        "databases": ["postgresql", "sqlite", "redis"],
    }

    cursor = conn.cursor()
    found = {}
    missing = []

    for category, packages in critical.items():
        for pkg in packages:
            cursor.execute(
                """
                SELECT COUNT(*) FROM package_versions
                WHERE attribute_path = ? AND version != 'unknown' AND version != ''
            """,
                (pkg,),
            )
            count = cursor.fetchone()[0]
            if count > 0:
                found[pkg] = count
            else:
                missing.append(f"{pkg} ({category})")

    total_expected = sum(len(pkgs) for pkgs in critical.values())
    passed = len(missing) <= total_expected * 0.2  # Allow up to 20% missing

    return CheckResult(
        "critical_packages",
        passed,
        f"{len(found)}/{total_expected} critical packages found",
        {"found": found, "missing": missing},
    )


# =============================================================================
# MAIN RUNNER
# =============================================================================


def run_sanity_checks(
    db_path: Path, nixpkgs_path: Path | None = None, verbose: bool = False
) -> SanityReport:
    """Run all sanity checks."""
    report = SanityReport()
    conn = sqlite3.connect(db_path)

    # Basic integrity checks
    basic_checks = [
        ("Duplicates", check_duplicates),
        ("Date Integrity", check_date_integrity),
        ("Schema Version", check_schema_version),
        ("Commit Hash Format", check_commit_hash_format),
        ("FTS Sync", check_fts_sync),
        ("Date Range", check_date_range_reasonable),
    ]

    # Version extraction checks (Issue #21 fixes)
    extraction_checks = [
        ("Version Source Distribution", check_version_source_distribution),
        ("Unknown Versions", check_unknown_versions),
        ("Wrapper Versions", check_wrapper_versions),
        ("Name-Based Extraction", check_name_based_extraction),
    ]

    # All-packages.nix handling checks (Issue #21 primary fix)
    allpkgs_checks = [
        ("CallPackage Packages", check_callpackage_packages),
        ("First Commit Baseline", check_first_commit_baseline),
        ("UPSERT Working", check_upsert_working),
        ("Continuous Coverage", check_continuous_coverage),
    ]

    # Edge case checks
    edge_checks = [
        ("Nested Packages", check_nested_packages),
        ("Special Versions", check_special_version_formats),
        ("Store Paths", check_store_paths),
        ("Critical Packages", check_critical_packages),
    ]

    all_checks = [
        ("BASIC INTEGRITY", basic_checks),
        ("VERSION EXTRACTION (Issue #21)", extraction_checks),
        ("ALL-PACKAGES.NIX HANDLING (Issue #21)", allpkgs_checks),
        ("EDGE CASES", edge_checks),
    ]

    for section, checks in all_checks:
        if verbose:
            print(f"\n=== {section} ===")
        for name, check_fn in checks:
            try:
                result = check_fn(conn)
                report.add(result)
                if verbose:
                    status = (
                        "PASS"
                        if result.passed
                        else ("WARN" if result.severity == "warning" else "FAIL")
                    )
                    print(f"[{status}] {name}: {result.message}")
            except Exception as e:
                report.add(CheckResult(name, False, f"Error: {e}"))
                if verbose:
                    print(f"[ERROR] {name}: {e}")

    # Git-based checks (require nixpkgs)
    if nixpkgs_path and nixpkgs_path.exists():
        git_checks = [
            (
                "Commits Exist in Git",
                lambda c: check_commit_exists_in_git(c, nixpkgs_path),
            ),
            ("Commit Dates Match", lambda c: check_commit_dates_match(c, nixpkgs_path)),
            (
                "All-Packages.nix Extraction",
                lambda c: check_all_packages_nix_extraction(c, nixpkgs_path),
            ),
            (
                "Infrastructure Files Handling",
                lambda c: check_infrastructure_files_handling(c, nixpkgs_path),
            ),
        ]

        if verbose:
            print(f"\n=== GIT VERIFICATION (nixpkgs: {nixpkgs_path}) ===")

        for name, check_fn in git_checks:
            try:
                result = check_fn(conn)
                report.add(result)
                if verbose:
                    status = (
                        "PASS"
                        if result.passed
                        else ("WARN" if result.severity == "warning" else "FAIL")
                    )
                    print(f"[{status}] {name}: {result.message}")
            except Exception as e:
                report.add(CheckResult(name, False, f"Error: {e}"))
                if verbose:
                    print(f"[ERROR] {name}: {e}")

    conn.close()
    return report


def print_report(report: SanityReport, nixpkgs_path: Path | None):
    """Print human-readable report."""
    print("\n" + "=" * 70)
    print("NXV DATABASE SANITY CHECK")
    print("=" * 70)

    for check in report.checks:
        status = (
            "PASS"
            if check.passed
            else ("WARN" if check.severity == "warning" else "FAIL")
        )
        print(f"[{status}] {check.name}: {check.message}")

    print("\n" + "-" * 70)
    print(
        f"SUMMARY: {report.passed} passed, {report.failed} failed, {report.warnings} warnings"
    )
    print("-" * 70)

    if report.failed == 0 and report.warnings == 0:
        print("\nAll sanity checks passed!")
    elif report.failed == 0:
        print(f"\nPassed with {report.warnings} warning(s)")
    else:
        print(f"\n{report.failed} check(s) failed - review above for details")

    if not nixpkgs_path:
        print("\nTip: Run with --nixpkgs PATH for git-based verification")


def main():
    parser = argparse.ArgumentParser(description="Sanity check nxv database integrity")
    parser.add_argument(
        "--db",
        type=Path,
        default=Path.home() / ".local/share/nxv/index.db",
        help="Path to nxv database",
    )
    parser.add_argument(
        "--nixpkgs",
        type=Path,
        default=None,
        help="Path to nixpkgs clone for git verification",
    )
    parser.add_argument("--verbose", "-v", action="store_true", help="Show progress")
    parser.add_argument("--json", action="store_true", help="Output JSON")

    args = parser.parse_args()

    if not args.db.exists():
        print(f"Error: database not found: {args.db}", file=sys.stderr)
        sys.exit(1)

    # Try to find nixpkgs in common locations
    nixpkgs_path = args.nixpkgs
    if not nixpkgs_path:
        for candidate in [
            Path("./nixpkgs"),
            Path("../nixpkgs"),
            Path.home() / "nixpkgs",
        ]:
            if candidate.exists() and (candidate / ".git").exists():
                nixpkgs_path = candidate
                break

    print(f"Database: {args.db}")
    if nixpkgs_path:
        print(f"Nixpkgs: {nixpkgs_path}")

    report = run_sanity_checks(args.db, nixpkgs_path, args.verbose)

    if args.json:
        output = {
            "passed": report.passed,
            "failed": report.failed,
            "warnings": report.warnings,
            "checks": [
                {
                    "name": c.name,
                    "passed": c.passed,
                    "message": c.message,
                    "severity": c.severity,
                    "details": c.details,
                }
                for c in report.checks
            ],
        }
        print(json.dumps(output, indent=2))
    else:
        print_report(report, nixpkgs_path)

    sys.exit(0 if report.failed == 0 else 1)


if __name__ == "__main__":
    main()
