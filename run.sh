#!/bin/sh
set -eu

DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/ohm_player"

if [ -x "$BIN" ]; then
    exec "$BIN"
fi

cargo run --release
