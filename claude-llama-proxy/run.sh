#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release
exec ./target/release/claude-llama-proxy
