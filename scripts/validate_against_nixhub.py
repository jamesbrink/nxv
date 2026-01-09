#!/usr/bin/env python3
"""
Validation script to compare nxv database against NixHub API.

This script validates:
1. Version completeness: Are all versions from NixHub present in nxv?
2. Commit hash accuracy: Do our commit hashes match NixHub's?
3. Store path accuracy: Do our store paths match NixHub's?
4. Binary cache availability: Are the store paths available in cache.nixos.org?

Usage:
    python scripts/validate_against_nixhub.py [options]

Options:
    --db PATH           Path to nxv SQLite database (default: ~/.local/share/nxv/index.db)
    --packages LIST     Comma-separated package names to validate (default: sample set)
    --sample N          Validate N random packages from database (default: 0)
    --check-cache       Also check if store paths exist in cache.nixos.org
    --output FILE       Output JSON report to file
    --verbose           Show detailed output
    --before DATE       Only consider versions before this date (YYYY-MM-DD)
    --after DATE        Only consider versions after this date (YYYY-MM-DD)
    --edge-cases        Include edge case packages (nested, special chars, etc.)
    --comprehensive     Run comprehensive test with 100 random + edge cases

Examples:
    # Validate specific packages
    python scripts/validate_against_nixhub.py --packages thunderbird,firefox,chromium

    # Validate 50 random packages
    python scripts/validate_against_nixhub.py --sample 50

    # Validate pre-July 2023 data only
    python scripts/validate_against_nixhub.py --before 2023-07-25 --sample 50

    # Full validation with cache check
    python scripts/validate_against_nixhub.py --packages hello --check-cache --verbose

    # Comprehensive validation
    python scripts/validate_against_nixhub.py --comprehensive --before 2023-07-25
"""

import argparse
import json
import sqlite3
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional
from urllib.request import urlopen, Request
from urllib.error import HTTPError, URLError


# NixHub API base URL
NIXHUB_API = "https://search.devbox.sh"

# Default packages to validate (mix of simple and complex)
DEFAULT_PACKAGES = [
    "thunderbird",
    "firefox",
    "chromium",
    "hello",
    "git",
    "vim",
    "neovim",
    "python3",
    "nodejs",
    "rustc",
    "go",
    "curl",
    "wget",
    "htop",
    "ripgrep",
]

# Edge case packages to test various scenarios
EDGE_CASE_PACKAGES = [
    # Simple, stable packages (should have high accuracy)
    "hello",
    "coreutils",
    "bash",
    "gzip",
    "tar",
    # Frequently updated packages
    "linux",
    "firefox",
    "chromium",
    "thunderbird",
    # Language ecosystems
    "python3",
    "nodejs",
    "rustc",
    "go",
    "ruby",
    "php",
    # Packages with complex versioning
    "gcc",
    "llvm",
    "clang",
    "openjdk",
    # Packages with multiple variants
    "postgresql",
    "mysql",
    "redis",
    "nginx",
    # Desktop applications
    "vscode",
    "slack",
    "discord",
    "spotify",
    # CLI tools
    "ripgrep",
    "fd",
    "bat",
    "exa",
    "fzf",
    "jq",
    "yq",
    # Build tools
    "cmake",
    "meson",
    "ninja",
    "bazel",
    # Network tools
    "curl",
    "wget",
    "httpie",
    "nmap",
    # Editors
    "vim",
    "neovim",
    "emacs",
    # Version control
    "git",
    "mercurial",
    "subversion",
    # Containers
    "docker",
    "podman",
    "kubernetes",
    # Databases
    "sqlite",
    "mongodb",
    "elasticsearch",
]

