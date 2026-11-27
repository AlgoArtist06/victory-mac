#!/bin/sh
# Before a commit: every macOS test, and the Windows builds still compile.
# Fails (non-zero) on any failure, so `sh mac/check.sh && git commit` only
# commits a green tree.
set -u
. /Volumes/victory-dev/env.sh
cd "$(dirname "$0")/.."
OUT=$(mktemp)
cargo test --release --target x86_64-apple-darwin --workspace >"$OUT" 2>&1
tests=$?
grep -E '^test result' "$OUT" | awk '{p+=$4; f+=$6} END {print "macOS tests: " p " passed, " f " failed"}'
grep -E 'FAILED|panicked' "$OUT" | head -5
PATH=/Volumes/xo/toolchain/llvm-mingw-20261006-ucrt-macos-universal/bin:$PATH
cargo check --target x86_64-pc-windows-gnullvm -p victory --message-format short >"$OUT" 2>&1
windows=$?
cargo check --target x86_64-pc-windows-gnullvm -p victory --features diagnostics --message-format short >>"$OUT" 2>&1
diag=$?
grep -E '^[^ ]+: error' "$OUT" | head -5
rm -f "$OUT"
if [ $tests -ne 0 ] || [ $windows -ne 0 ] || [ $diag -ne 0 ]; then
  echo "check FAILED (tests $tests, windows $windows, windows diagnostics $diag)"
  exit 1
fi
echo "check passed"
