#!/bin/sh
set -eu

DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/ohm_player"

if [ -x "$BIN" ]; then
    exec "$BIN"
fi

echo "Ohm Player binary not found next to this launcher." >&2
echo "Build a release package first, or run from the project root with cargo." >&2
exit 1