# Categories for reporting
PACKAGE_CATEGORIES = {
    "stable": ["hello", "coreutils", "bash", "gzip", "tar"],
    "browsers": ["firefox", "chromium", "thunderbird"],
    "languages": ["python3", "nodejs", "rustc", "go", "ruby", "php"],
    "compilers": ["gcc", "llvm", "clang", "openjdk"],
    "databases": ["postgresql", "mysql", "redis", "sqlite", "mongodb"],
    "cli_tools": ["ripgrep", "fd", "bat", "fzf", "jq", "curl", "wget"],
    "editors": ["vim", "neovim", "emacs", "vscode"],
    "devops": ["docker", "podman", "kubernetes", "nginx"],
}


@dataclass
class VersionComparison:
    """Comparison of a single version between nxv and NixHub."""

    version: str
    in_nxv: bool = False
    in_nixhub: bool = False
    nxv_commit: Optional[str] = None
    nixhub_commit: Optional[str] = None
    commit_match: Optional[bool] = None
    nxv_store_path: Optional[str] = None
    nixhub_store_path: Optional[str] = None
    store_path_match: Optional[bool] = None
    cache_available: Optional[bool] = None


@dataclass
class PackageValidation:
    """Validation results for a single package."""

    package_name: str
    nxv_version_count: int = 0
    nixhub_version_count: int = 0
    versions_only_in_nxv: list[str] = field(default_factory=list)
    versions_only_in_nixhub: list[str] = field(default_factory=list)
    versions_in_both: list[str] = field(default_factory=list)
    commit_mismatches: list[VersionComparison] = field(default_factory=list)
    store_path_mismatches: list[VersionComparison] = field(default_factory=list)
    cache_unavailable: list[str] = field(default_factory=list)
    completeness_ratio: float = 0.0
    error: Optional[str] = None


@dataclass
class ValidationReport:
    """Overall validation report."""

    packages_validated: int = 0
    packages_with_errors: int = 0
    total_versions_nxv: int = 0
    total_versions_nixhub: int = 0
    total_missing_from_nxv: int = 0
    total_commit_mismatches: int = 0
    total_store_path_mismatches: int = 0
    total_cache_unavailable: int = 0
    avg_completeness: float = 0.0
    package_validations: list[PackageValidation] = field(default_factory=list)


def fetch_json(url: str, retries: int = 3, delay: float = 1.0) -> dict:
    """Fetch JSON from URL with retries."""
    last_error: Optional[Exception] = None
    for attempt in range(retries):
        try:
            req = Request(url, headers={"User-Agent": "nxv-validator/1.0"})
            with urlopen(req, timeout=30) as response:
                return json.loads(response.read().decode("utf-8"))
        except (HTTPError, URLError, json.JSONDecodeError) as e:
            last_error = e
            if attempt < retries - 1:
                time.sleep(delay * (attempt + 1))
    if last_error:
        raise last_error
    raise RuntimeError("Failed to fetch JSON after retries")


def check_cache_availability(store_path: str) -> bool:
    """Check if a store path is available in cache.nixos.org."""
    if not store_path or not store_path.startswith("/nix/store/"):
        return False

    # Extract hash from store path: /nix/store/<hash>-<name>
    parts = store_path.split("/")
    if len(parts) < 4:
        return False

    store_name = parts[3]  # <hash>-<name>
    if "-" not in store_name:
        return False

    store_hash = store_name.split("-")[0]
    url = f"https://cache.nixos.org/{store_hash}.narinfo"

    try:
        req = Request(url, method="HEAD", headers={"User-Agent": "nxv-validator/1.0"})
        with urlopen(req, timeout=10) as response:
            return response.status == 200
    except HTTPError as e:
        return e.code != 404
    except URLError:
        return False


