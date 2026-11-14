#!/bin/bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
build="$root/target/selftest"
mkdir -p "$build"
(cd "$root" && unset CARGO_TARGET_DIR && cargo build --release -p hikari-vrm)
clang++ -O2 -std=c++17 -fobjc-arc -Wall -Wextra -I"$here" "$here/selftest.mm" "$root/target/release/libhikari_vrm.a" \
    -framework Metal -framework Foundation -framework ImageIO -framework CoreGraphics -framework AppKit -framework QuartzCore -framework IOSurface -o "$build/engine_selftest"
echo "$build/engine_selftest"
