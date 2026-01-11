#!/usr/bin/env bash
# Post-reindex validation script
# Run this after completing a full reindex to validate data quality.
#
# Usage:
#   ./scripts/post_reindex_validation.sh [options]
#
# Options:
#   --db PATH           Path to nxv database (default: ~/.local/share/nxv/index.db)
#   --nixpkgs PATH      Path to nixpkgs clone (enables git verification)
#   --output DIR        Directory to save validation reports
#   --quick             Run quick validation (fewer packages)
#   --full              Run comprehensive validation (all edge cases)
#
# Exit codes:
#   0 - Validation passed (>= 80% completeness target)
#   1 - Validation failed or errors occurred

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Default values
DB_PATH="${HOME}/.local/share/nxv/index.db"
NIXPKGS_PATH=""
OUTPUT_DIR=""
MODE="standard"

# Quality targets
TARGET_COMPLETENESS=80  # Minimum % completeness vs NixHub
TARGET_POST_2023_COMPLETENESS=70  # Post-July 2023 data (was broken)
MAX_COMMIT_MISMATCHES=50  # Maximum acceptable commit hash mismatches

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --db)
            DB_PATH="$2"
            shift 2
            ;;
        --nixpkgs)
            NIXPKGS_PATH="$2"
            shift 2
            ;;
        --output)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        --quick)
            MODE="quick"
            shift
            ;;
        --full)
            MODE="full"
            shift
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

echo "=========================================="
echo "NXV Post-Reindex Validation"
echo "=========================================="
echo ""
echo "Database: $DB_PATH"
echo "Mode: $MODE"
if [[ -n "$NIXPKGS_PATH" ]]; then
    echo "Nixpkgs: $NIXPKGS_PATH"
fi
echo ""

# Check database exists
if [[ ! -f "$DB_PATH" ]]; then
    echo -e "${RED}ERROR: Database not found: $DB_PATH${NC}"
    exit 1
fi

# Create output directory if specified
if [[ -n "$OUTPUT_DIR" ]]; then
    mkdir -p "$OUTPUT_DIR"
fi

FAILED=0
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Helper function to run validation
run_validation() {
    local name="$1"
    local args="$2"
    local output_file="${3:-}"

    echo -e "${BLUE}Running: $name${NC}"

    local cmd="python3 ${SCRIPT_DIR}/validate_against_nixhub.py --db $DB_PATH $args"

    if [[ -n "$output_file" && -n "$OUTPUT_DIR" ]]; then
        cmd="$cmd --output ${OUTPUT_DIR}/${output_file}"
    fi

    eval "$cmd"
    echo ""
}

# Test 1: Basic database stats
echo "=========================================="
echo "1. Database Statistics"
echo "=========================================="
echo ""

# Get stats using sqlite3
TOTAL_RECORDS=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM package_versions;")
UNIQUE_PACKAGES=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT attribute_path) FROM package_versions;")
UNIQUE_VERSIONS=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT attribute_path || ':' || version) FROM package_versions;")
LAST_COMMIT=$(sqlite3 "$DB_PATH" "SELECT value FROM meta WHERE key = 'last_indexed_commit';")
LAST_DATE=$(sqlite3 "$DB_PATH" "SELECT datetime(value, 'unixepoch') FROM meta WHERE key = 'last_indexed_date';")

echo "Total records: $TOTAL_RECORDS"
echo "Unique packages: $UNIQUE_PACKAGES"
echo "Unique package-versions: $UNIQUE_VERSIONS"
echo "Last indexed commit: $LAST_COMMIT"
echo "Last indexed date: $LAST_DATE"
echo ""

# Check for reasonable numbers
if [[ $TOTAL_RECORDS -lt 1000000 ]]; then
    echo -e "${YELLOW}WARNING: Total records ($TOTAL_RECORDS) seems low${NC}"
    echo "  Expected at least 1,000,000 records for a full index"
fi

# Test 2: Version source distribution
echo "=========================================="
echo "2. Version Source Distribution"
echo "=========================================="
echo ""

sqlite3 "$DB_PATH" "
SELECT
    COALESCE(version_source, 'NULL') as source,
    COUNT(*) as count,
    ROUND(COUNT(*) * 100.0 / (SELECT COUNT(*) FROM package_versions), 1) as pct
FROM package_versions
GROUP BY version_source
ORDER BY count DESC;
"
echo ""

# Test 3: Unknown version check
echo "=========================================="
echo "3. Unknown Version Analysis"
echo "=========================================="
echo ""

UNKNOWN_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM package_versions WHERE version = 'unknown' OR version IS NULL OR version = '';")
UNKNOWN_PCT=$(sqlite3 "$DB_PATH" "SELECT ROUND(COUNT(*) * 100.0 / (SELECT COUNT(*) FROM package_versions), 1) FROM package_versions WHERE version = 'unknown' OR version IS NULL OR version = '';")

