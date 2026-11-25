#!/bin/bash
# Read-only sampling of a running Devload instance.
# Usage: scripts/bench/sample_running.sh [minutes=30] [out_dir=build/bench-live]
# Never signals or attaches to the process: only ps, top, footprint.
set -u
MIN=${1:-30}
OUT=${2:-build/bench-live}
PID=$(pgrep -f 'Devload.app/Contents/MacOS/Devload' | head -1)
[ -z "$PID" ] && { echo "Devload is not running" >&2; exit 1; }
mkdir -p "$OUT"
echo "pid=$PID minutes=$MIN out=$OUT"

# CPU time in seconds from ps "time" (m:ss.cc or h:mm:ss)
cputime() { ps -o time= -p "$PID" | awk -F: '{ if (NF==3) print $1*3600+$2*60+$3; else print $1*60+$2 }'; }

footprint "$PID" > "$OUT/footprint-start.txt" 2>&1
echo "epoch,cpu_pct,rss_kb,cputime_s" > "$OUT/ps.csv"
echo "epoch,cpu,power,idlew,mem" > "$OUT/top.csv"
T0=$(date +%s); C0=$(cputime)
END=$((T0 + MIN * 60))
while [ "$(date +%s)" -lt "$END" ]; do
  now=$(date +%s)
  read -r pcpu rss <<<"$(ps -o %cpu=,rss= -p "$PID")"
  echo "$now,$pcpu,$rss,$(cputime)" >> "$OUT/ps.csv"
  # top -l 2: the second sample has real deltas over -s seconds
  top -l 2 -s 5 -pid "$PID" -stats pid,cpu,power,idlew,mem \
    | awk -v t="$now" -v p="$PID" '$1==p {line=$0} END {split(line,a," "); print t","a[2]","a[3]","a[4]","a[5]}' >> "$OUT/top.csv"
  sleep 5
done
T1=$(date +%s); C1=$(cputime)
footprint "$PID" > "$OUT/footprint-end.txt" 2>&1
python3 - "$OUT" "$T0" "$T1" "$C0" "$C1" <<'EOF'
import csv, sys, statistics as st
out, t0, t1, c0, c1 = sys.argv[1], *map(float, sys.argv[2:])
wall = t1 - t0; cpu = c1 - c0
rows = list(csv.DictReader(open(f"{out}/top.csv")))
def col(k):
    v = []
    for r in rows:
        try: v.append(float(r[k].rstrip("MK+-")))
        except (ValueError, AttributeError): pass
    return v
pw, cp, iw = col("power"), col("cpu"), col("idlew")
ps = list(csv.DictReader(open(f"{out}/ps.csv")))
rss = [int(r["rss_kb"]) for r in ps]
print(f"wall {wall:.0f} s, cpu time {cpu:.2f} s -> avg CPU {100*cpu/wall:.3f} %")
print(f"top cpu%: mean {st.mean(cp):.2f} max {max(cp):.2f} (n={len(cp)})")
print(f"top power: mean {st.mean(pw):.2f} max {max(pw):.2f}")
# top's IDLEW is a running total since the process started
print(f"idle wakeups: {iw[-1]-iw[0]:.0f} over the run = {(iw[-1]-iw[0])/wall*60:.1f}/min")
print(f"rss MB: start {rss[0]/1024:.1f} end {rss[-1]/1024:.1f} max {max(rss)/1024:.1f}")
EOF
grep -E 'Footprint|phys_footprint' "$OUT"/footprint-*.txt | head
