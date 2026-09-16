#!/usr/bin/env bash
# Runs ON the box, as root. The three runs a reviewer needs, on one machine, one datadir, one
# block, each from a cold page cache, each writing the trie tables end to end:
#
#   1. PartitionedStateRoot, writing, on reth's default thread count (the available
#      parallelism, which is what reth sizes its CPU pool to). First, so that a build that does
#      not scale shows up in its CPU usage within minutes, not hours.
#   2. reth's own full rebuild: the merkle stage's serial walk, nodes committed in chunks.
#      Skip with BASELINE=0 when its number for this datadir already exists.
#   3. PartitionedStateRoot, writing, on one thread: the control that per-leaf cost matches
#      reth's. Skip with SERIAL=0.
#
# After each run the binary digests the trie tables it left behind and has reth's own walker
# derive the root from them. The summary checks that all digests agree, so the tables are shown
# identical, not just the root.
#
# Results land in ~/results/<timestamp>/ as plain text, with summary.txt at the end.
set -euo pipefail
# shellcheck disable=SC1091
source "$HOME/.cargo/env"
DATADIR="${DATADIR:-$HOME/.local/share/reth/mainnet}"
BIN="${BIN:-$HOME/reth-trie-rebuild-bench/target/release/datadir_bench}"
BASELINE="${BASELINE:-1}"
SERIAL="${SERIAL:-1}"
OUT="${OUT:-$HOME/results/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$OUT"

{ du -sh "$DATADIR"/db "$DATADIR"/static_files 2>/dev/null || true; } | tee "$OUT/sizes.txt"
{ lscpu | grep -E "Model name|^CPU\(s\)|Thread|Core"; free -g; echo "nproc $(nproc)"; } | tee "$OUT/cpu.txt"
# The shipped tree has no .git, so the bench commit is best effort; the reth revision is what matters.
REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
{
  echo "binary $BIN"
  echo "bench commit $(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "reth rev $(grep -m1 'rev = ' "$REPO_DIR/Cargo.toml" | sed 's/.*rev = "\([^"]*\)".*/\1/')"
} | tee "$OUT/versions.txt"

# Cold cache only where we can; a non-root dry run just skips it.
drop_caches() { if [ "$(id -u)" = 0 ]; then sync; echo 3 > /proc/sys/vm/drop_caches; fi; }

# Tip block + expected root from a public RPC, so the binary can verify itself. EXPECTED_ROOT
# overrides, for datadirs no RPC knows.
TIP=$("$BIN" --datadir "$DATADIR" --threads 1 --repeat 0 | awk '/^tip block/ {print $3}')
echo "tip block $TIP" | tee "$OUT/tip.txt"
if [ -z "${EXPECTED_ROOT:-}" ]; then
  HEX=$(printf '0x%x' "$TIP")
  EXPECTED_ROOT=$(curl -s -X POST -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBlockByNumber\",\"params\":[\"$HEX\",false]}" \
    https://ethereum-rpc.publicnode.com | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["stateRoot"])')
fi
echo "expected root $EXPECTED_ROOT" | tee -a "$OUT/tip.txt"

# One run: cold cache, system samplers, the binary under `time -v`, output to its own file.
run() {
  local name=$1; shift
  drop_caches
  pidstat -u -r 5 > "$OUT/pidstat-$name.txt" 2>&1 &
  local pid_stat=$!
  iostat -x 5 > "$OUT/iostat-$name.txt" 2>&1 &
  local pid_io=$!
  echo "=== $name (cold cache): $BIN $*" | tee "$OUT/$name.txt"
  /usr/bin/time -v "$BIN" --datadir "$DATADIR" --expected-root "$EXPECTED_ROOT" "$@" 2>&1 | tee -a "$OUT/$name.txt"
  kill $pid_stat $pid_io 2>/dev/null || true
}

run gen-write --write
[ "$BASELINE" = 1 ] && run reth-rebuild --reth-rebuild
[ "$SERIAL" = 1 ] && run gen-write-1 --write --threads 1

# Summary: one line per run, then the digest check.
{
  echo "datadir $DATADIR, tip block $TIP, expected root $EXPECTED_ROOT"
  cat "$OUT/versions.txt" "$OUT/cpu.txt"
  echo
  for name in reth-rebuild gen-write gen-write-1; do
    [ -f "$OUT/$name.txt" ] || continue
    threads=$(awk '/^threads / {print $2}' "$OUT/$name.txt")
    result=$(grep -E '^(reth rebuild|run 1):' "$OUT/$name.txt" | head -1)
    match=$(grep -E 'root matches expected' "$OUT/$name.txt" | head -1 || true)
    walk=$(grep -E '^reth walk over written tables' "$OUT/$name.txt" | head -1 || true)
    wall=$(awk -F': ' '/Elapsed \(wall clock\)/ {print $2}' "$OUT/$name.txt")
    cpu=$(awk -F': ' '/Percent of CPU/ {print $2}' "$OUT/$name.txt")
    digest=$(awk '/^trie tables:/ {for (i=1;i<=NF;i++) if ($i=="digest") print $(i+1)}' "$OUT/$name.txt")
    nodes=$(awk '/^trie tables:/ {for (i=2;i<=NF;i++) if ($i=="nodes" && $(i-1)!="account" && $(i-1)!="storage") print $(i+1)}' "$OUT/$name.txt")
    echo "$name: threads ${threads:-1}, wall $wall (includes the checks below), cpu $cpu"
    echo "  $result"
    echo "  ${match:-ROOT NOT CHECKED}"
    echo "  trie tables: $nodes nodes, digest $digest"
    echo "  ${walk:-RETH WALK NOT RUN}"
  done
  echo
  digests=$(awk '/^trie tables:/ {for (i=1;i<=NF;i++) if ($i=="digest") print $(i+1)}' "$OUT"/reth-rebuild.txt "$OUT"/gen-write*.txt 2>/dev/null | sort -u | wc -l)
  if [ "$digests" = 1 ]; then
    echo "PARITY OK: all runs left identical trie tables"
  else
    echo "PARITY FAILED: $digests distinct trie table digests"
  fi
} | tee "$OUT/summary.txt"

echo "results in $OUT"