echo "Records with unknown/empty version: $UNKNOWN_COUNT ($UNKNOWN_PCT%)"

if (( $(echo "$UNKNOWN_PCT > 10" | bc -l) )); then
    echo -e "${YELLOW}WARNING: High unknown version rate ($UNKNOWN_PCT%)${NC}"
    echo "  Target: < 10%"
fi
echo ""

# Test 4: Post-July 2023 data check
echo "=========================================="
echo "4. Post-July 2023 Data Coverage"
echo "=========================================="
echo ""

POST_2023_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM package_versions WHERE first_commit_date >= strftime('%s', '2023-07-25');")
POST_2023_UNIQUE=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT attribute_path) FROM package_versions WHERE first_commit_date >= strftime('%s', '2023-07-25');")

echo "Records after July 25, 2023: $POST_2023_COUNT"
echo "Unique packages after July 25, 2023: $POST_2023_UNIQUE"

if [[ $POST_2023_COUNT -lt 100000 ]]; then
    echo -e "${YELLOW}WARNING: Post-July 2023 record count ($POST_2023_COUNT) seems low${NC}"
    echo "  This was the period affected by the all-packages.nix issue"
fi
echo ""

# Test 5: Index Gap Detection
echo "=========================================="
echo "5. Index Gap Detection"
echo "=========================================="
echo ""

echo -e "${BLUE}Checking for gaps in monthly record counts...${NC}"
echo ""

# Run gap detection and capture exit code
set +e
python3 "${SCRIPT_DIR}/validate_against_nixhub.py" --db "$DB_PATH" --check-gaps
GAP_EXIT=$?
set -e

if [[ $GAP_EXIT -ne 0 ]]; then
    echo -e "${RED}CRITICAL: Gap detection found missing months!${NC}"
    echo "  This may indicate commits were skipped during indexing."
    echo "  Review the output above and consider re-indexing affected periods."
    FAILED=1
fi
echo ""

# Test 6: NixHub validation
echo "=========================================="
echo "6. NixHub Comparison"
echo "=========================================="
echo ""

case $MODE in
    quick)
        VALIDATION_ARGS="--packages hello,git,curl,vim,python3"
        ;;
    full)
        VALIDATION_ARGS="--comprehensive"
        ;;
    *)
        VALIDATION_ARGS="--edge-cases"
        ;;
esac

# Add nixpkgs verification if available
if [[ -n "$NIXPKGS_PATH" ]]; then
    VALIDATION_ARGS="$VALIDATION_ARGS --nixpkgs $NIXPKGS_PATH --verify-commits"
fi

run_validation "NixHub comparison" "$VALIDATION_ARGS" "nixhub_validation.json"

# Test 7: Critical packages spot check
echo "=========================================="
echo "7. Critical Packages Spot Check"
echo "=========================================="
echo ""

CRITICAL_PACKAGES="firefox,chromium,thunderbird,python3,nodejs,rustc,go,gcc"
echo "Checking: $CRITICAL_PACKAGES"
echo ""

for pkg in ${CRITICAL_PACKAGES//,/ }; do
    VERSION_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT version) FROM package_versions WHERE attribute_path = '$pkg' AND version != 'unknown' AND version != '';")
    LATEST=$(sqlite3 "$DB_PATH" "SELECT version FROM package_versions WHERE attribute_path = '$pkg' AND version != 'unknown' ORDER BY first_commit_date DESC LIMIT 1;")

    if [[ $VERSION_COUNT -gt 0 ]]; then
        echo -e "  $pkg: ${GREEN}$VERSION_COUNT versions${NC} (latest: $LATEST)"
    else
        echo -e "  $pkg: ${RED}NO VERSIONS${NC}"
        FAILED=1
    fi
done
echo ""

# Test 8: Wrapper Package Version Coverage
echo "=========================================="
echo "8. Wrapper Package Version Coverage"
echo "=========================================="
echo ""
echo "Checking packages known to have versions in separate files..."
echo "(These are prone to being missed by incremental detection)"
echo ""

# Wrapper packages should have many versions across years
# Format: package:min_expected_versions
# These packages define versions in packages.nix, common.nix, browser.nix, etc.
WRAPPER_PACKAGES=(
    "firefox:20"
    "firefox-esr:15"
    "chromium:20"
    "thunderbird:15"
    "vscode:15"
    "signal-desktop:10"
    "slack:10"
    "discord:10"
    "spotify:5"
    "zoom-us:5"
)

