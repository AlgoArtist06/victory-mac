#!/bin/sh
# Builds and runs the platform probe as an x86_64 (Rosetta) program.
set -e
cd "$(dirname "$0")"
clang -arch x86_64 -O1 -fobjc-arc -framework Metal -framework Foundation probe.m -o "${TMPDIR:-/tmp}/victory-probe"
exec "${TMPDIR:-/tmp}/victory-probe"
