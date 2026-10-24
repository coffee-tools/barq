#!/usr/bin/env bash
# Run Core Lightning's own tests twice on one build, natively and with barq
# serving `pay` on every node, and list where barq is not a drop-in `pay`.
#
#   tests/cln/run.sh [test files...]      default: tests/test_pay.py
#
# It clones Core Lightning at the tag in CLN_VERSION into tests/cln/lightning
# (or uses CLN_DIR), applies fixtures-barq.patch (the TEST_BARQ_PLUGIN hook),
# builds both projects and writes junit reports to tests/cln/results.
# Core Lightning's build dependencies must be installed (doc/getting-started
# in its tree), plus uv and cargo. CONFIGURE_OPTS goes to ./configure, for
# example CONFIGURE_OPTS=--disable-rust; JOBS sets the parallelism.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
VERSION=$(cat "$HERE/CLN_VERSION")
CLN=${CLN_DIR:-$HERE/lightning}
JOBS=${JOBS:-4}
OUT=$HERE/results
if [ $# -gt 0 ]; then TESTS=("$@"); else TESTS=(tests/test_pay.py); fi

if [ ! -d "$CLN" ]; then
  git clone --depth 1 --branch "$VERSION" https://github.com/ElementsProject/lightning "$CLN"
  git -C "$CLN" submodule update --init --recursive --depth 1
fi
if ! git -C "$CLN" apply --check --reverse "$HERE/fixtures-barq.patch" 2>/dev/null; then
  git -C "$CLN" apply "$HERE/fixtures-barq.patch"
fi

(cd "$ROOT" && cargo build -p barq-plugin)
BARQ=$ROOT/target/debug/barq-plugin

cd "$CLN"
# shellcheck disable=SC2086
[ -f config.vars ] || ./configure --enable-debugbuild ${CONFIGURE_OPTS:-}
uv sync --all-extras --all-groups
uv run make -j "$JOBS"

# Without Rust plugins pyln must not pass --grpc-port.
grep -q '^RUST=0' config.vars && export RUST=0
export VALGRIND=0 TEST_DEBUG=0 SLOW_MACHINE=1
mkdir -p "$OUT"
uv run pytest "${TESTS[@]}" -n "$JOBS" --timeout=1800 --junit-xml="$OUT/native.xml" || true
TEST_BARQ_PLUGIN=$BARQ uv run pytest "${TESTS[@]}" -n "$JOBS" --timeout=1800 --junit-xml="$OUT/barq.xml" || true

python3 "$HERE/compare_junit.py" --native "$OUT/native.xml" --barq "$OUT/barq.xml" \
  --expected "$HERE/expected-failures.txt"