WRAPPER_FAILED=0
for entry in "${WRAPPER_PACKAGES[@]}"; do
    pkg="${entry%%:*}"
    min_versions="${entry##*:}"

    VERSION_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT version) FROM package_versions WHERE attribute_path = '$pkg' AND version != 'unknown' AND version != '';")
    EARLIEST=$(sqlite3 "$DB_PATH" "SELECT datetime(MIN(first_commit_date), 'unixepoch') FROM package_versions WHERE attribute_path = '$pkg';")
    LATEST=$(sqlite3 "$DB_PATH" "SELECT datetime(MAX(last_commit_date), 'unixepoch') FROM package_versions WHERE attribute_path = '$pkg';")

    if [[ $VERSION_COUNT -ge $min_versions ]]; then
        echo -e "  $pkg: ${GREEN}$VERSION_COUNT versions${NC} (expected $min_versions+) [$EARLIEST to $LATEST]"
    elif [[ $VERSION_COUNT -gt 0 ]]; then
        echo -e "  $pkg: ${YELLOW}$VERSION_COUNT versions${NC} (expected $min_versions+)"
        echo "    Range: $EARLIEST to $LATEST"
        echo "    This may indicate missing version updates."
        WRAPPER_FAILED=1
    else
        echo -e "  $pkg: ${RED}NO VERSIONS${NC} (expected $min_versions+)"
        WRAPPER_FAILED=1
    fi
done

if [[ $WRAPPER_FAILED -eq 1 ]]; then
    echo ""
    echo -e "${YELLOW}WARNING: Some wrapper packages have fewer versions than expected.${NC}"
    echo "  These packages define versions in separate files (e.g., packages.nix)"
    echo "  and may have been missed by incremental detection."
    echo "  Consider re-indexing with --full or --full-extraction-interval 50"
fi
echo ""

# Test 8b: High-frequency update packages
echo "=========================================="
echo "8b. High-Frequency Update Packages"
echo "=========================================="
echo ""
echo "Checking packages that update frequently (should have many versions)..."
echo ""

# These packages update very frequently - low counts indicate gaps
HIGH_FREQ_PACKAGES=(
    "linux:50"           # Kernel updates frequently
    "nodejs:30"          # New versions every few months
    "python3:15"         # 3.x versions
    "rustc:20"           # Frequent releases
    "go:15"              # Go versions
    "ruby:15"            # Ruby versions
    "php:15"             # PHP versions
    "gcc:10"             # GCC versions
    "llvm:15"            # LLVM versions
    "clang:15"           # Clang versions
    "postgresql:10"      # Multiple major versions
    "mysql:10"           # MySQL versions
    "redis:10"           # Redis versions
    "nginx:15"           # Nginx versions
    "docker:10"          # Docker versions
)

HIGH_FREQ_FAILED=0
for entry in "${HIGH_FREQ_PACKAGES[@]}"; do
    pkg="${entry%%:*}"
    min_versions="${entry##*:}"

    VERSION_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT version) FROM package_versions WHERE attribute_path = '$pkg' AND version != 'unknown' AND version != '';")

    if [[ $VERSION_COUNT -ge $min_versions ]]; then
        echo -e "  $pkg: ${GREEN}$VERSION_COUNT versions${NC} (expected $min_versions+)"
    elif [[ $VERSION_COUNT -gt 0 ]]; then
        echo -e "  $pkg: ${YELLOW}$VERSION_COUNT versions${NC} (expected $min_versions+)"
        HIGH_FREQ_FAILED=1
    else
        echo -e "  $pkg: ${RED}NO VERSIONS${NC} (expected $min_versions+)"
        HIGH_FREQ_FAILED=1
    fi
done

if [[ $HIGH_FREQ_FAILED -eq 1 ]]; then
    echo ""
    echo -e "${YELLOW}WARNING: Some high-frequency packages have fewer versions than expected.${NC}"
fi
echo ""

# Test 8c: Yearly version distribution check
echo "=========================================="
echo "8c. Yearly Version Distribution"
echo "=========================================="
echo ""
echo "Checking that popular packages have versions across all indexed years..."
echo ""

# Get the year range from the database
FIRST_YEAR=$(sqlite3 "$DB_PATH" "SELECT strftime('%Y', datetime(MIN(first_commit_date), 'unixepoch')) FROM package_versions WHERE first_commit_date IS NOT NULL;")
LAST_YEAR=$(sqlite3 "$DB_PATH" "SELECT strftime('%Y', datetime(MAX(last_commit_date), 'unixepoch')) FROM package_versions WHERE last_commit_date IS NOT NULL;")

echo "Index year range: $FIRST_YEAR to $LAST_YEAR"
echo ""

# Packages that should have versions in every year
YEARLY_CHECK_PACKAGES="firefox,chromium,python3,nodejs,git"
YEARLY_FAILED=0

