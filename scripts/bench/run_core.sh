#!/bin/bash
# Core benchmarks of docs/benchmarks.md, from the repo root:
#   scripts/bench/run_core.sh [out_dir=build/bench-core]
# Everything writes into scratch folders under out_dir; nothing touches
# ~/Library/Application Support/devload.
set -eu
OUT=${1:-build/bench-core}
CARGO=${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}
M=core/Cargo.toml
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)

echo "== machine"
sw_vers | tee "$OUT/machine.txt"
sysctl hw.model hw.memsize hw.ncpu machdep.cpu.brand_string | tee -a "$OUT/machine.txt"
echo "processes: $(ps -A -o pid= | wc -l | tr -d ' ')" | tee -a "$OUT/machine.txt"

echo "== tick, criterion (5 s simulated clock, flush and maintenance amortized)"
"$CARGO" bench --manifest-path $M --bench tick 2>&1 | tee "$OUT/tick-criterion.txt"

echo "== tick, real 5 s cadence (60 ticks = 5 min)"
echo "processes: $(ps -A -o pid= | wc -l | tr -d ' ')" | tee "$OUT/tick-cost.txt"
"$CARGO" run -q --release --manifest-path $M --example tick_cost -- 60 "$OUT/tick-cost-data" 2>&1 | tee -a "$OUT/tick-cost.txt"

echo "== storage: 7, 30 and 91 simulated days"
for d in 7 30 91; do
  "$CARGO" run -q --release --manifest-path $M --example store_fill -- "$OUT/store-$d" "$d" 2>&1 | tee "$OUT/store-$d.txt"
done

echo "== engine on the 91-day database: popover series reads"
"$CARGO" run -q --release --manifest-path $M --example engine_live -- "$OUT/store-91" 2>&1 \
  | grep -E '^(open|tick|series|prompt)' | tee "$OUT/engine-live.txt"
