#!/bin/sh
# Runs the title natively on macOS (no Wine), one instance at a time, for at
# most 600 seconds, and stops it if the Mac runs low on memory.
# Usage: mac/native-run.sh <seconds, at most 600> [victory run arguments...]
# Env: OUT (log folder), GAME (title folder), EXE (the macOS build of victory).
set -u
SECS=$1; shift
[ "$SECS" -gt 600 ] && SECS=600
OUT=${OUT:-/Volumes/victory-dev/native}
GAME=${GAME:-/Volumes/T7/alpha/GTA6.Xbox.v1.0.2.1/GTA6.Xbox.v1.0.2.1}
EXE=${EXE:-/Volumes/victory-dev/target/x86_64-apple-darwin/release/victory}
running() { ps -axo pid,command | awk '($2 ~ /victory(\.exe)?$/) && $3 == "run" {print $1}'; }
if [ -n "$(running)" ]; then
  echo "another victory instance is running; stop it first" >&2
  exit 1
fi
mkdir -p "$OUT" && cd "$OUT"
echo "seconds footprint swap_used memory_level" > "$OUT/mem.tsv"
export ROSETTA_ADVERTISE_AVX=1
"$EXE" run --execute "$@" "$GAME/game_scarlett_master.exe" > "$OUT/log.txt" 2>&1 &
PID=$!
trap 'kill -9 $PID 2>/dev/null' EXIT INT TERM
t=0
low=0
while kill -0 $PID 2>/dev/null && [ $t -lt "$SECS" ]; do
  # See wine-proof.sh: an 8 GB Mac hung at 3.5 GB of swap.
  S=$(sysctl -n vm.swapusage | awk '{print $6}')
  L=$(sysctl -n kern.memorystatus_level)
  if [ "$L" -lt 10 ]; then low=$((low + 1)); else low=0; fi
  if [ "$(echo "${S%M} > 2500" | bc)" = 1 ] || [ "$low" -ge 2 ]; then
    echo "stopped after ${t}s: memory too low (swap $S, level $L%)" >> "$OUT/log.txt"
    break
  fi
  if [ $((t % 5)) -eq 0 ]; then
    F=$(footprint -p "$PID" 2>/dev/null | awk '/phys_footprint:/ {print $2 $3; exit}')
    echo "$t $F $S $L%" >> "$OUT/mem.tsv"
  fi
  sleep 1; t=$((t + 1))
done
kill -0 $PID 2>/dev/null && { kill -9 $PID; echo "stopped at the ${SECS}s limit" >> "$OUT/log.txt"; }
wait $PID 2>/dev/null
echo "exit $? after ${t}s"
sed 's/\x1b\[[0-9;]*m//g' "$OUT/log.txt" | grep -a -v -e '^  missing' -e 'image mapped' -e '^title dll' -e 'imports bound' | tail -8 | cut -c1-250
