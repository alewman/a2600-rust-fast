#!/usr/bin/env bash
# The fork's gate: every change must leave this board identical to a2600-rust.
# cargo test, rung 1 (the die), then every lockstep case against the
# reference traces (external/refcache: a2600-rust's, which equal
# a2600-python's at the pin on all 254 cases).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cargo test --release -q --manifest-path "$ROOT/Cargo.toml" 2>&1 | grep -E "test result|FAILED|panicked" | grep -v " 0 passed" || true
"$ROOT/scripts/rung1.sh" | tail -1
nice python3 "$ROOT/scripts/lockstep.py" --jobs "${JOBS:-12}" --reference-cache "$ROOT/external/refcache" | tail -1
