#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
CARGO_TARGET_DIR="$PWD/build/target" cargo build --release
