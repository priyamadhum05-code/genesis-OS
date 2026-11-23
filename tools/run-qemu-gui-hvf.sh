#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export GENESIS_HVF=1
exec tools/run-qemu-gui.sh "$@"
