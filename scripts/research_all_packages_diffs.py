#!/usr/bin/env python3
"""
Research script to analyze all-packages.nix diff patterns.

This script helps us understand:
1. How often all-packages.nix is modified
2. What percentage of changes are simple version bumps vs complex changes
3. Whether we can reliably extract attribute names from diff lines
4. Edge cases that would cause Option 2 (diff parsing) to fail

Usage:
    python scripts/research_all_packages_diffs.py --nixpkgs /path/to/nixpkgs [options]

Options:
    --nixpkgs PATH      Path to nixpkgs repository (required)
    --since DATE        Start date (default: 2023-07-25, when our indexer broke)
    --until DATE        End date (default: now)
    --limit N           Limit number of commits to analyze (default: 1000)
    --output FILE       Output JSON report to file
    --verbose           Show detailed output for each commit
"""

import argparse
import json
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


# Files we're researching - these are excluded from the indexer's file-to-attr map
INFRASTRUCTURE_FILES = [
    "pkgs/top-level/all-packages.nix",
    "pkgs/top-level/aliases.nix",
]

# Regex patterns to extract attribute names from Nix assignment lines
# Matches: "  attrName = " at the start of a line
ATTR_ASSIGNMENT_PATTERN = re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_\-\']*)\s*=")

# Matches: "  attrName = callPackage" specifically
CALLPACKAGE_PATTERN = re.compile(
    r"^\s*([a-zA-Z_][a-zA-Z0-9_\-\']*)\s*=\s*(?:callPackage|python\d*Packages\.callPackage)"
)

# Matches inherited attributes: "  inherit (something) attr1 attr2;"
INHERIT_PATTERN = re.compile(r"^\s*inherit\s+\([^)]+\)\s+([^;]+);")

# Matches: "  attrName = prev.attrName.override" (overlay pattern)
OVERRIDE_PATTERN = re.compile(
    r"^\s*([a-zA-Z_][a-zA-Z0-9_\-\']*)\s*=\s*(?:prev|super|old)\.[a-zA-Z_][a-zA-Z0-9_\-\']*\.override"
)


@dataclass
class DiffAnalysis:
    """Analysis of a single commit's diff."""

    commit_hash: str
    commit_date: str
    commit_message: str
    files_changed: list[str]
    lines_added: int = 0
    lines_removed: int = 0
    attrs_extracted: list[str] = field(default_factory=list)
    extraction_method: dict[str, str] = field(default_factory=dict)  # attr -> method
    unparseable_lines: list[str] = field(default_factory=list)
    is_merge_commit: bool = False


@dataclass
class ResearchReport:
    """Overall research report."""

    total_commits: int = 0
    commits_touching_all_packages: int = 0
    commits_touching_aliases: int = 0
    total_attrs_extracted: int = 0
    extraction_success_rate: float = 0.0
    avg_attrs_per_commit: float = 0.0
    avg_lines_changed: float = 0.0
    extraction_methods: Counter = field(default_factory=Counter)
    common_unparseable_patterns: Counter = field(default_factory=Counter)
    large_diff_commits: list[str] = field(default_factory=list)  # >100 lines
    merge_commits: int = 0
    analyses: list[DiffAnalysis] = field(default_factory=list)


