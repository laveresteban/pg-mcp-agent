#!/usr/bin/env bash
set -euo pipefail

# Dev entrypoint: rebuild the mock_mcp_server and run it. This makes container
# iterate-fast for development: edits on the host are visible via the bind mount
# and a container restart will rebuild and run.

echo "[mock_mcp_server] Running cargo build (debug)..."
# Build the debug binary to speed up iteration. For a release build, set --release.
cargo build --bin mock_mcp_server

# Run the binary from target/debug
exec ./target/debug/mock_mcp_server
