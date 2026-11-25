# What does devload itself cost?

Measured on 2026-11-25 on one Mac: `Mac17,9`, Apple M5 Pro (18 cores), 24 GB RAM, macOS 26.5.1 (25F80), about 730 processes running. A normal working afternoon, not an idle machine.

## Summary

| What | Result | How |
|---|---|---|
| CPU, installed app, popover closed | **0.20 % of one core** on average (3.66 s of CPU in 30 min) | `ps -o time` delta over 30 min |
| CPU, `top` samples | mean 0.15 %, max 0.5 % (168 samples) | `top -l 2 -s 5` every 10 s |
| Energy impact (`top` POWER) | mean 0.15, max 0.5 | same samples |
| Idle wakeups | 80 in 30 min (2.7 per minute) | `top` IDLEW, start vs end |
| Memory (physical footprint) | **28 MB** at start, 29 MB after 30 min, peak 33 MB | `footprint <pid>` |
| Resident size | 26 to 44 MB | `ps -o rss` |
| App size on disk | 4.1 MB | `du -sh Devload.app` |
| One tick (every 5 s), real cadence | **6.6 ms CPU** mean, p95 8.4 ms, max 9.4 ms (0.13 % of one core) | `tick_cost` example, 60 ticks |
| One tick, hot loop | 0.93 ms | `cargo bench --bench tick` |
| First tick after launch | 26 to 30 ms | every process is new, nothing cached |
| Database after 7 days | 29 MB | `store_fill`, simulated |
| Database after 30 / 91 days | 35 MB / 48 MB, then flat (retention) | `store_fill`, simulated |
| Disk writes | about 2 MB per hour (WAL + checkpoint) | `store_fill`, last 24 h |
| Flush (every 5 min) | 0.25 ms | `store_fill` |
| Hourly maintenance | median 2.6 to 4.0 ms, max 25 ms | `store_fill` |
| Popover graph, core side, 91-day database | 7 days: 38 ms (Pressure) / 7 ms (Groups); 24 h: 9 / 2 ms; 1 h: < 0.1 ms | `engine_live` example, single cold run |

## Method

### 1. The installed app

The installed `/Applications/Devload.app` was sampled for 30 minutes with the popover closed, read-only: `ps`, `top` and `footprint`, no signal, no debugger. The app had been running for about 15 minutes when sampling started. `scripts/bench/sample_running.sh` does this and prints the summary.

The process total includes everything: the Rust core ticking every 5 s, the Docker scan, and the Swift side redrawing the menu bar figure when it changes.

Idle wakeups are low partly because the machine was busy: macOS counts a wakeup only when the CPU was idle.

### 2. Tick cost

`Engine::tick` is the whole 5 s cycle: list every process, one `proc_pid_rusage` per process (more facts at first sight only), group them by project, accumulate the minute, write to SQLite every 5 minutes, maintenance every hour. The cost is the ticking thread's CPU time (`Engine::last_cost_ns`).

Two numbers, because they differ by 7x:

- `cargo run --release --example tick_cost` ticks every 5 s like the app: **6.6 ms**. This is the realistic one, and it matches the installed app (3.66 s CPU / 360 ticks = 10 ms per tick for the whole process, UI included).
- `cargo bench --bench tick` (criterion) ticks back to back on a simulated clock: 0.93 ms. Same work, but caches are hot and the core stays clocked up. It is a lower bound, useful to catch regressions, not what the app costs.

### 3. Storage

`core/examples/store_fill.rs` simulates a busy dev machine (about 25 groups over 50 MiB at any time, top 3 processes each, process churn, ~3 pressure episodes a day) on a fast clock, through the real `Store::flush` every 5 minutes and `Store::maintenance` every hour. Minute rows are kept 7 days, hour rows 90 days, so:

- the first 7 days grow about 4 MB per day (29 MB at day 7),
- after that, about 0.2 to 0.3 MB per day (hour rows only),
- after 90 days the size stays near 48 MB.

These volumes are simulated, not read from a real database. Your size depends on how many projects and processes you run.

### 4. Popover open

The click to graph drawn latency is logged by builds launched with `DEVLOAD_TRACE=1`. A second instance can run beside the installed one (the single-instance lock is per data folder, so `DEVLOAD_DATA_DIR` on a scratch folder is enough), but measuring it needs a real click on its menu bar item. That was not done here, to stay off the GUI; the number is missing.

What was measured is the core side of an open: `Engine::series` for each period and mode on the 91-day simulated database (`engine_live` example, one cold run). The 7-day Pressure graph is the slowest at 38 ms. In the same database, reading 7 days of `machine_minute` rows takes 0.9 ms and the 7-day group read 16 ms (`store_fill`, median of 30), so the first open pays for loading, not for drawing.

## Reproduce

From the repo root, Rust toolchain on `PATH`:

```sh
# 1. The running app, read-only, 30 minutes
scripts/bench/sample_running.sh 30 build/bench-live

# 2-4. Tick, storage and graph reads, into scratch folders under build/bench-core
scripts/bench/run_core.sh build/bench-core
```

`run_core.sh` runs, in order:

```sh
cargo bench --manifest-path core/Cargo.toml --bench tick
cargo run -q --release --manifest-path core/Cargo.toml --example tick_cost -- 60 build/bench-core/tick-cost-data
cargo run -q --release --manifest-path core/Cargo.toml --example store_fill -- build/bench-core/store-91 91   # also 7 and 30
cargo run -q --release --manifest-path core/Cargo.toml --example engine_live -- build/bench-core/store-91
```

Nothing writes to `~/Library/Application Support/devload`. Close heavy jobs before running: a build in the background changes every number here.
