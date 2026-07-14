#!/usr/bin/env bash
# Run factorioctl tests against a running server
#
# Prerequisites:
# - Server must be running (use ./tests/setup.sh)
#
# Usage: ./tests/run_tests.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

# Configuration
RCON_PORT="${RCON_PORT:-27016}"
RCON_PASSWORD="${RCON_PASSWORD:-test_password}"
CLI=(./target/release/factorioctl --port "$RCON_PORT" --password "$RCON_PASSWORD")

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

PASSED=0
FAILED=0
SKIPPED=0

pass() {
    echo -e "  ${GREEN}PASS${NC}: $1"
    PASSED=$((PASSED + 1))
}

fail() {
    echo -e "  ${RED}FAIL${NC}: $1"
    echo "       Output: $2"
    FAILED=$((FAILED + 1))
}

skip() {
    echo -e "  ${YELLOW}SKIP${NC}: $1"
    SKIPPED=$((SKIPPED + 1))
}

echo "=== factorioctl Test Suite ==="
echo ""

# Check if server is running
echo "Checking server connection..."
if ! "${CLI[@]}" get tick > /dev/null 2>&1; then
    echo "ERROR: Cannot connect to server. Run ./tests/setup.sh first."
    exit 1
fi
echo ""

# Test 1: Basic connectivity
echo "1. Basic Connectivity"
OUTPUT=$("${CLI[@]}" get tick 2>&1 || true)
if echo "$OUTPUT" | grep -q "Tick:"; then
    pass "get tick"
else
    fail "get tick" "$OUTPUT"
fi

OUTPUT=$("${CLI[@]}" get surfaces 2>&1 || true)
if echo "$OUTPUT" | grep -q "nauvis"; then
    pass "get surfaces"
else
    fail "get surfaces" "$OUTPUT"
fi
echo ""

# Test 2: Character initialization
echo "2. Character Management"
OUTPUT=$("${CLI[@]}" character init 2>&1 || true)
if echo "$OUTPUT" | grep -q "character" || echo "$OUTPUT" | grep -q "unit_number"; then
    pass "character init"
else
    fail "character init" "$OUTPUT"
fi

OUTPUT=$("${CLI[@]}" character status 2>&1 || true)
if echo "$OUTPUT" | grep -q "valid" || echo "$OUTPUT" | grep -q "Position"; then
    pass "character status"
else
    fail "character status" "$OUTPUT"
fi
echo ""

# Test 3: World queries
echo "3. World Queries"
OUTPUT=$("${CLI[@]}" get resources --area -100,-100,100,100 2>&1 || true)
if echo "$OUTPUT" | grep -qE "(iron-ore|copper-ore|coal|stone|resource)"; then
    pass "get resources"
else
    # May be empty on some maps
    if echo "$OUTPUT" | grep -q "No resources"; then
        pass "get resources (empty)"
    else
        fail "get resources" "$OUTPUT"
    fi
fi

OUTPUT=$("${CLI[@]}" get tile 0,0 2>&1 || true)
if echo "$OUTPUT" | grep -q "Tile:"; then
    pass "get tile"
else
    fail "get tile" "$OUTPUT"
fi
echo ""

# Test 4: Character movement through the shipped walk driver
echo "4. Character Movement"
OUTPUT=$("${CLI[@]}" walk-to --run --pathfind 8,0 2>&1 || true)
if echo "$OUTPUT" | grep -q "Arrived at"; then
    pass "walk-to pathfinding"
else
    fail "walk-to pathfinding" "$OUTPUT"
fi
echo ""

# Test 5: JSON output
echo "5. JSON Output"
OUTPUT=$("${CLI[@]}" --output json get tick 2>&1 || true)
if echo "$OUTPUT" | grep -q '"tick"'; then
    pass "JSON get tick"
else
    fail "JSON get tick" "$OUTPUT"
fi

OUTPUT=$("${CLI[@]}" --output json character status 2>&1 || true)
if echo "$OUTPUT" | grep -q '"valid"'; then
    pass "JSON character status"
else
    fail "JSON character status" "$OUTPUT"
fi
echo ""

# Test 6: Tick control
echo "6. Tick Control"
OUTPUT=$("${CLI[@]}" tick pause 2>&1 || true)
if echo "$OUTPUT" | grep -q "paused"; then
    pass "tick pause"
else
    fail "tick pause" "$OUTPUT"
fi

OUTPUT=$("${CLI[@]}" tick resume 2>&1 || true)
if echo "$OUTPUT" | grep -q "resumed"; then
    pass "tick resume"
else
    fail "tick resume" "$OUTPUT"
fi
echo ""

# Summary
echo "=== Test Summary ==="
echo -e "  ${GREEN}Passed${NC}: $PASSED"
echo -e "  ${RED}Failed${NC}: $FAILED"
echo -e "  ${YELLOW}Skipped${NC}: $SKIPPED"
echo ""

if (( FAILED > 0 )); then
    exit 1
fi
