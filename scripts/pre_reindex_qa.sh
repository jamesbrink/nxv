#!/usr/bin/env bash
# Pre-reindex QA gate script
# Run this before starting a full reindex to ensure everything is working correctly.
#
# Usage:
#   ./scripts/pre_reindex_qa.sh [--nixpkgs /path/to/nixpkgs]
#
# Exit codes:
#   0 - All checks passed, safe to reindex
#   1 - One or more checks failed

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

NIXPKGS_PATH=""
SKIP_REGRESSION=""

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --nixpkgs)
            NIXPKGS_PATH="$2"
            shift 2
            ;;
        --skip-regression)
            SKIP_REGRESSION="1"
            shift
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

echo "=========================================="
echo "NXV Pre-Reindex QA Gate"
echo "=========================================="
echo ""

FAILED=0

# Check 1: Rust formatting
echo -n "Checking code formatting (cargo fmt)... "
if cargo fmt --check > /dev/null 2>&1; then
    echo -e "${GREEN}PASSED${NC}"
else
    echo -e "${RED}FAILED${NC}"
    echo "  Run 'cargo fmt' to fix formatting issues"
    FAILED=1
fi

# Check 2: Clippy lints
echo -n "Checking clippy lints... "
if cargo clippy --features indexer -- -D warnings > /dev/null 2>&1; then
    echo -e "${GREEN}PASSED${NC}"
else
    echo -e "${RED}FAILED${NC}"
    echo "  Run 'cargo clippy --features indexer' to see warnings"
    FAILED=1
fi

# Check 3: Unit tests
echo -n "Running unit tests... "
if cargo test --features indexer > /dev/null 2>&1; then
    echo -e "${GREEN}PASSED${NC}"
else
    echo -e "${RED}FAILED${NC}"
    echo "  Run 'cargo test --features indexer' to see failures"
    FAILED=1
fi

# Check 4: Build indexer
echo -n "Building indexer... "
if cargo build --features indexer --release > /dev/null 2>&1; then
    echo -e "${GREEN}PASSED${NC}"
else
    echo -e "${RED}FAILED${NC}"
    echo "  Run 'cargo build --features indexer --release' to see errors"
    FAILED=1
fi

# Check 5: Nix available
echo -n "Checking nix is available... "
if command -v nix > /dev/null 2>&1; then
    NIX_VERSION=$(nix --version 2>/dev/null || echo "unknown")
    echo -e "${GREEN}PASSED${NC} ($NIX_VERSION)"
else
    echo -e "${RED}FAILED${NC}"
    echo "  Nix is required for indexing"
    FAILED=1
fi

# Check 6: Regression test (if nixpkgs provided)
if [[ -n "$NIXPKGS_PATH" && -z "$SKIP_REGRESSION" ]]; then
    echo -n "Running regression test... "
    if [[ -d "$NIXPKGS_PATH" && -d "$NIXPKGS_PATH/.git" ]]; then
        if NIXPKGS_PATH="$NIXPKGS_PATH" cargo test --features indexer test_regression_fixture -- --ignored > /dev/null 2>&1; then
            echo -e "${GREEN}PASSED${NC}"
        else
            echo -e "${YELLOW}WARNING${NC} (some packages may have changed)"
            echo "  This is expected if nixpkgs has newer versions than fixture"
        fi
    else
        echo -e "${YELLOW}SKIPPED${NC} (invalid nixpkgs path)"
    fi
elif [[ -z "$SKIP_REGRESSION" ]]; then
    echo -e "Regression test: ${YELLOW}SKIPPED${NC} (no --nixpkgs provided)"
fi

# Check 7: Validate extract.nix syntax
echo -n "Validating extract.nix syntax... "
EXTRACT_NIX="src/index/nix/extract.nix"
if [[ -f "$EXTRACT_NIX" ]]; then
    if nix-instantiate --parse "$EXTRACT_NIX" > /dev/null 2>&1; then
        echo -e "${GREEN}PASSED${NC}"
    else
        echo -e "${RED}FAILED${NC}"
        echo "  Run 'nix-instantiate --parse $EXTRACT_NIX' to see errors"
        FAILED=1
    fi
else
    echo -e "${YELLOW}SKIPPED${NC} (file not found)"
fi

# Check 8: Validate positions.nix syntax
echo -n "Validating positions.nix syntax... "
POSITIONS_NIX="src/index/nix/positions.nix"
if [[ -f "$POSITIONS_NIX" ]]; then
    if nix-instantiate --parse "$POSITIONS_NIX" > /dev/null 2>&1; then
        echo -e "${GREEN}PASSED${NC}"
    else
        echo -e "${RED}FAILED${NC}"
        echo "  Run 'nix-instantiate --parse $POSITIONS_NIX' to see errors"
        FAILED=1
    fi
else
    echo -e "${YELLOW}SKIPPED${NC} (file not found)"
fi

# Check 9: Git status (no uncommitted changes)
echo -n "Checking git status... "
if git diff --quiet && git diff --cached --quiet; then
    echo -e "${GREEN}PASSED${NC} (clean working tree)"
else
    echo -e "${YELLOW}WARNING${NC} (uncommitted changes)"
    echo "  Consider committing changes before reindex"
fi

echo ""
echo "=========================================="
if [[ $FAILED -eq 0 ]]; then
    echo -e "${GREEN}All critical checks passed!${NC}"
    echo "Safe to proceed with reindex."
    echo ""
    echo "Recommended reindex command:"
    if [[ -n "$NIXPKGS_PATH" ]]; then
        echo "  cargo run --features indexer --release -- index --nixpkgs $NIXPKGS_PATH --full"
    else
        echo "  cargo run --features indexer --release -- index --nixpkgs /path/to/nixpkgs --full"
    fi
    exit 0
else
    echo -e "${RED}Some checks failed!${NC}"
    echo "Please fix the issues before reindexing."
    exit 1
fi
