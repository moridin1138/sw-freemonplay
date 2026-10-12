#!/usr/bin/env bash
# Builds the x86_64 Linux client in Docker (works from macOS, including Apple Silicon).
# Output: target/linux-x86_64/release/freemonplay-screen
set -euo pipefail

cd "$(dirname "$0")/../.."
image=freemonplay-linux-build

if ! docker info >/dev/null 2>&1; then
    echo "Docker isn't running. Start Docker Desktop (open -a Docker) and run this again." >&2
    exit 1
fi

docker build --platform linux/amd64 -t "$image" build/linux
docker run --rm --platform linux/amd64 \
    -v "$PWD":/src \
    -v freemonplay-cargo-registry:/root/.cargo/registry \
    -e CARGO_TARGET_DIR=/src/target/linux-x86_64 \
    "$image" \
    cargo build --release --locked -p freemonplay-screen

echo
echo "Built target/linux-x86_64/release/freemonplay-screen"