for pkg in ${YEARLY_CHECK_PACKAGES//,/ }; do
    echo "  $pkg:"
    YEARS_WITH_DATA=$(sqlite3 "$DB_PATH" "
        SELECT GROUP_CONCAT(DISTINCT strftime('%Y', datetime(first_commit_date, 'unixepoch')))
        FROM package_versions
        WHERE attribute_path = '$pkg'
        AND version != 'unknown'
        AND version != ''
        AND first_commit_date IS NOT NULL;
    ")

    if [[ -z "$YEARS_WITH_DATA" ]]; then
        echo -e "    ${RED}NO DATA${NC}"
        YEARLY_FAILED=1
        continue
    fi

    # Check each year
    MISSING_YEARS=""
    for year in $(seq "$FIRST_YEAR" "$LAST_YEAR"); do
        if [[ ! "$YEARS_WITH_DATA" =~ $year ]]; then
            MISSING_YEARS="$MISSING_YEARS $year"
        fi
    done

    if [[ -z "$MISSING_YEARS" ]]; then
        echo -e "    ${GREEN}Data in all years ($FIRST_YEAR-$LAST_YEAR)${NC}"
    else
        echo -e "    ${YELLOW}Missing years:$MISSING_YEARS${NC}"
        YEARLY_FAILED=1
    fi
done

if [[ $YEARLY_FAILED -eq 1 ]]; then
    echo ""
    echo -e "${YELLOW}WARNING: Some packages are missing data for certain years.${NC}"
    echo "  This may indicate gaps in indexing for those periods."
fi
echo ""

# Test 8d: Edge case packages (special naming patterns)
echo "=========================================="
echo "8d. Edge Case Packages"
echo "=========================================="
echo ""
echo "Checking packages with special naming patterns..."
echo ""

# These packages have unusual patterns that might be missed
EDGE_CASES=(
    # Packages with numeric suffixes
    "python2:5"
    "python3:15"
    "python311:3"
    "python312:3"
    "nodejs_18:5"
    "nodejs_20:5"
    "nodejs_22:3"
    # Packages with hyphens
    "google-chrome:5"
    "libreoffice-fresh:10"
    "libreoffice-still:5"
    # Packages ending in -unwrapped (wrappers)
    "firefox-unwrapped:15"
    "chromium-unwrapped:15"
    # Go packages
    "go_1_21:3"
    "go_1_22:3"
    # Rust packages
    "rust:15"
    "cargo:15"
    # JDK variants
    "openjdk:10"
    "openjdk11:5"
    "openjdk17:5"
    "openjdk21:3"
    # Database variants
    "postgresql_15:3"
    "postgresql_16:3"
    "mysql80:3"
)

EDGE_FAILED=0
for entry in "${EDGE_CASES[@]}"; do
    pkg="${entry%%:*}"
    min_versions="${entry##*:}"

    VERSION_COUNT=$(sqlite3 "$DB_PATH" "SELECT COUNT(DISTINCT version) FROM package_versions WHERE attribute_path = '$pkg' AND version != 'unknown' AND version != '';")

    if [[ $VERSION_COUNT -ge $min_versions ]]; then
        echo -e "  $pkg: ${GREEN}$VERSION_COUNT versions${NC}"
    elif [[ $VERSION_COUNT -gt 0 ]]; then
        echo -e "  $pkg: ${YELLOW}$VERSION_COUNT versions${NC} (expected $min_versions+)"
        # Don't fail for edge cases, just warn
    else
        # Only warn, don't fail - some edge cases may legitimately not exist in all indexes
        echo -e "  $pkg: ${YELLOW}not found or no versions${NC}"
    fi
done
echo ""

# Test 9: Pre vs Post July 2023 comparison
echo "=========================================="
echo "9. Pre vs Post July 2023 Comparison"
echo "=========================================="
echo ""

echo "Pre-July 2023 validation:"
run_validation "Pre-July 2023" "--packages firefox,chromium,python3,nodejs,git --before 2023-07-25"

echo "Post-July 2023 validation:"
run_validation "Post-July 2023" "--packages firefox,chromium,python3,nodejs,git --after 2023-07-25"

# Summary
echo "=========================================="
echo "VALIDATION SUMMARY"
echo "=========================================="
echo ""

if [[ $FAILED -eq 0 ]]; then
    echo -e "${GREEN}Validation completed successfully!${NC}"
    echo ""
    echo "Next steps:"
    echo "  1. Review the detailed reports above"
    echo "  2. If completeness >= $TARGET_COMPLETENESS%, index is ready for use"
    echo "  3. Run: nxv publish --url-prefix <url> --secret-key <key>"
    if [[ -n "$OUTPUT_DIR" ]]; then
        echo ""
        echo "Reports saved to: $OUTPUT_DIR/"
    fi
    exit 0
else
    echo -e "${RED}Some validation checks failed!${NC}"
    echo ""
    echo "Please review the issues above before publishing."
    exit 1
fi
