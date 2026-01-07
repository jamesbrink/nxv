#!/usr/bin/env python3
"""
Database benchmark script for nxv.

Measures query performance across all major query paths used by the application.
Compares package_versions (full) vs package_latest (materialized view) tables.
Includes cold cache (first query) vs warm cache metrics.
"""

import sqlite3
import time
import statistics
import subprocess
import sys
from pathlib import Path
from dataclasses import dataclass, field


# Default database paths
DEFAULT_DB_PATH = Path.home() / "Library/Application Support/nxv/index.db"
LINUX_DB_PATH = Path.home() / ".local/share/nxv/index.db"


@dataclass
class BenchmarkResult:
    name: str
    table: str
    rows: int
    cold_ms: float  # First query (cold cache)
    warm_ms: float  # Average of subsequent queries
    runs: int = 3


@dataclass
class DatabaseInfo:
    path: Path
    size_mb: float = 0
    total_rows: int = 0
    unique_attrs: int = 0
    unique_names: int = 0
    has_matview: bool = False
    matview_rows: int = 0


def get_db_path() -> Path:
    if DEFAULT_DB_PATH.exists():
        return DEFAULT_DB_PATH
    if LINUX_DB_PATH.exists():
        return LINUX_DB_PATH
    raise FileNotFoundError("No nxv database found")


def drop_os_caches():
    """Attempt to drop OS file caches (requires sudo on most systems)."""
    if sys.platform == "darwin":
        # macOS - purge disk cache (may require sudo)
        try:
            subprocess.run(["purge"], capture_output=True, timeout=10)
            return True
        except (subprocess.SubprocessError, FileNotFoundError):
            return False
    elif sys.platform == "linux":
        # Linux - drop caches
        try:
            subprocess.run(
                ["sudo", "sh", "-c", "echo 3 > /proc/sys/vm/drop_caches"],
                capture_output=True,
                timeout=10,
            )
            return True
        except (subprocess.SubprocessError, FileNotFoundError):
            return False
    return False


def benchmark_query(
    conn: sqlite3.Connection,
    name: str,
    table: str,
    query: str,
    params: tuple = (),
    runs: int = 3,
    db_path: Path = None,
    true_cold: bool = False,
) -> BenchmarkResult:
    """Run a query multiple times and return benchmark results with cold/warm split.

    If true_cold=True, drops OS cache and reopens connection before cold measurement.
    """
    times = []
    rows = 0

    for i in range(runs + 1):  # +1 for cold run
        # For true cold measurement on first run
        if i == 0 and true_cold and db_path:
            conn.close()
            drop_os_caches()
            time.sleep(0.5)  # Let OS settle
            conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
            conn.execute("PRAGMA cache_size = -2000")

        start = time.perf_counter()
        try:
            cursor = conn.execute(query, params)
            results = cursor.fetchall()
            elapsed = (time.perf_counter() - start) * 1000
            times.append(elapsed)
            rows = len(results)
        except sqlite3.Error as e:
            print(f"  Error in {name}: {e}")
            return BenchmarkResult(name, table, 0, -1, -1, runs)

    cold_ms = times[0]  # First query
    warm_ms = statistics.mean(times[1:]) if len(times) > 1 else times[0]

    return BenchmarkResult(name, table, rows, cold_ms, warm_ms, runs)


def get_db_info(db_path: Path) -> DatabaseInfo:
    """Get database metadata."""
    info = DatabaseInfo(path=db_path)
    info.size_mb = db_path.stat().st_size / (1024 * 1024)

    try:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)

        cursor = conn.execute("SELECT COUNT(*) FROM package_versions")
        info.total_rows = cursor.fetchone()[0]

        cursor = conn.execute("SELECT COUNT(DISTINCT attribute_path) FROM package_versions")
        info.unique_attrs = cursor.fetchone()[0]

        cursor = conn.execute("SELECT COUNT(DISTINCT name) FROM package_versions")
        info.unique_names = cursor.fetchone()[0]

        # Check for matview
        cursor = conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='package_latest'"
        )
        info.has_matview = cursor.fetchone() is not None

        if info.has_matview:
            cursor = conn.execute("SELECT COUNT(*) FROM package_latest")
            info.matview_rows = cursor.fetchone()[0]

        conn.close()
    except sqlite3.Error as e:
        print(f"Error reading database info: {e}")

    return info


