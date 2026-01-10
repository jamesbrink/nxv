#!/usr/bin/env python3
"""
Sanity check script for nxv database integrity.

Validates internal data quality without external API calls.

Usage:
    python scripts/sanity_check.py [--db PATH] [--verbose] [--json]
"""

import argparse
import json
import sqlite3
import sys
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path


@dataclass
class CheckResult:
    name: str
    passed: bool
    message: str
    details: dict = field(default_factory=dict)


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
        else:
            self.failed += 1


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
        "no_duplicates", False,
        f"Found {len(duplicates)} duplicate pairs",
        {"duplicates": [{"attr": d[0], "version": d[1], "count": d[2]} for d in duplicates]}
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
        return CheckResult("date_integrity", True, "All date ranges valid (first <= last)")

    cursor.execute("""
        SELECT attribute_path, version, first_commit_date, last_commit_date
        FROM package_versions
        WHERE first_commit_date > last_commit_date
        LIMIT 5
    """)
    examples = cursor.fetchall()

    return CheckResult(
        "date_integrity", False,
        f"{invalid_count} rows have first_commit_date > last_commit_date",
        {"examples": [{"attr": e[0], "version": e[1]} for e in examples]}
    )


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
    distribution = {row[0] or "NULL": {"count": row[1], "pct": row[2]} for row in cursor.fetchall()}

    # Check that direct extraction is reasonable (>30%)
    direct_pct = distribution.get("direct", {}).get("pct", 0)
    null_pct = distribution.get("NULL", {}).get("pct", 0) + distribution.get("", {}).get("pct", 0)

    passed = direct_pct >= 30 and null_pct < 5
    msg = f"direct={direct_pct}%, null/empty={null_pct}%"

    return CheckResult("version_source_distribution", passed, msg, {"distribution": distribution})


def check_unknown_versions(conn: sqlite3.Connection) -> CheckResult:
    """Check rate of unknown/empty versions."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT
            COUNT(*) as total,
            SUM(CASE WHEN version = 'unknown' OR version IS NULL OR version = '' THEN 1 ELSE 0 END) as unknown
        FROM package_versions
    """)
    total, unknown = cursor.fetchone()
    pct = (unknown / total * 100) if total > 0 else 0

    passed = pct < 5  # Less than 5% unknown is acceptable
    return CheckResult(
        "unknown_versions", passed,
        f"{unknown}/{total} ({pct:.2f}%) unknown/empty versions",
        {"total": total, "unknown": unknown, "pct": pct}
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
    invalid_first = cursor.fetchone()[0]

    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE last_commit_hash IS NOT NULL
          AND (LENGTH(last_commit_hash) != 40
               OR last_commit_hash GLOB '*[^0-9a-f]*')
    """)
    invalid_last = cursor.fetchone()[0]

    total_invalid = invalid_first + invalid_last
    passed = total_invalid == 0

    return CheckResult(
        "commit_hash_format", passed,
        f"{total_invalid} invalid commit hashes" if not passed else "All commit hashes valid (40-char hex)",
        {"invalid_first": invalid_first, "invalid_last": invalid_last}
    )


def check_critical_packages(conn: sqlite3.Connection) -> CheckResult:
    """Check that critical packages have versions."""
    critical = ["firefox", "chromium", "python3", "nodejs", "gcc", "hello", "git", "curl", "vim"]
    cursor = conn.cursor()

    missing = []
    found = {}
    for pkg in critical:
        cursor.execute("""
            SELECT COUNT(*) FROM package_versions
            WHERE attribute_path = ? AND version != 'unknown' AND version != ''
        """, (pkg,))
        count = cursor.fetchone()[0]
        if count == 0:
            missing.append(pkg)
        else:
            found[pkg] = count

    passed = len(missing) == 0
    return CheckResult(
        "critical_packages", passed,
        f"Missing: {missing}" if missing else f"All {len(critical)} critical packages present",
        {"found": found, "missing": missing}
    )


def check_upsert_working(conn: sqlite3.Connection) -> CheckResult:
    """Check that UPSERT is working (packages have date spans)."""
    cursor = conn.cursor()

    # Count packages with first != last (seen in multiple commits)
    cursor.execute("""
        SELECT
            SUM(CASE WHEN first_commit_date = last_commit_date THEN 1 ELSE 0 END) as single,
            SUM(CASE WHEN first_commit_date < last_commit_date THEN 1 ELSE 0 END) as multi,
            COUNT(*) as total
        FROM package_versions
    """)
    single, multi, total = cursor.fetchone()
    multi_pct = (multi / total * 100) if total > 0 else 0

    # Also check for long-lived packages (>30 days)
    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE (last_commit_date - first_commit_date) > (30 * 86400)
    """)
    long_lived = cursor.fetchone()[0]

    passed = multi_pct > 50  # At least 50% should have date spans
    return CheckResult(
        "upsert_working", passed,
        f"{multi_pct:.1f}% have date spans, {long_lived} packages >30 days",
        {"single_commit": single, "multi_commit": multi, "long_lived_30d": long_lived}
    )


def check_schema_version(conn: sqlite3.Connection) -> CheckResult:
    """Check schema version is expected."""
    cursor = conn.cursor()
    cursor.execute("SELECT value FROM meta WHERE key = 'schema_version'")
    row = cursor.fetchone()

    if not row:
        return CheckResult("schema_version", False, "No schema_version in meta table")

    version = int(row[0])
    expected = 4  # Current schema version
    passed = version == expected

    return CheckResult(
        "schema_version", passed,
        f"Schema version {version}" + ("" if passed else f" (expected {expected})"),
        {"version": version, "expected": expected}
    )