def get_nxv_versions(
    db_path: Path,
    package_name: str,
    before_date: Optional[str] = None,
    after_date: Optional[str] = None,
) -> dict[str, dict]:
    """Get all versions of a package from nxv database.

    Returns dict mapping version -> {commit_hash, store_path, date, ...}
    """
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Build query with optional date filters
    query = """
        SELECT DISTINCT version, first_commit_hash, store_path_x86_64_linux,
               datetime(first_commit_date, 'unixepoch') as commit_date
        FROM package_versions
        WHERE (attribute_path = ? OR name = ?)
    """
    params: list = [package_name, package_name]

    if before_date:
        query += " AND first_commit_date < strftime('%s', ?)"
        params.append(before_date)

    if after_date:
        query += " AND first_commit_date >= strftime('%s', ?)"
        params.append(after_date)

    query += " ORDER BY first_commit_date DESC"

    cursor.execute(query, params)

    versions = {}
    for row in cursor.fetchall():
        version = row["version"]
        if version and version != "unknown":
            versions[version] = {
                "commit_hash": row["first_commit_hash"],
                "store_path": row["store_path_x86_64_linux"],
                "date": row["commit_date"],
            }

    conn.close()
    return versions


def get_nixhub_versions(
    package_name: str,
    before_date: Optional[str] = None,
    after_date: Optional[str] = None,
) -> dict[str, dict]:
    """Get all versions of a package from NixHub API.

    Returns dict mapping version -> {commit_hash, store_path, date, ...}
    """
    url = f"{NIXHUB_API}/v2/pkg?name={package_name}"
    try:
        data = fetch_json(url)
    except Exception as e:
        raise Exception(f"Failed to fetch from NixHub: {e}")

    versions = {}
    releases = data.get("releases", [])

    for release in releases:
        version = release.get("version")
        if not version:
            continue

        last_updated = release.get("last_updated", "")

        # Filter by date if specified
        if before_date and last_updated:
            if last_updated >= before_date:
                continue
        if after_date and last_updated:
            if last_updated < after_date:
                continue

        # Get x86_64-linux platform info (our default)
        platforms = release.get("platforms", [])
        linux_platform = None
        for p in platforms:
            if p.get("system") == "x86_64-linux":
                linux_platform = p
                break

        if linux_platform:
            store_path = None
            outputs = linux_platform.get("outputs") or []
            for output in outputs:
                if output and (output.get("default") or output.get("name") == "out"):
                    store_path = output.get("path")
                    break

            versions[version] = {
                "commit_hash": linux_platform.get("commit_hash"),
                "store_path": store_path,
                "date": linux_platform.get("date") or last_updated,
            }
        else:
            # No x86_64-linux, but record the version exists
            versions[version] = {
                "commit_hash": None,
                "store_path": None,
                "date": last_updated,
            }

    return versions


