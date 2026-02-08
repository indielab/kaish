#!/bin/bash
set -euo pipefail

# Wrapper: find kaish binary and run publish.kai

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
KAISH="${SCRIPT_DIR}/../target/release/kaish"

if [[ ! -x "$KAISH" ]]; then
    KAISH="$(command -v kaish 2>/dev/null || true)"
fi

if [[ -z "$KAISH" ]]; then
    echo "❌ kaish not found. Run 'cargo build --release' or 'cargo install kaish' first."
    exit 1
fi

exec "$KAISH" "${SCRIPT_DIR}/publish.kai"
