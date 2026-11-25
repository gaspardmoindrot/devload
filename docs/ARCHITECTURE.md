# Architecture

devload is a single process: a Rust core, built as a static library, linked into a Swift/AppKit app. The core does the measuring, grouping and storing. The app draws the menu bar item, the popover and the graph, and posts notifications.

```
┌──────────────────── Devload.app (one process) ────────────────────┐
│                                                                   │
│  Swift shell (app/)                 Rust core (core/)             │
│  ─────────────────                  ─────────────────             │
│  AppController   ── dl_start ────▶  collector thread (5 s tick)   │
│  NSStatusItem    ◀── callback ───   engine (single-threaded,      │
│  PopoverView     ── dl_snapshot ─▶          behind a mutex)       │
│  GraphView       ── dl_series ───▶  docker thread (socket poll)   │
│  Notifier        ── dl_prompt ───▶  store (SQLite, WAL)           │
│                  ── dl_quit ─────▶  quit (SIGTERM / SIGKILL,      │
│                                           docker stop)            │
└───────────────────────────────────────────────────────────────────┘
```

## Rust core (`core/`)

| Module | What it does |
|---|---|
| `collect/` | One `Collector::tick` every 5 s. Reads machine memory, pressure and CPU (`host_statistics64`, `sysctl`), lists processes (`proc_listallpids`) and reads each one's footprint and CPU time (`proc_pid_rusage`). A process's facts (parent, executable, working directory, arguments, a whitelist of environment variables) are read once, the first time it's seen. It never starts a subprocess. |
| `collect/classify.rs`, `repo.rs`, `agents.rs` | Puts each process in a group: the git worktree its working directory is in, the Claude Code or Codex session it descends from, or the app responsible for it. |
| `docker/` | A minimal HTTP client for the Docker Engine socket (Docker Desktop, OrbStack, Colima). Groups containers by `com.docker.compose.project` and reads their memory. It runs on its own thread. |
| `engine/` | Turns scans into rows (dev projects, apps, system), minute aggregates, pressure episodes and alerts. Keeps the last hour of 5 s ticks in RAM for the 1 h graph. |
| `store.rs` | SQLite schema, one write per minute, hourly rollup, retention (minutes 7 days, hours 90 days). Every column is commented in the schema, so an agent can read the database on its own. |
| `series.rs` | Reduces the graph's data to one value per pixel column, so Swift only draws. |
| `prompt.rs` | Builds the Analyze prompt. |
| `scrub.rs` | Removes secrets from process arguments before they're stored or put in a prompt. |
| `quit.rs` | Quits a row: SIGTERM, then SIGKILL for whatever is left, and `docker stop` for its containers. Refuses pids that were recycled. |
| `ffi.rs` | The C ABI (`dl_*` functions). `core/include/devload.h` is generated from it by cbindgen. |

Units inside the core: timestamps in Unix seconds, memory in bytes (in RAM) or MiB (in the database), CPU in per-mille of one core.

## Swift shell (`app/`)

| File | What it does |
|---|---|
| `Devload/main.swift`, `App.swift` | Entry point. Refuses to start a second copy. `--snapshot <dir>` renders PNGs from sample data instead of running. |
| `AppController.swift` | The status item and the popover. The popover's views are built when it opens and released when it closes. While it's closed, the app only redraws the icon when the figure changes. |
| `Core.swift` | Swift wrapper around the C ABI (`LiveCore`). `SampleCore.swift` is the fake core used by tests and snapshots. |
| `PopoverModel.swift`, `PopoverView.swift` | The popover, in SwiftUI. |
| `GraphView.swift` | The graph, drawn with AppKit, plus the hover readout. |
| `IconRenderer.swift` | The menu bar figure and its pressure dot. |
| `Notifier.swift` | Pressure alerts (one per episode). |
| `LoginItem.swift` | Launch at login through `SMAppService.mainApp`. |

## Threads

- **Main thread:** AppKit and SwiftUI.
- **Collector thread:** ticks every 5 s and calls the engine under its mutex. The callback to Swift is hopped to the main queue.
- **Docker thread:** polls the socket when one exists, and hands stacks to the engine.

## Tests

- `core/tests/` and `#[cfg(test)]` modules: Rust unit and integration tests, including a Docker fixture server and a filled store.
- `core/tests/c/abi_smoke.c`: calls the C ABI from C, to catch ABI drift.
- `app/Tests/`: Swift tests, including pixel checks on rendered views.
- `make snapshots`: PNGs from sample data, for visual review.

## Data folder

`~/Library/Application Support/devload/` (or `$DEVLOAD_DATA_DIR`):

- `devload.sqlite`: the database (WAL mode).
- `devload.lock`: held (`flock`) while devload runs, so a second copy can't write to the same folder.