def run_benchmarks(
    conn: sqlite3.Connection,
    tables: list[str],
    db_path: Path = None,
    true_cold: bool = False,
) -> list[BenchmarkResult]:
    """Run all benchmarks and return results."""
    results = []

    # Test data
    search_terms = ["python", "nodejs", "firefox", "gtk", "qt"]
    exact_attrs = ["python3", "firefox", "nodejs", "gcc"]

    # Common kwargs for benchmark_query
    bm_kwargs = {"db_path": db_path, "true_cold": true_cold}

    for table in tables:
        # Check if table exists
        cursor = conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
            (table,),
        )
        if not cursor.fetchone():
            print(f"  Skipping {table} (not found)")
            continue

        print(f"  Benchmarking {table}..." + (" (true cold)" if true_cold else ""))

        # 1. Exact attribute path lookup
        for attr in exact_attrs:
            results.append(
                benchmark_query(
                    conn,
                    f"exact_attr({attr})",
                    table,
                    f"SELECT * FROM {table} WHERE attribute_path = ?",
                    (attr,),
                    **bm_kwargs,
                )
            )

        # 2. Name prefix search (LIKE)
        for term in search_terms:
            results.append(
                benchmark_query(
                    conn,
                    f"name_prefix({term})",
                    table,
                    f"SELECT * FROM {table} WHERE name LIKE ? ORDER BY last_commit_date DESC",
                    (f"{term}%",),
                    **bm_kwargs,
                )
            )

        # 3. Attribute path prefix search
        for term in search_terms:
            results.append(
                benchmark_query(
                    conn,
                    f"attr_prefix({term})",
                    table,
                    f"SELECT * FROM {table} WHERE attribute_path LIKE ? ORDER BY last_commit_date DESC",
                    (f"{term}%",),
                    **bm_kwargs,
                )
            )

        # 4. Search with relevance ranking (main search query)
        for term in search_terms:
            query = f"""
                SELECT *,
                    CASE
                        WHEN attribute_path = ?1 THEN 1
                        WHEN attribute_path LIKE ?2 AND attribute_path NOT LIKE ?3 THEN 2
                        WHEN attribute_path LIKE ?4 THEN 3
                        WHEN attribute_path LIKE ?5 THEN 4
                        WHEN attribute_path LIKE ?2 THEN 5
                        ELSE 6
                    END as relevance
                FROM {table}
                WHERE attribute_path LIKE ?2 OR attribute_path LIKE ?6
                ORDER BY relevance, last_commit_date DESC
                LIMIT 100
            """
            params = (
                term,
                f"{term}%",
                f"{term}%.%",
                f"%.{term}",
                f"%.{term}%",
                f"%.{term}%",
            )
            results.append(
                benchmark_query(conn, f"relevance_search({term})", table, query, params, **bm_kwargs)
            )

        # 5. Search by name + version
        results.append(
            benchmark_query(
                conn,
                "name_version(python,3.11)",
                table,
                f"""
                SELECT * FROM {table}
                WHERE attribute_path LIKE ? AND version LIKE ?
                ORDER BY last_commit_date DESC
                """,
                ("python%", "3.11%"),
                **bm_kwargs,
            )
        )

        # 6. Stats queries
        results.append(
            benchmark_query(
                conn,
                "stats_count",
                table,
                f"SELECT COUNT(*) FROM {table}",
                runs=1,
                **bm_kwargs,
            )
        )

        results.append(
            benchmark_query(
                conn,
                "stats_distinct_attrs",
                table,
                f"SELECT COUNT(DISTINCT attribute_path) FROM {table}",
                runs=1,
                **bm_kwargs,
            )
        )

        # 7. Version history for a package
        results.append(
            benchmark_query(
                conn,
                "version_history(python3)",
                table,
                f"""
                SELECT version, MIN(first_commit_date), MAX(last_commit_date)
                FROM {table}
                WHERE attribute_path = ?
                GROUP BY version
                ORDER BY MIN(first_commit_date) DESC
                """,
                ("python3",),
                **bm_kwargs,
            )
        )

        # 8. Completion prefix
        results.append(
            benchmark_query(
                conn,
                "complete_prefix(pyth)",
                table,
                f"""
                SELECT DISTINCT attribute_path FROM {table}
                WHERE attribute_path LIKE ?
                ORDER BY attribute_path
                LIMIT 20
                """,
                ("pyth%",),
                **bm_kwargs,
            )
        )

    return results