def check_date_range(conn: sqlite3.Connection) -> CheckResult:
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

    # Parse dates to check reasonability
    earliest_dt = datetime.strptime(earliest, "%Y-%m-%d %H:%M:%S") if earliest else None
    latest_dt = datetime.strptime(latest, "%Y-%m-%d %H:%M:%S") if latest else None

    passed = True
    issues = []

    if earliest_dt and earliest_dt.year < 2014:
        passed = False
        issues.append(f"earliest date too old ({earliest})")

    if latest_dt and latest_dt.year > datetime.now().year + 1:
        passed = False
        issues.append(f"latest date in future ({latest})")

    return CheckResult(
        "date_range", passed,
        f"{earliest} to {latest} ({days} days)" + (f" - {issues}" if issues else ""),
        {"earliest": earliest, "latest": latest, "days_covered": days}
    )


def check_fts_sync(conn: sqlite3.Connection) -> CheckResult:
    """Check FTS index is in sync with main table."""
    cursor = conn.cursor()

    cursor.execute("SELECT COUNT(*) FROM package_versions")
    main_count = cursor.fetchone()[0]

    cursor.execute("SELECT COUNT(*) FROM package_versions_fts")
    fts_count = cursor.fetchone()[0]

    passed = main_count == fts_count
    return CheckResult(
        "fts_sync", passed,
        f"FTS has {fts_count} rows, main has {main_count}" if not passed else f"FTS in sync ({fts_count} rows)",
        {"main_count": main_count, "fts_count": fts_count}
    )


def check_store_path_format(conn: sqlite3.Connection) -> CheckResult:
    """Check store paths have valid format when present."""
    cursor = conn.cursor()
    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE store_path_x86_64_linux IS NOT NULL
          AND store_path_x86_64_linux != ''
          AND store_path_x86_64_linux NOT LIKE '/nix/store/%'
    """)
    invalid = cursor.fetchone()[0]

    cursor.execute("""
        SELECT COUNT(*) FROM package_versions
        WHERE store_path_x86_64_linux IS NOT NULL AND store_path_x86_64_linux != ''
    """)
    total_with_paths = cursor.fetchone()[0]

    passed = invalid == 0
    return CheckResult(
        "store_path_format", passed,
        f"{invalid} invalid store paths" if not passed else f"All {total_with_paths} store paths valid",
        {"invalid": invalid, "total_with_paths": total_with_paths}
    )


def run_sanity_checks(db_path: Path, verbose: bool = False) -> SanityReport:
    """Run all sanity checks."""
    report = SanityReport()

    conn = sqlite3.connect(db_path)

    checks = [
        ("Duplicates", check_duplicates),
        ("Date Integrity", check_date_integrity),
        ("Version Source", check_version_source_distribution),
        ("Unknown Versions", check_unknown_versions),
        ("Commit Hash Format", check_commit_hash_format),
        ("Critical Packages", check_critical_packages),
        ("UPSERT Working", check_upsert_working),
        ("Schema Version", check_schema_version),
        ("Date Range", check_date_range),
        ("FTS Sync", check_fts_sync),
        ("Store Path Format", check_store_path_format),
    ]

    for name, check_fn in checks:
        try:
            result = check_fn(conn)
            report.add(result)
            if verbose:
                status = "PASS" if result.passed else "FAIL"
                print(f"[{status}] {name}: {result.message}")
        except Exception as e:
            report.add(CheckResult(name, False, f"Error: {e}"))
            if verbose:
                print(f"[ERROR] {name}: {e}")

    conn.close()
    return report


def print_report(report: SanityReport):
    """Print human-readable report."""
    print("\n" + "=" * 60)
    print("NXV DATABASE SANITY CHECK")
    print("=" * 60)

    for check in report.checks:
        status = "PASS" if check.passed else "FAIL"
        print(f"\n[{status}] {check.name}")
        print(f"       {check.message}")

    print("\n" + "-" * 60)
    print(f"SUMMARY: {report.passed} passed, {report.failed} failed")
    print("-" * 60)

    if report.failed == 0:
        print("\nAll sanity checks passed!")
    else:
        print(f"\n{report.failed} check(s) failed - review above for details")


def main():
    parser = argparse.ArgumentParser(description="Sanity check nxv database integrity")
    parser.add_argument(
        "--db", type=Path,
        default=Path.home() / ".local/share/nxv/index.db",
        help="Path to nxv database"
    )
    parser.add_argument("--verbose", "-v", action="store_true", help="Show progress")
    parser.add_argument("--json", action="store_true", help="Output JSON")

    args = parser.parse_args()

    if not args.db.exists():
        print(f"Error: database not found: {args.db}", file=sys.stderr)
        sys.exit(1)

    print(f"Database: {args.db}")
    report = run_sanity_checks(args.db, args.verbose)

    if args.json:
        output = {
            "passed": report.passed,
            "failed": report.failed,
            "checks": [
                {
                    "name": c.name,
                    "passed": c.passed,
                    "message": c.message,
                    "details": c.details
                }
                for c in report.checks
            ]
        }
        print(json.dumps(output, indent=2))
    else:
        print_report(report)

    sys.exit(0 if report.failed == 0 else 1)


if __name__ == "__main__":
    main()
