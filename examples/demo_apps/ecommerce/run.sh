#!/usr/bin/env bash
# Spid3r Mini E-Commerce Platform Server (Linux/macOS)
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

echo "======================================================================"
echo "      Starting Spid3r Mini E-Commerce Platform Server (SQLite 3)"
echo "======================================================================"

cd "$ROOT_DIR"

if [ -f "$SCRIPT_DIR/bin/sqlite3.dll" ]; then
    export SPID3R_SQLITE_PATH="$SCRIPT_DIR/bin/sqlite3.dll"
fi

if [ -f "target/release/spid3r" ]; then
    ./target/release/spid3r run examples/demo_apps/ecommerce/src/server.matrix 8080
elif [ -f "target/debug/spid3r" ]; then
    ./target/debug/spid3r run examples/demo_apps/ecommerce/src/server.matrix 8080
else
    cargo run --release -- run examples/demo_apps/ecommerce/src/server.matrix 8080
fi