def validate_package(
    db_path: Path,
    package_name: str,
    check_cache: bool = False,
    verbose: bool = False,
    before_date: Optional[str] = None,
    after_date: Optional[str] = None,
) -> PackageValidation:
    """Validate a single package against NixHub."""
    validation = PackageValidation(package_name=package_name)

    if verbose:
        date_range = ""
        if before_date:
            date_range += f" (before {before_date})"
        if after_date:
            date_range += f" (after {after_date})"
        print(f"\nValidating: {package_name}{date_range}")

    # Get versions from both sources
    try:
        nxv_versions = get_nxv_versions(db_path, package_name, before_date, after_date)
        validation.nxv_version_count = len(nxv_versions)
        if verbose:
            print(f"  nxv: {len(nxv_versions)} versions")
    except Exception as e:
        validation.error = f"nxv error: {e}"
        if verbose:
            print(f"  ERROR (nxv): {e}")
        return validation

    try:
        nixhub_versions = get_nixhub_versions(package_name, before_date, after_date)
        validation.nixhub_version_count = len(nixhub_versions)
        if verbose:
            print(f"  NixHub: {len(nixhub_versions)} versions")
    except Exception as e:
        validation.error = f"NixHub error: {e}"
        if verbose:
            print(f"  ERROR (NixHub): {e}")
        return validation

    # Compare versions
    nxv_set = set(nxv_versions.keys())
    nixhub_set = set(nixhub_versions.keys())

    validation.versions_only_in_nxv = sorted(nxv_set - nixhub_set)
    validation.versions_only_in_nixhub = sorted(nixhub_set - nxv_set)
    validation.versions_in_both = sorted(nxv_set & nixhub_set)

    if verbose:
        if validation.versions_only_in_nixhub:
            print(f"  Missing from nxv: {len(validation.versions_only_in_nixhub)}")
            for v in validation.versions_only_in_nixhub[:5]:
                print(f"    - {v}")
            if len(validation.versions_only_in_nixhub) > 5:
                print(f"    ... and {len(validation.versions_only_in_nixhub) - 5} more")

    # Compare commit hashes and store paths for versions in both
    for version in validation.versions_in_both:
        nxv_data = nxv_versions[version]
        nixhub_data = nixhub_versions[version]

        comparison = VersionComparison(
            version=version,
            in_nxv=True,
            in_nixhub=True,
            nxv_commit=nxv_data.get("commit_hash"),
            nixhub_commit=nixhub_data.get("commit_hash"),
            nxv_store_path=nxv_data.get("store_path"),
            nixhub_store_path=nixhub_data.get("store_path"),
        )

        # Check commit hash match
        if comparison.nxv_commit and comparison.nixhub_commit:
            comparison.commit_match = comparison.nxv_commit == comparison.nixhub_commit
            if not comparison.commit_match:
                validation.commit_mismatches.append(comparison)

        # Check store path match
        if comparison.nxv_store_path and comparison.nixhub_store_path:
            comparison.store_path_match = (
                comparison.nxv_store_path == comparison.nixhub_store_path
            )
            if not comparison.store_path_match:
                validation.store_path_mismatches.append(comparison)

        # Check cache availability
        if check_cache and comparison.nixhub_store_path:
            comparison.cache_available = check_cache_availability(
                comparison.nixhub_store_path
            )
            if not comparison.cache_available:
                validation.cache_unavailable.append(version)

    # Calculate completeness ratio
    if nixhub_set:
        validation.completeness_ratio = len(nxv_set & nixhub_set) / len(nixhub_set)

    if verbose:
        print(f"  Completeness: {validation.completeness_ratio:.1%}")
        if validation.commit_mismatches:
            print(f"  Commit mismatches: {len(validation.commit_mismatches)}")
        if validation.store_path_mismatches:
            print(f"  Store path mismatches: {len(validation.store_path_mismatches)}")

    return validation


def get_random_packages(db_path: Path, count: int) -> list[str]:
    """Get random package names from the database."""
    conn = sqlite3.connect(db_path)
    cursor = conn.cursor()

    cursor.execute(
        """
        SELECT DISTINCT attribute_path
        FROM package_versions
        WHERE attribute_path NOT LIKE '%.%'  -- Skip nested packages
          AND attribute_path NOT LIKE '%-%unwrapped'
          AND version != 'unknown'
        ORDER BY RANDOM()
        LIMIT ?
        """,
        (count,),
    )

    packages = [row[0] for row in cursor.fetchall()]
    conn.close()
    return packages


def generate_report(validations: list[PackageValidation]) -> ValidationReport:
    """Generate a summary report from all validations."""
    report = ValidationReport()
    report.package_validations = validations
    report.packages_validated = len(validations)

    total_completeness = 0.0

    for v in validations:
        if v.error:
            report.packages_with_errors += 1
            continue

        report.total_versions_nxv += v.nxv_version_count
        report.total_versions_nixhub += v.nixhub_version_count
        report.total_missing_from_nxv += len(v.versions_only_in_nixhub)
        report.total_commit_mismatches += len(v.commit_mismatches)
        report.total_store_path_mismatches += len(v.store_path_mismatches)
        report.total_cache_unavailable += len(v.cache_unavailable)
        total_completeness += v.completeness_ratio

    valid_count = report.packages_validated - report.packages_with_errors
    if valid_count > 0:
        report.avg_completeness = total_completeness / valid_count

    return report