def run_git(args: list[str], cwd: Path) -> str:
    """Run a git command and return stdout."""
    result = subprocess.run(
        ["git"] + args,
        cwd=cwd,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout


def get_commits_touching_files(
    nixpkgs_path: Path,
    files: list[str],
    since: str,
    until: Optional[str],
    limit: int,
) -> list[tuple[str, str, str, bool]]:
    """Get commits that touched specified files.

    Returns list of (hash, date, message, is_merge) tuples.
    """
    args = [
        "log",
        "--first-parent",
        "--format=%H|%aI|%s|%P",
        f"--since={since}",
        f"-n{limit}",
        "--",
    ] + files

    if until:
        args.insert(4, f"--until={until}")

    output = run_git(args, nixpkgs_path)
    commits = []

    for line in output.strip().split("\n"):
        if not line:
            continue
        parts = line.split("|", 3)
        if len(parts) >= 4:
            hash_, date, message, parents = parts
            # Merge commits have multiple parents (space-separated)
            is_merge = " " in parents
            commits.append((hash_, date, message, is_merge))

    return commits


def get_diff_for_commit(
    nixpkgs_path: Path, commit_hash: str, files: list[str]
) -> tuple[str, list[str]]:
    """Get the diff for a commit, comparing against first parent.

    Returns (diff_text, changed_files).
    """
    # For merge commits, compare against first parent to see actual PR changes
    try:
        diff = run_git(
            ["diff", f"{commit_hash}^1", commit_hash, "--"] + files, nixpkgs_path
        )
    except subprocess.CalledProcessError:
        # Might fail for initial commits
        diff = run_git(["show", commit_hash, "--format=", "--"] + files, nixpkgs_path)

    # Get list of changed files
    try:
        files_output = run_git(
            ["diff", "--name-only", f"{commit_hash}^1", commit_hash, "--"] + files,
            nixpkgs_path,
        )
        changed_files = [f for f in files_output.strip().split("\n") if f]
    except subprocess.CalledProcessError:
        changed_files = files

    return diff, changed_files


def extract_attrs_from_line(line: str) -> tuple[Optional[str], str]:
    """Try to extract an attribute name from a diff line.

    Returns (attr_name, extraction_method) or (None, "unparseable").
    """
    # Remove diff prefix (+/-)
    clean_line = line[1:] if line.startswith(("+", "-")) else line

    # Skip comments
    if clean_line.strip().startswith("#"):
        return None, "comment"

    # Skip empty lines
    if not clean_line.strip():
        return None, "empty"

    # Try callPackage pattern first (most specific)
    match = CALLPACKAGE_PATTERN.match(clean_line)
    if match:
        return match.group(1), "callPackage"

    # Try override pattern
    match = OVERRIDE_PATTERN.match(clean_line)
    if match:
        return match.group(1), "override"

    # Try inherit pattern
    match = INHERIT_PATTERN.match(clean_line)
    if match:
        # Extract all inherited attrs
        attrs = match.group(1).split()
        if attrs:
            return attrs[0], "inherit"  # Return first for now

    # Try general assignment pattern
    match = ATTR_ASSIGNMENT_PATTERN.match(clean_line)
    if match:
        attr = match.group(1)
        # Filter out common non-package assignments
        if attr in ("let", "in", "if", "then", "else", "inherit", "with", "rec"):
            return None, "keyword"
        if attr.startswith("_"):
            return None, "private"
        return attr, "assignment"

    return None, "unparseable"


def analyze_diff(
    diff_text: str,
) -> tuple[list[str], dict[str, str], list[str], int, int]:
    """Analyze a diff and extract attribute names.

    Returns (attrs, extraction_methods, unparseable_lines).
    """
    attrs = []
    methods = {}
    unparseable = []
    lines_added = 0
    lines_removed = 0

    for line in diff_text.split("\n"):
        if line.startswith("+") and not line.startswith("+++"):
            lines_added += 1
            attr, method = extract_attrs_from_line(line)
            if attr:
                if attr not in attrs:
                    attrs.append(attr)
                    methods[attr] = method
            elif method == "unparseable":
                # Only track truly unparseable lines, not comments/empty
                unparseable.append(line[:100])  # Truncate long lines

        elif line.startswith("-") and not line.startswith("---"):
            lines_removed += 1
            attr, method = extract_attrs_from_line(line)
            if attr:
                if attr not in attrs:
                    attrs.append(attr)
                    methods[attr] = method

    return attrs, methods, unparseable, lines_added, lines_removed


def analyze_commit(
    nixpkgs_path: Path,
    commit_hash: str,
    commit_date: str,
    commit_message: str,
    is_merge: bool,
    verbose: bool = False,
) -> DiffAnalysis:
    """Analyze a single commit."""
    diff_text, changed_files = get_diff_for_commit(
        nixpkgs_path, commit_hash, INFRASTRUCTURE_FILES
    )

    attrs, methods, unparseable, lines_added, lines_removed = analyze_diff(diff_text)

    analysis = DiffAnalysis(
        commit_hash=commit_hash,
        commit_date=commit_date,
        commit_message=commit_message[:100],
        files_changed=changed_files,
        lines_added=lines_added,
        lines_removed=lines_removed,
        attrs_extracted=attrs,
        extraction_method=methods,
        unparseable_lines=unparseable[:10],  # Keep first 10
        is_merge_commit=is_merge,
    )

    if verbose:
        print(f"\n{'=' * 60}")
        print(f"Commit: {commit_hash[:7]} ({commit_date})")
        print(f"Message: {commit_message[:60]}...")
        print(f"Files: {', '.join(changed_files)}")
        print(f"Lines: +{lines_added} -{lines_removed}")
        print(f"Attrs extracted: {len(attrs)}")
        if attrs:
            for attr in attrs[:10]:
                print(f"  - {attr} ({methods.get(attr, '?')})")
            if len(attrs) > 10:
                print(f"  ... and {len(attrs) - 10} more")
        if unparseable:
            print(f"Unparseable lines: {len(unparseable)}")
            for line in unparseable[:3]:
                print(f"  ? {line[:60]}...")

    return analysis


def generate_report(analyses: list[DiffAnalysis]) -> ResearchReport:
    """Generate a summary report from all analyses."""
    report = ResearchReport()
    report.total_commits = len(analyses)
    report.analyses = analyses

    total_attrs = 0
    total_lines = 0
    all_methods = Counter()
    all_unparseable = Counter()

    for analysis in analyses:
        if "pkgs/top-level/all-packages.nix" in analysis.files_changed:
            report.commits_touching_all_packages += 1
        if "pkgs/top-level/aliases.nix" in analysis.files_changed:
            report.commits_touching_aliases += 1

        total_attrs += len(analysis.attrs_extracted)
        total_lines += analysis.lines_added + analysis.lines_removed

        for method in analysis.extraction_method.values():
            all_methods[method] += 1

        for line in analysis.unparseable_lines:
            # Normalize the line for counting patterns
            normalized = re.sub(r"[a-zA-Z0-9_\-]+", "X", line[:50])
            all_unparseable[normalized] += 1

        if analysis.lines_added + analysis.lines_removed > 100:
            report.large_diff_commits.append(analysis.commit_hash)

        if analysis.is_merge_commit:
            report.merge_commits += 1

    report.total_attrs_extracted = total_attrs
    report.extraction_methods = all_methods
    report.common_unparseable_patterns = Counter(dict(all_unparseable.most_common(20)))

    if analyses:
        report.avg_attrs_per_commit = total_attrs / len(analyses)
        report.avg_lines_changed = total_lines / len(analyses)

    # Calculate success rate: commits where we extracted at least one attr
    successful = sum(1 for a in analyses if a.attrs_extracted)
    report.extraction_success_rate = successful / len(analyses) if analyses else 0

    return report


def print_report(report: ResearchReport):
    """Print a human-readable report."""
    print("\n" + "=" * 70)
    print("ALL-PACKAGES.NIX DIFF ANALYSIS REPORT")
    print("=" * 70)

    print(f"\nCommits analyzed: {report.total_commits}")
    print(f"  - Touching all-packages.nix: {report.commits_touching_all_packages}")
    print(f"  - Touching aliases.nix: {report.commits_touching_aliases}")
    print(f"  - Merge commits: {report.merge_commits}")

    print("\nExtraction Statistics:")
    print(f"  - Total attrs extracted: {report.total_attrs_extracted}")
    print(f"  - Avg attrs per commit: {report.avg_attrs_per_commit:.1f}")
    print(f"  - Avg lines changed per commit: {report.avg_lines_changed:.1f}")
    print(f"  - Success rate (>=1 attr): {report.extraction_success_rate:.1%}")

    print("\nExtraction Methods:")
    for method, count in report.extraction_methods.most_common():
        print(f"  - {method}: {count}")

    print(f"\nLarge diffs (>100 lines): {len(report.large_diff_commits)}")
    if report.large_diff_commits:
        for commit in report.large_diff_commits[:5]:
            print(f"  - {commit[:7]}")
        if len(report.large_diff_commits) > 5:
            print(f"  ... and {len(report.large_diff_commits) - 5} more")

    print("\nCommon Unparseable Patterns:")
    for pattern, count in report.common_unparseable_patterns.most_common(10):
        print(f"  [{count:3d}] {pattern[:50]}")

    # Analysis of viability
    print("\n" + "=" * 70)
    print("VIABILITY ASSESSMENT FOR OPTION 2 (DIFF PARSING)")
    print("=" * 70)

    if report.extraction_success_rate >= 0.9:
        print("\n[GOOD] High success rate (>=90%)")
        print("  Diff parsing appears viable for most commits.")
    elif report.extraction_success_rate >= 0.7:
        print("\n[MODERATE] Moderate success rate (70-90%)")
        print("  Diff parsing would work for most commits but needs fallback.")
    else:
        print("\n[POOR] Low success rate (<70%)")
        print("  Diff parsing may not be reliable enough.")

    if len(report.large_diff_commits) > report.total_commits * 0.1:
        print("\n[WARNING] >10% of commits have large diffs (>100 lines)")
        print("  May need special handling for bulk updates.")

    if report.avg_attrs_per_commit < 2:
        print("\n[GOOD] Low avg attrs per commit")
        print("  Most changes are targeted, not bulk updates.")
    else:
        print("\n[INFO] Higher avg attrs per commit")
        print(f"  Average of {report.avg_attrs_per_commit:.1f} attrs changed.")


def main():
    parser = argparse.ArgumentParser(
        description="Research all-packages.nix diff patterns for nxv indexer."
    )
    parser.add_argument(
        "--nixpkgs",
        type=Path,
        required=True,
        help="Path to nixpkgs repository",
    )
    parser.add_argument(
        "--since",
        type=str,
        default="2023-07-25",
        help="Start date (default: 2023-07-25)",
    )
    parser.add_argument(
        "--until",
        type=str,
        default=None,
        help="End date (default: now)",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=1000,
        help="Max commits to analyze (default: 1000)",
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
        help="Show detailed output for each commit",
    )

    args = parser.parse_args()

    if not args.nixpkgs.exists():
        print(f"Error: nixpkgs path does not exist: {args.nixpkgs}", file=sys.stderr)
        sys.exit(1)

    print(f"Analyzing commits in {args.nixpkgs}")
    print(f"Since: {args.since}")
    print(f"Until: {args.until or 'now'}")
    print(f"Limit: {args.limit}")

    # Get commits
    print("\nFetching commits...")
    commits = get_commits_touching_files(
        args.nixpkgs, INFRASTRUCTURE_FILES, args.since, args.until, args.limit
    )
    print(f"Found {len(commits)} commits touching infrastructure files")

    # Analyze each commit
    print("\nAnalyzing commits...")
    analyses = []
    for i, (hash_, date, message, is_merge) in enumerate(commits):
        if i % 100 == 0 and not args.verbose:
            print(f"  Progress: {i}/{len(commits)}")
        analysis = analyze_commit(
            args.nixpkgs, hash_, date, message, is_merge, args.verbose
        )
        analyses.append(analysis)

    # Generate report
    report = generate_report(analyses)
    print_report(report)

    # Save JSON if requested
    if args.output:
        # Convert to serializable format
        report_dict = {
            "total_commits": report.total_commits,
            "commits_touching_all_packages": report.commits_touching_all_packages,
            "commits_touching_aliases": report.commits_touching_aliases,
            "total_attrs_extracted": report.total_attrs_extracted,
            "extraction_success_rate": report.extraction_success_rate,
            "avg_attrs_per_commit": report.avg_attrs_per_commit,
            "avg_lines_changed": report.avg_lines_changed,
            "extraction_methods": dict(report.extraction_methods),
            "common_unparseable_patterns": dict(report.common_unparseable_patterns),
            "large_diff_commits": report.large_diff_commits,
            "merge_commits": report.merge_commits,
            "analyses": [
                {
                    "commit_hash": a.commit_hash,
                    "commit_date": a.commit_date,
                    "commit_message": a.commit_message,
                    "files_changed": a.files_changed,
                    "lines_added": a.lines_added,
                    "lines_removed": a.lines_removed,
                    "attrs_extracted": a.attrs_extracted,
                    "extraction_method": a.extraction_method,
                    "unparseable_lines": a.unparseable_lines,
                    "is_merge_commit": a.is_merge_commit,
                }
                for a in report.analyses
            ],
        }
        with open(args.output, "w") as f:
            json.dump(report_dict, f, indent=2)
        print(f"\nReport saved to: {args.output}")


if __name__ == "__main__":
    main()
