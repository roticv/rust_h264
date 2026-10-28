#!/usr/bin/env bash
# Run the JVT H.264 conformance suites (JVT-AVC_V1, JVT-FR-EXT) through
# Fluster and compare the results with expected_failures.txt.
#
# Usage: tools/conformance/run.sh [extra fluster run args, e.g. -tv CAPAMA3_Sand_F]
#
# Fluster is cloned (pinned) into $FLUSTER_DIR, default target/fluster; the
# suites (~1 GB) are downloaded into it on the first run. Exits non-zero
# when a vector outside expected_failures.txt fails, or one listed passes.
set -euo pipefail

FLUSTER_REPO=https://github.com/fluendo/fluster.git
FLUSTER_REV=5d9d2bc
SUITES=(JVT-AVC_V1 JVT-FR-EXT)

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
fluster_dir=${FLUSTER_DIR:-$root/target/fluster}

cargo build --release --example conformance_decode --manifest-path "$root/Cargo.toml"
export RUST_H264_DECODER="$root/target/release/examples/conformance_decode"

if [ ! -d "$fluster_dir/.git" ]; then
    git clone -q "$FLUSTER_REPO" "$fluster_dir"
    git -C "$fluster_dir" checkout -q "$FLUSTER_REV"
fi
cp "$here/rust_h264.py" "$fluster_dir/fluster/decoders/rust_h264.py"

cd "$fluster_dir"
for suite in "${SUITES[@]}"; do
    [ -d "resources/$suite" ] || python3 fluster.py download "$suite"
done

summary=$(mktemp)
trap 'rm -f "$summary"' EXIT
# Fluster exits non-zero when any vector fails; the comparison below decides
python3 fluster.py run -d rust_h264 -ts "${SUITES[@]}" -q -s -f json -so "$summary" "$@" \
    >/dev/null || true

python3 - "$summary" "$here/expected_failures.txt" <<'EOF'
import json
import sys

summary, expected_path = sys.argv[1:3]
expected = {
    line.strip()
    for line in open(expected_path)
    if line.strip() and not line.startswith("#")
}
results = {}
for name, suite in json.load(open(summary))["test_suites"].items():
    stats = suite["decoders"]["rust_h264"]
    print(f"{name}: {stats['passed']}/{stats['total_tests']} passed")
    for vector, r in stats["vectors"].items():
        results[vector] = r["result"] == "Success"

passed = sum(results.values())
print(f"total: {passed}/{len(results)} passed")
regressed = sorted(v for v, ok in results.items() if not ok and v not in expected)
fixed = sorted(v for v, ok in results.items() if ok and v in expected)
if regressed:
    print("unexpected failures:", " ".join(regressed))
if fixed:
    print("now passing, remove from expected_failures.txt:", " ".join(fixed))
sys.exit(1 if regressed or fixed else 0)
EOF