def print_report(report: ValidationReport):
    """Print a human-readable report."""
    print("\n" + "=" * 70)
    print("NXV vs NIXHUB VALIDATION REPORT")
    print("=" * 70)

    print(f"\nPackages validated: {report.packages_validated}")
    print(f"Packages with errors: {report.packages_with_errors}")

    print(f"\nVersion Counts:")
    print(f"  - Total in nxv: {report.total_versions_nxv}")
    print(f"  - Total in NixHub: {report.total_versions_nixhub}")
    print(f"  - Missing from nxv: {report.total_missing_from_nxv}")

    print(f"\nData Quality:")
    print(f"  - Commit hash mismatches: {report.total_commit_mismatches}")
    print(f"  - Store path mismatches: {report.total_store_path_mismatches}")
    print(f"  - Cache unavailable: {report.total_cache_unavailable}")

    print(f"\nAverage completeness: {report.avg_completeness:.1%}")

    # Show worst packages
    print("\n" + "-" * 70)
    print("PACKAGES WITH MOST MISSING VERSIONS:")
    print("-" * 70)

    sorted_packages = sorted(
        [v for v in report.package_validations if not v.error],
        key=lambda v: len(v.versions_only_in_nixhub),
        reverse=True,
    )

    for v in sorted_packages[:10]:
        missing = len(v.versions_only_in_nixhub)
        if missing > 0:
            print(f"\n{v.package_name}:")
            print(f"  nxv: {v.nxv_version_count}, NixHub: {v.nixhub_version_count}")
            print(f"  Missing: {missing} ({v.completeness_ratio:.1%} complete)")
            if v.versions_only_in_nixhub:
                print(f"  Latest missing: {', '.join(v.versions_only_in_nixhub[:5])}")

    # Assessment
    print("\n" + "=" * 70)
    print("ASSESSMENT")
    print("=" * 70)

    if report.avg_completeness >= 0.95:
        print("\n[EXCELLENT] Average completeness >= 95%")
    elif report.avg_completeness >= 0.80:
        print("\n[GOOD] Average completeness 80-95%")
    elif report.avg_completeness >= 0.50:
        print("\n[POOR] Average completeness 50-80%")
    else:
        print("\n[CRITICAL] Average completeness < 50%")

    if report.total_commit_mismatches > 0:
        print(f"\n[WARNING] {report.total_commit_mismatches} commit hash mismatches")
        print("  This may indicate data accuracy issues.")


def print_category_report(validations: list[PackageValidation]):
    """Print a report broken down by package category."""
    print("\n" + "-" * 70)
    print("COMPLETENESS BY CATEGORY:")
    print("-" * 70)

    for category, pkg_list in PACKAGE_CATEGORIES.items():
        cat_validations = [v for v in validations if v.package_name in pkg_list and not v.error]
        if not cat_validations:
            continue

        total_nxv = sum(v.nxv_version_count for v in cat_validations)
        total_nixhub = sum(v.nixhub_version_count for v in cat_validations)
        total_missing = sum(len(v.versions_only_in_nixhub) for v in cat_validations)
        avg_completeness = sum(v.completeness_ratio for v in cat_validations) / len(cat_validations)

        print(f"\n{category.upper()} ({len(cat_validations)} packages):")
        print(f"  nxv: {total_nxv}, NixHub: {total_nixhub}, Missing: {total_missing}")
        print(f"  Avg completeness: {avg_completeness:.1%}")


