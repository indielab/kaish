#!/bin/bash
# shellcheck-bourne-subset.sh
#
# Validates that kaish's Bourne-compatible test cases pass ShellCheck.
# This ensures our design goal of "Bourne subset passes shellcheck --enable=all"
# is actually maintained.
#
# Usage: ./scripts/shellcheck-bourne-subset.sh
#
# Excluded warnings (false positives for syntax fragment tests):
#   SC2034: X appears unused (we test syntax, not complete scripts)
#   SC2154: VAR is referenced but not assigned (same reason)
#   SC2164: Use cd ... || exit (not relevant for syntax tests)
#   SC2250: Prefer braces (kaish accepts both $VAR and ${VAR})
#
# We use --shell=bash because kaish is "Bourne-lite", not POSIX sh.
# Specifically, kaish uses [[ ]] for tests, which is a bash-ism.
#
# The point is to catch warnings about problematic constructs (word splitting,
# backticks, etc.), not false positives from testing syntax in isolation.

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# ShellCheck exclusions for syntax fragment testing
# These are false positives when testing individual statements
#   SC2312: Consider invoking command separately (pipes in syntax tests)
SHELLCHECK_EXCLUDES="--exclude=SC2034,SC2154,SC2164,SC2250,SC2312"

# Use bash since kaish is "Bourne-lite" (uses [[ ]], not [ ])
SHELLCHECK_SHELL="bash"

# Check shellcheck is installed
if ! command -v shellcheck &>/dev/null; then
    echo -e "${RED}Error: shellcheck not found${NC}"
    echo "Install with: pacman -S shellcheck  (or your package manager)"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
TEST_DIR="$PROJECT_ROOT/tests/parser"

# Create temp file for extracted test cases
TEMP_FILE=$(mktemp --suffix=.sh)
TEMP_COMBINED=$(mktemp --suffix=.sh)
trap 'rm -f "$TEMP_FILE" "$TEMP_COMBINED"' EXIT

echo "ShellCheck Alignment Validation"
echo "================================"
echo ""

TOTAL=0
PASSED=0
FAILED=0
SKIPPED=0

# Process each .test file
for test_file in "$TEST_DIR"/*.test; do
    [[ -f "$test_file" ]] || continue

    # Read file and extract Bourne-compatible tests
    in_test=false
    is_bourne=false
    test_name=""
    expect_ok=false
    input_section=false

    while IFS= read -r line || [[ -n "$line" ]]; do
        # Test header
        if [[ "$line" =~ ^#\ test:\ (.+) ]]; then
            test_name="${BASH_REMATCH[1]}"
            is_bourne=false
            expect_ok=false
            input_section=false
            continue
        fi

        # Check for bourne marker
        if [[ "$line" =~ ^#\ bourne:\ yes ]]; then
            is_bourne=true
            continue
        fi

        # Check expect
        if [[ "$line" =~ ^#\ expect:\ ok ]]; then
            expect_ok=true
            continue
        fi

        # Start of input section
        if [[ "$line" == "---" && "$input_section" == false ]]; then
            input_section=true
            > "$TEMP_FILE"  # Clear temp file
            continue
        fi

        # End of input section (start of expected output)
        if [[ "$line" == "---" && "$input_section" == true ]]; then
            input_section=false

            # Only validate if marked as Bourne and expect: ok
            if [[ "$is_bourne" == true && "$expect_ok" == true ]]; then
                TOTAL=$((TOTAL + 1))

                # Run shellcheck on the extracted input
                # shellcheck disable=SC2086
                if shellcheck --enable=all --shell="$SHELLCHECK_SHELL" $SHELLCHECK_EXCLUDES "$TEMP_FILE" 2>/dev/null; then
                    echo -e "${GREEN}✓${NC} $test_name"
                    PASSED=$((PASSED + 1))
                else
                    echo -e "${RED}✗${NC} $test_name"
                    echo "  Input:"
                    sed 's/^/    /' "$TEMP_FILE"
                    echo "  ShellCheck output:"
                    # shellcheck disable=SC2086
                    shellcheck --enable=all --shell="$SHELLCHECK_SHELL" $SHELLCHECK_EXCLUDES "$TEMP_FILE" 2>&1 | sed 's/^/    /'
                    FAILED=$((FAILED + 1))
                fi
            fi

            is_bourne=false
            continue
        fi

        # End of test
        if [[ "$line" == "===" ]]; then
            in_test=false
            continue
        fi

        # Accumulate input lines
        if [[ "$input_section" == true ]]; then
            echo "$line" >> "$TEMP_FILE"
        fi
    done < "$test_file"
done

echo ""
echo "================================"
echo -e "Results: ${GREEN}$PASSED passed${NC}, ${RED}$FAILED failed${NC}, $TOTAL total"

if [[ $FAILED -gt 0 ]]; then
    echo -e "${RED}ShellCheck alignment check failed!${NC}"
    exit 1
fi

if [[ $TOTAL -eq 0 ]]; then
    echo -e "${YELLOW}Warning: No tests marked with '# bourne: yes' found${NC}"
    echo "Add '# bourne: yes' to Bourne-compatible test cases in tests/parser/*.test"
    exit 0
fi

echo -e "${GREEN}All Bourne-subset tests pass shellcheck --enable=all${NC}"
