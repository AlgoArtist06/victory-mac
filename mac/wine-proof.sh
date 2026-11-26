#!/bin/sh
# Proof run under Wine (data only, not the target): runs the Windows build of
# victory on the title with drawing off and logs memory every 5 seconds.
# Usage: mac/wine-proof.sh <seconds, at most 300> [victory run arguments...]
# Needs: the Windows build (x86_64-pc-windows-gnullvm) with mac/wine-proof.patch
# applied (Wine-only workarounds: GetLogicalDrives never returns in this
# process under Wine 11, and Wine's write watch faults stop Rosetta inside
# AVX2 masked stores), a Wine runtime in $RT, the title folder in $GAME.
# VICTORY_NO_DRAWS=0 draws.
set -u
SECS=$1; shift
# Every run lasts at most 5 minutes.
[ "$SECS" -gt 300 ] && SECS=300
OUT=${OUT:-/Volumes/victory-dev/proof}
RT=${RT:-/Volumes/xo/dist/runtimes/d3dmetal-4.0b3}
GAME=${GAME:-/Volumes/T7/alpha/GTA6.Xbox.v1.0.2.1/GTA6.Xbox.v1.0.2.1}
EXE=${EXE:-/Volumes/victory-dev/target/x86_64-pc-windows-gnullvm/release/victory.exe}
# One instance at a time: refuse to start beside another, and always stop
# ours (and its Wine server) on the way out, whatever happens.
running() { ps -axo pid,command | awk '$2 ~ /victory.exe$/ && $3 == "run" {print $1}'; }
if [ -n "$(running)" ]; then
  echo "another victory.exe is running; stop it first" >&2
  exit 1
fi
mkdir -p "$OUT"
export WINEMSYNC=1 WINEDEBUG=${WINEDEBUG:--all} WINEPREFIX=${WINEPREFIX:-/Volumes/xo/bottles/victory}
export DYLD_FALLBACK_LIBRARY_PATH=$RT/lib:/usr/lib CX_APPLEGPTK_LIBD3DSHARED_PATH=$RT/lib/external/libd3dshared.dylib
export ROSETTA_ADVERTISE_AVX=1 VICTORY_NO_DRAWS=${VICTORY_NO_DRAWS:-1} VICTORY_NO_WRITE_WATCH=1
WGAME="Z:$(echo "$GAME" | tr / '\\')"
export VICTORY_GAME_DIR=$WGAME
[ "$VICTORY_NO_DRAWS" = 0 ] && unset VICTORY_NO_DRAWS
trap 'pkill -f "victory.exe run"; "$RT/bin/wineserver" -k 2>/dev/null' EXIT INT TERM
"$RT/bin/wine" "$EXE" run "$@" "$WGAME\\game_scarlett_master.exe" > "$OUT/log.txt" 2>&1 &
sleep 3
echo "seconds rss_mb footprint swap_used free_pct cpu_pct" > "$OUT/mem.tsv"
t=0
while [ $t -lt "$SECS" ]; do
  P=$(running)
  [ -z "$P" ] && { echo "exited after ${t}s" >> "$OUT/mem.tsv"; break; }
  set -- $(ps -o rss=,pcpu= -p "$P")
  F=$(footprint -p "$P" 2>/dev/null | awk '/phys_footprint:/ {print $2 $3; exit}')
  S=$(sysctl -n vm.swapusage | awk '{print $6}')
  M=$(memory_pressure | awk '/free percentage/ {print $5}')
  echo "$t $(($1 / 1024)) $F $S $M $2" >> "$OUT/mem.tsv"
  sleep 5; t=$((t + 5))
done
sed 's/\x1b\[[0-9;]*m//g' "$OUT/log.txt" | grep -a -v -e '^[[:space:]]' -e mvk- -e msync -e FreeType -e freetype -e TrueType -e 'or equal' > "$OUT/clean.txt"
tail -5 "$OUT/mem.tsv"; tail -8 "$OUT/clean.txt" | cut -c1-220