def main():
    parser = argparse.ArgumentParser(
        description="Validate nxv database against NixHub API."
    )
    parser.add_argument(
        "--db",
        type=Path,
        default=Path.home() / ".local/share/nxv/index.db",
        help="Path to nxv SQLite database",
    )
    parser.add_argument(
        "--packages",
        type=str,
        default=None,
        help="Comma-separated package names to validate",
    )
    parser.add_argument(
        "--sample",
        type=int,
        default=0,
        help="Validate N random packages from database",
    )
    parser.add_argument(
        "--check-cache",
        action="store_true",
        help="Check if store paths exist in cache.nixos.org",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=None,
        help="Output JSON report to file",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Show detailed output",
    )
    parser.add_argument(
        "--before",
        type=str,
        default=None,
        help="Only consider versions before this date (YYYY-MM-DD)",
    )
    parser.add_argument(
        "--after",
        type=str,
        default=None,
        help="Only consider versions after this date (YYYY-MM-DD)",
    )
    parser.add_argument(
        "--edge-cases",
        action="store_true",
        help="Include edge case packages for comprehensive testing",
    )
    parser.add_argument(
        "--comprehensive",
        action="store_true",
        help="Run comprehensive test with 100 random + all edge cases",
    )

    args = parser.parse_args()

    if not args.db.exists():
        print(f"Error: database not found: {args.db}", file=sys.stderr)
        sys.exit(1)

    # Determine packages to validate
    if args.comprehensive:
        print("Running comprehensive validation...")
        random_pkgs = get_random_packages(args.db, 100)
        packages = list(set(EDGE_CASE_PACKAGES + random_pkgs))
    elif args.packages:
        packages = [p.strip() for p in args.packages.split(",")]
    elif args.edge_cases:
        packages = EDGE_CASE_PACKAGES
    elif args.sample > 0:
        print(f"Selecting {args.sample} random packages...")
        packages = get_random_packages(args.db, args.sample)
    else:
        packages = DEFAULT_PACKAGES

    print(f"Database: {args.db}")
    print(f"Packages to validate: {len(packages)}")
    if args.before:
        print(f"Date filter: before {args.before}")
    if args.after:
        print(f"Date filter: after {args.after}")
    if args.check_cache:
        print("Cache checking: enabled (slower)")

    # Validate each package
    validations = []
    for i, package in enumerate(packages):
        if not args.verbose:
            print(f"\rProgress: {i+1}/{len(packages)} - {package[:30]:30s}", end="")
        validation = validate_package(
            args.db,
            package,
            args.check_cache,
            args.verbose,
            args.before,
            args.after,
        )
        validations.append(validation)
        # Rate limiting for NixHub API
        if i < len(packages) - 1:
            time.sleep(0.5)

    if not args.verbose:
        print()  # Newline after progress

    # Generate and print report
    report = generate_report(validations)
    print_report(report)

    # Print category breakdown if we have edge cases
    if args.edge_cases or args.comprehensive:
        print_category_report(validations)

    # Save JSON if requested
    if args.output:
        report_dict = {
            "packages_validated": report.packages_validated,
            "packages_with_errors": report.packages_with_errors,
            "total_versions_nxv": report.total_versions_nxv,
            "total_versions_nixhub": report.total_versions_nixhub,
            "total_missing_from_nxv": report.total_missing_from_nxv,
            "total_commit_mismatches": report.total_commit_mismatches,
            "total_store_path_mismatches": report.total_store_path_mismatches,
            "total_cache_unavailable": report.total_cache_unavailable,
            "avg_completeness": report.avg_completeness,
            "package_validations": [
                {
                    "package_name": v.package_name,
                    "nxv_version_count": v.nxv_version_count,
                    "nixhub_version_count": v.nixhub_version_count,
                    "versions_only_in_nxv": v.versions_only_in_nxv,
                    "versions_only_in_nixhub": v.versions_only_in_nixhub,
                    "versions_in_both": v.versions_in_both,
                    "completeness_ratio": v.completeness_ratio,
                    "error": v.error,
                    "commit_mismatches": [
                        {
                            "version": c.version,
                            "nxv_commit": c.nxv_commit,
                            "nixhub_commit": c.nixhub_commit,
                        }
                        for c in v.commit_mismatches
                    ],
                    "store_path_mismatches": [
                        {
                            "version": c.version,
                            "nxv_store_path": c.nxv_store_path,
                            "nixhub_store_path": c.nixhub_store_path,
                        }
                        for c in v.store_path_mismatches
                    ],
                }
                for v in report.package_validations
            ],
        }
        with open(args.output, "w") as f:
            json.dump(report_dict, f, indent=2)
        print(f"\nReport saved to: {args.output}")


if __name__ == "__main__":
    main()