def print_results(results: list[BenchmarkResult], tables: list[str]) -> None:
    """Print benchmark results in a formatted table."""
    # Group by query name
    by_name: dict[str, dict[str, BenchmarkResult]] = {}
    for r in results:
        if r.name not in by_name:
            by_name[r.name] = {}
        by_name[r.name][r.table] = r

    if len(tables) == 1:
        # Single table output
        table = tables[0]
        print(f"\n{'Query':<35} {'Cold (ms)':<12} {'Warm (ms)':<12} {'Rows':<12}")
        print("=" * 75)
        for name, tbl_results in by_name.items():
            r = tbl_results.get(table)
            if r and r.cold_ms >= 0:
                print(f"{name:<35} {r.cold_ms:<12.2f} {r.warm_ms:<12.2f} {r.rows:<12,}")
    else:
        # Comparison output
        print(f"\n{'Query':<30} {'Full Cold':<10} {'Full Warm':<10} {'Mat Cold':<10} {'Mat Warm':<10} {'Speedup':<8} {'Rows'}")
        print("=" * 100)
        for name, tbl_results in by_name.items():
            full = tbl_results.get("package_versions")
            mat = tbl_results.get("package_latest")

            full_cold = f"{full.cold_ms:.1f}" if full and full.cold_ms >= 0 else "-"
            full_warm = f"{full.warm_ms:.1f}" if full and full.warm_ms >= 0 else "-"
            mat_cold = f"{mat.cold_ms:.1f}" if mat and mat.cold_ms >= 0 else "-"
            mat_warm = f"{mat.warm_ms:.1f}" if mat and mat.warm_ms >= 0 else "-"

            if full and mat and full.warm_ms > 0 and mat.warm_ms > 0:
                speedup = f"{full.warm_ms / mat.warm_ms:.1f}x"
            else:
                speedup = "-"

            rows = f"{full.rows:,}" if full else (f"{mat.rows:,}" if mat else "0")
            print(f"{name:<30} {full_cold:<10} {full_warm:<10} {mat_cold:<10} {mat_warm:<10} {speedup:<8} {rows}")

    print()


def print_summary(info: DatabaseInfo) -> None:
    """Print database summary statistics."""
    print(f"\nDatabase: {info.path}")
    print("-" * 50)
    print(f"File size:              {info.size_mb:.1f} MB")
    print(f"Total rows:             {info.total_rows:,}")
    print(f"Unique attribute paths: {info.unique_attrs:,}")
    print(f"Unique names:           {info.unique_names:,}")
    print(f"Avg versions/package:   {info.total_rows / info.unique_attrs:.1f}" if info.unique_attrs else "")

    if info.has_matview:
        print(f"\nMaterialized view (package_latest):")
        print(f"  Rows:             {info.matview_rows:,}")
        print(f"  Reduction factor: {info.total_rows / info.matview_rows:.1f}x" if info.matview_rows else "")


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="Benchmark nxv database queries")
    parser.add_argument("--db", type=Path, help="Path to database file")
    parser.add_argument("--drop-cache", action="store_true", help="Attempt to drop OS caches before benchmarking")
    parser.add_argument("--true-cold", action="store_true",
                        help="Drop OS cache before each cold measurement (slow, accurate)")
    parser.add_argument("--table", choices=["full", "matview", "both"], default="both",
                        help="Which table(s) to benchmark")
    args = parser.parse_args()

    db_path = args.db or get_db_path()
    print(f"Using database: {db_path}")

    if not db_path.exists():
        print(f"Error: Database not found at {db_path}")
        sys.exit(1)

    # Get database info (skip if true-cold to avoid warming cache)
    if not args.true_cold:
        info = get_db_info(db_path)
        print_summary(info)
    else:
        print(f"\nDatabase: {db_path}")
        print(f"File size: {db_path.stat().st_size / (1024 * 1024):.1f} MB")
        print("(Skipping detailed info to preserve cold cache)")
        # Minimal info for table detection
        info = DatabaseInfo(path=db_path)
        conn_tmp = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        cursor = conn_tmp.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='package_latest'"
        )
        info.has_matview = cursor.fetchone() is not None
        conn_tmp.close()

    # Determine which tables to test
    if args.table == "full":
        tables = ["package_versions"]
    elif args.table == "matview":
        tables = ["package_latest"]
    else:
        tables = ["package_versions"]
        if info.has_matview:
            tables.append("package_latest")

    # Drop caches if requested (one-time at start)
    if args.drop_cache and not args.true_cold:
        print("\nDropping OS caches...")
        if drop_os_caches():
            print("  Caches dropped successfully")
            time.sleep(1)  # Give OS time to settle
        else:
            print("  Failed to drop caches (may need sudo)")

    # Open connection with minimal caching
    print("\nRunning benchmarks...")
    if args.true_cold:
        print("  (true cold mode: dropping OS cache before each cold measurement)")

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.execute("PRAGMA cache_size = -2000")  # 2MB cache (small)

    results = run_benchmarks(conn, tables, db_path=db_path, true_cold=args.true_cold)
    print_results(results, tables)

    conn.close()


if __name__ == "__main__":
    main()
