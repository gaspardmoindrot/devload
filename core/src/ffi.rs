//! The C ABI that Devload.app calls. The header `include/devload.h` is
//! generated from this file by cbindgen (`make header`).
//!
//! Threads: one collector thread ticks the engine every 5 s; one docker
//! thread scans the Docker socket every 30 s. Every export and every tick
//! runs inside `catch_unwind`: a panic is logged and skipped, the app keeps
//! running. No lock is held while the Swift callback runs.

use std::ffi::{CStr, CString, c_char, c_void};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::collect::Config;
use crate::docker::{self, Docker};
use crate::engine::{Engine, Mode, Period, SeriesBufs, Snapshot};
use crate::engine::aggregate::{Row, RowKind};
use crate::engine::parts::Parts;
use crate::quit::{self, Timing};

pub const TICK: Duration = Duration::from_secs(5);
pub const DOCKER_EVERY: Duration = Duration::from_secs(30);

/// `dl_start` results.
pub const DL_OK: i32 = 0;
pub const DL_ALREADY_STARTED: i32 = 1;
/// Another devload holds the lock in this data folder.
pub const DL_OTHER_INSTANCE: i32 = 2;
pub const DL_ERROR: i32 = 3;

/// Event kinds.
///
/// A new sample. Sent when the icon's figure (tenths of a GB) or the
/// pressure level changed, and on every tick while the popover is open.
pub const DL_EVENT_TICK: u8 = 1;
/// A pressure episode started: notify. `pressure` is its level.
pub const DL_EVENT_ALERT: u8 = 2;
/// A `dl_quit` ended. `row` is the row id; `quit`, `denied`, `left` count
/// processes and containers.
pub const DL_EVENT_QUIT: u8 = 3;

/// Part kinds.
pub const DL_PART_PROCESS: u8 = 0;
/// An agent runtime: `count` sessions, `idle` of them idle.
pub const DL_PART_AGENT: u8 = 1;
/// A docker stack: `count` containers, `detail` its services.
pub const DL_PART_DOCKER: u8 = 2;

/// Sent to the callback, on the collector thread.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DlEvent {
    pub kind: u8,
    /// 0 Normal, 1 Tight, 2 Critical.
    pub pressure: u8,
    pub used: u64,
    pub total: u64,
    /// DL_EVENT_QUIT: the row id. 0 for other kinds.
    pub row: u32,
    /// DL_EVENT_QUIT: processes and containers now gone.
    pub quit: u32,
    /// DL_EVENT_QUIT: processes we may not signal (another user's).
    pub denied: u32,
    /// DL_EVENT_QUIT: processes and containers still alive at the end.
    pub left: u32,
}

pub type DlCallback = Option<extern "C" fn(ctx: *mut c_void, event: DlEvent)>;

/// One row of a list. Strings live until `dl_snapshot_free`.
#[repr(C)]
pub struct DlRow {
    /// Stable while the app runs: pass it to `dl_parts_get` and `dl_quit`.
    pub id: u32,
    pub name: *const c_char,
    pub detail: *const c_char,
    pub footprint: u64,
    /// Per-mille of one core.
    pub cpu_pm: u32,
    /// 0 dev project, 1 app.
    pub kind: u8,
    /// The .app bundle, or the worktree folder. "" for a docker stack with
    /// no worktree.
    pub path: *const c_char,
}

/// One part of a row: processes sharing a label, an agent's sessions, or a
/// docker stack. Strings live until `dl_parts_free`.
#[repr(C)]
pub struct DlPart {
    /// "node", "Claude sessions", "Linux VM", a stack name.
    pub label: *const c_char,
    /// Docker: its services, "api, db". Else "".
    pub detail: *const c_char,
    /// Processes; agent: sessions; docker: containers.
    pub count: u32,
    /// Agent: idle sessions. Else 0.
    pub idle: u32,
    pub footprint: u64,
    /// DL_PART_PROCESS, DL_PART_AGENT or DL_PART_DOCKER.
    pub kind: u8,
}

/// What a row is made of at the last tick. Free with `dl_parts_free`.
#[repr(C)]
pub struct DlParts {
    /// Biggest first, at most 8.
    pub parts: *const DlPart,
    pub len: u32,
    pub more_count: u32,
    pub more_footprint: u64,
    /// Processes a `dl_quit` would signal.
    pub processes: u32,
    /// Containers a `dl_quit` would stop.
    pub containers: u32,
}

/// The header and both lists. Free with `dl_snapshot_free`.
#[repr(C)]
pub struct DlSnapshot {
    /// False until the first tick is in.
    pub ready: bool,
    pub pressure: u8,
    pub cpu_pct: u32,
    pub ts: i64,
    /// Since when the pressure colour holds (Unix seconds).
    pub since: i64,
    pub used: u64,
    pub total: u64,
    pub dev: *const DlRow,
    pub dev_len: u32,
    pub dev_more_count: u32,
    pub dev_more_footprint: u64,
    pub apps: *const DlRow,
    pub apps_len: u32,
    pub apps_more_count: u32,
    pub apps_more_footprint: u64,
}

/// What a graph fill returns besides the buffers.
#[repr(C)]
#[derive(Debug, Default)]
pub struct DlSeriesMeta {
    pub from: i64,
    pub to: i64,
    /// Top of the y axis (MiB).
    pub total_mib: u32,
    /// Groups mode: how many legend names (0–3).
    pub top_len: u32,
    /// Groups mode: legend names, biggest first, one per line. Free with
    /// `dl_string_free` (null when `top_len` is 0).
    pub top_names: *mut c_char,
}

#[derive(Clone, Copy)]
struct Ctx(*mut c_void);
// The Swift side passes an Unmanaged pointer and hops to the main thread.
unsafe impl Send for Ctx {}

struct Running {
    engine: Arc<Mutex<Engine>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    cb: DlCallback,
    ctx: Ctx,
    threads: Vec<JoinHandle<()>>,
    _lock: File,
}

static STATE: Mutex<Option<Running>> = Mutex::new(None);
/// Rows with a `dl_quit` in progress.
static QUITTING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// A poisoned mutex only means a tick panicked: the data is still usable.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn engine() -> Option<Arc<Mutex<Engine>>> {
    lock(&STATE).as_ref().map(|r| r.engine.clone())
}

fn guard<T>(what: &str, fallback: T, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("devload: panic in {what}, skipped");
            fallback
        }
    }
}

pub fn default_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/devload")
}

/// Takes `devload.lock` in `dir`. None if another process holds it.
fn take_lock(dir: &Path) -> std::io::Result<Option<File>> {
    std::fs::create_dir_all(dir)?;
    let f = File::options().create(true).truncate(false).write(true).open(dir.join("devload.lock"))?;
    // flock locks belong to the open file: released when the app dies.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    Ok((r == 0).then_some(f))
}

/// `dl_start` with a custom tick (tests).
pub fn start(dir: &Path, cb: DlCallback, ctx: *mut c_void, tick: Duration) -> i32 {
    let mut state = lock(&STATE);
    if state.is_some() {
        return DL_ALREADY_STARTED;
    }
    let lock_file = match take_lock(dir) {
        Ok(Some(f)) => f,
        Ok(None) => return DL_OTHER_INSTANCE,
        Err(e) => {
            eprintln!("devload: can't use {}: {e}", dir.display());
            return DL_ERROR;
        }
    };
    let engine = Arc::new(Mutex::new(Engine::new(Config::from_env(), dir, now())));
    let stop = Arc::new((Mutex::new(false), Condvar::new()));
    let ctx = Ctx(ctx);

    let collector = {
        let (engine, stop) = (engine.clone(), stop.clone());
        std::thread::Builder::new()
            .name("devload-collector".into())
            .spawn(move || collector_loop(&engine, &stop, cb, ctx, tick))
    };
    let docker = {
        let (engine, stop) = (engine.clone(), stop.clone());
        std::thread::Builder::new().name("devload-docker".into()).spawn(move || docker_loop(&engine, &stop))
    };
    let threads = match (collector, docker) {
        (Ok(a), Ok(b)) => vec![a, b],
        _ => return DL_ERROR,
    };
    *state = Some(Running { engine, stop, threads, cb, ctx, _lock: lock_file });
    DL_OK
}

/// Waits `d` or until stopped. True when stopped.
fn wait(stop: &(Mutex<bool>, Condvar), d: Duration) -> bool {
    let (m, cv) = stop;
    let g = lock(m);
    let (g, _) = cv.wait_timeout_while(g, d, |s| !*s).unwrap_or_else(|e| e.into_inner());
    *g
}

fn collector_loop(engine: &Mutex<Engine>, stop: &(Mutex<bool>, Condvar), cb: DlCallback, ctx: Ctx, tick: Duration) {
    let mut next = Instant::now();
    // What the menu bar icon shows: tenths of a GB and the pressure level.
    let mut shown: Option<(u64, u8)> = None;
    loop {
        let out = guard("tick", None, || {
            let mut e = lock(engine);
            let out = e.tick(now());
            let s = e.snapshot();
            Some((out, s.used, s.total, s.pressure as u8, e.popover_open()))
        });
        // The engine lock is released here: Swift may call back into us.
        if let (Some((out, used, total, pressure, open)), Some(f)) = (out, cb) {
            // Popover closed, Swift is only woken when the icon changes.
            let icon = (icon_tenths(used), pressure);
            if out.recorded && (open || shown != Some(icon)) {
                shown = Some(icon);
                f(ctx.0, DlEvent { kind: DL_EVENT_TICK, pressure, used, total, ..Default::default() });
            }
            if let Some(level) = out.alert {
                f(ctx.0, DlEvent { kind: DL_EVENT_ALERT, pressure: level as u8, used, total, ..Default::default() });
            }
        }
        next += tick;
        let now_i = Instant::now();
        if next <= now_i {
            // Late (a slow tick, or the Mac slept): no catch-up burst.
            next = now_i + tick;
        }
        if wait(stop, next - now_i) {
            return;
        }
    }
}

/// Used memory in tenths of a GB, rounded like the icon's "19.5 GB".
fn icon_tenths(used: u64) -> u64 {
    (used * 10 + (1 << 29)) >> 30
}

fn docker_socket() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    docker::resolve_socket(|k| std::env::var(k).ok(), &home)
}

fn docker_loop(engine: &Mutex<Engine>, stop: &(Mutex<bool>, Condvar)) {
    let socket = docker_socket();
    let provider = socket.as_deref().map_or(docker::Provider::Unknown, docker::Provider::of_socket);
    let mut d = Docker::new(socket);
    loop {
        // The scan (up to a few hundred ms) runs without the engine lock.
        if let Some(stacks) = guard("docker scan", None, || Some(d.scan())) {
            lock(engine).set_stacks(now(), provider, stacks);
        }
        if wait(stop, DOCKER_EVERY) {
            return;
        }
    }
}

// ---------------------------------------------------------------- exports

/// Starts the core. `data_dir`: UTF-8 path, or null for
/// ~/Library/Application Support/devload. `callback` runs on a background
/// thread with `ctx`. Returns DL_OK, DL_ALREADY_STARTED, DL_OTHER_INSTANCE
/// or DL_ERROR.
///
/// # Safety
/// `data_dir` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_start(data_dir: *const c_char, callback: DlCallback, ctx: *mut c_void) -> i32 {
    let dir = if data_dir.is_null() {
        default_data_dir()
    } else {
        match unsafe { CStr::from_ptr(data_dir) }.to_str() {
            Ok(s) if !s.is_empty() => PathBuf::from(s),
            _ => return DL_ERROR,
        }
    };
    guard("dl_start", DL_ERROR, || start(&dir, callback, ctx, TICK))
}

/// Stops the threads and writes what is in memory. Safe to call twice.
#[unsafe(no_mangle)]
pub extern "C" fn dl_stop() {
    guard("dl_stop", (), || {
        let Some(r) = lock(&STATE).take() else { return };
        *lock(&r.stop.0) = true;
        r.stop.1.notify_all();
        for t in r.threads {
            let _ = t.join();
        }
        lock(&r.engine).stop(now());
    })
}

/// The popover opened or closed (graph data is cached while it is open).
#[unsafe(no_mangle)]
pub extern "C" fn dl_set_popover_open(open: bool) {
    guard("dl_set_popover_open", (), || {
        if let Some(e) = engine() {
            lock(&e).set_popover_open(open);
        }
    })
}

/// Owns everything a `DlSnapshot` points at. `c` comes first so the
/// pointer handed out is also the pointer to free.
#[repr(C)]
struct SnapBox {
    c: DlSnapshot,
    _rows: Vec<DlRow>,
    _strings: Vec<CString>,
}

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

fn snap_box(s: &Snapshot) -> Box<SnapBox> {
    let mut strings = Vec::with_capacity(3 * (s.dev.len() + s.apps.len()));
    let mut rows = Vec::with_capacity(s.dev.len() + s.apps.len());
    for r in s.dev.iter().chain(&s.apps) {
        let (name, detail, path) = (cstring(&r.name), cstring(&r.detail), cstring(&r.path));
        rows.push(row(r, &name, &detail, &path));
        strings.extend([name, detail, path]);
    }
    // CString data is on the heap: moving the CStrings keeps the pointers valid.
    let dev_len = s.dev.len();
    let c = DlSnapshot {
        ready: s.ready,
        pressure: s.pressure as u8,
        cpu_pct: s.cpu_pct,
        ts: s.ts,
        since: s.since,
        used: s.used,
        total: s.total,
        dev: rows.as_ptr(),
        dev_len: dev_len as u32,
        dev_more_count: s.dev_more.count,
        dev_more_footprint: s.dev_more.footprint,
        apps: rows[dev_len..].as_ptr(),
        apps_len: s.apps.len() as u32,
        apps_more_count: s.apps_more.count,
        apps_more_footprint: s.apps_more.footprint,
    };
    Box::new(SnapBox { c, _rows: rows, _strings: strings })
}

fn row(r: &Row, name: &CString, detail: &CString, path: &CString) -> DlRow {
    DlRow {
        id: r.gid,
        name: name.as_ptr(),
        detail: detail.as_ptr(),
        footprint: r.footprint,
        cpu_pm: r.cpu_pm,
        kind: if r.kind == RowKind::App { 1 } else { 0 },
        path: path.as_ptr(),
    }
}

/// The current header and lists, or null before `dl_start`.
#[unsafe(no_mangle)]
pub extern "C" fn dl_snapshot_get() -> *mut DlSnapshot {
    guard("dl_snapshot_get", std::ptr::null_mut(), || {
        let Some(e) = engine() else { return std::ptr::null_mut() };
        let b = snap_box(lock(&e).snapshot());
        Box::into_raw(b) as *mut DlSnapshot
    })
}

/// # Safety
/// `s` is null or a pointer from `dl_snapshot_get`, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_snapshot_free(s: *mut DlSnapshot) {
    if !s.is_null() {
        drop(unsafe { Box::from_raw(s as *mut SnapBox) });
    }
}

/// Owns everything a `DlParts` points at (see `SnapBox`).
#[repr(C)]
struct PartsBox {
    c: DlParts,
    _parts: Vec<DlPart>,
    _strings: Vec<CString>,
}

fn parts_box(p: &Parts) -> Box<PartsBox> {
    let mut strings = Vec::with_capacity(2 * p.parts.len());
    let mut parts = Vec::with_capacity(p.parts.len());
    for x in &p.parts {
        let (label, detail) = (cstring(&x.label), cstring(&x.detail));
        parts.push(DlPart {
            label: label.as_ptr(),
            detail: detail.as_ptr(),
            count: x.count,
            idle: x.idle,
            footprint: x.footprint,
            kind: x.kind as u8,
        });
        strings.extend([label, detail]);
    }
    let c = DlParts {
        parts: parts.as_ptr(),
        len: parts.len() as u32,
        more_count: p.more.count,
        more_footprint: p.more.footprint,
        processes: p.processes,
        containers: p.containers,
    };
    Box::new(PartsBox { c, _parts: parts, _strings: strings })
}

/// What row `row` (a `DlRow.id`) is made of at the last tick. Null before
/// `dl_start` or when no row has this id.
#[unsafe(no_mangle)]
pub extern "C" fn dl_parts_get(row: u32) -> *mut DlParts {
    guard("dl_parts_get", std::ptr::null_mut(), || {
        let Some(e) = engine() else { return std::ptr::null_mut() };
        let Some(p) = lock(&e).parts(row) else { return std::ptr::null_mut() };
        Box::into_raw(parts_box(&p)) as *mut DlParts
    })
}

/// # Safety
/// `p` is null or a pointer from `dl_parts_get`, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_parts_free(p: *mut DlParts) {
    if !p.is_null() {
        drop(unsafe { Box::from_raw(p as *mut PartsBox) });
    }
}

/// Quits row `row` (a `DlRow.id`) as seen at the last tick: SIGTERM its
/// processes and stop its docker containers, SIGKILL what is still alive
/// after 3 s. Returns at once; DL_EVENT_QUIT comes through the callback
/// when done (about 4 s, up to 12 s with containers). False before
/// `dl_start`, when no row has this id, or while a quit of it runs.
#[unsafe(no_mangle)]
pub extern "C" fn dl_quit(row: u32) -> bool {
    guard("dl_quit", false, || {
        let (engine, stop, cb, ctx) = {
            let st = lock(&STATE);
            let Some(r) = st.as_ref() else { return false };
            (r.engine.clone(), r.stop.clone(), r.cb, r.ctx)
        };
        // Taken under the engine lock, signalled without it.
        let Some(targets) = lock(&engine).quit_targets(row) else { return false };
        {
            let mut q = lock(&QUITTING);
            if q.contains(&row) {
                return false;
            }
            q.push(row);
        }
        let spawned = std::thread::Builder::new().name("devload-quit".into()).spawn(move || {
            let ctx = ctx; // the whole Ctx (it is Send), not its raw field
            let out = guard("quit", quit::Outcome::default(), || {
                quit::run(&targets, Timing::default(), |ids| docker_socket().map_or(0, |s| docker::stop_containers(&s, ids)))
            });
            lock(&QUITTING).retain(|&r| r != row);
            // After dl_stop, Swift may have let go of ctx: no event.
            if let (Some(f), false) = (cb, *lock(&stop.0)) {
                f(ctx.0, DlEvent { kind: DL_EVENT_QUIT, row, quit: out.quit, denied: out.denied, left: out.left, ..Default::default() });
            }
        });
        if spawned.is_err() {
            lock(&QUITTING).retain(|&r| r != row);
            return false;
        }
        true
    })
}

/// Fills the graph for `period` (0 1h, 1 6h, 2 24h, 3 7d) and `mode`
/// (0 Pressure, 1 Groups), `width` columns, ending now.
///
/// Pressure mode writes `lo`, `hi` (MiB) and `level` (0–2, 255 = no data),
/// `width` each; Groups mode writes `stack` (4 × width: top1, top2, top3,
/// other, MiB) and `has_data` (width). Buffers of the other mode may be null.
/// Returns false on a bad argument (nothing written).
///
/// # Safety
/// Non-null buffers hold at least the sizes above; `meta` is valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_series_fill(
    period: u8,
    mode: u8,
    width: u32,
    lo: *mut f32,
    hi: *mut f32,
    level: *mut u8,
    stack: *mut f32,
    has_data: *mut bool,
    meta: *mut DlSeriesMeta,
) -> bool {
    let (Some(period), false) = (Period::from_u8(period), meta.is_null()) else { return false };
    let w = width as usize;
    let mode = match mode {
        0 if !lo.is_null() && !hi.is_null() && !level.is_null() => Mode::Pressure,
        1 if !stack.is_null() && !has_data.is_null() => Mode::Groups,
        _ => return false,
    };
    let slice = |p: *mut f32, n: usize| if p.is_null() { &mut [][..] } else { unsafe { std::slice::from_raw_parts_mut(p, n) } };
    let bufs = SeriesBufs {
        lo: slice(lo, w),
        hi: slice(hi, w),
        level: if level.is_null() { &mut [] } else { unsafe { std::slice::from_raw_parts_mut(level, w) } },
        stack: slice(stack, 4 * w),
        has_data: if has_data.is_null() { &mut [] } else { unsafe { std::slice::from_raw_parts_mut(has_data, w) } },
    };
    let out = unsafe { &mut *meta };
    guard("dl_series_fill", false, move || {
        let Some(e) = engine() else { return false };
        let m = lock(&e).series(now(), period, mode, bufs);
        let names: Vec<&str> = m.top.iter().map(|(_, n)| n.as_str()).collect();
        *out = DlSeriesMeta {
            from: m.from,
            to: m.to,
            total_mib: m.total_mib,
            top_len: names.len() as u32,
            top_names: if names.is_empty() {
                std::ptr::null_mut()
            } else {
                cstring(&names.iter().map(|n| n.replace('\n', " ")).collect::<Vec<_>>().join("\n")).into_raw()
            },
        };
        true
    })
}

/// The Analyze prompt for `period` (0–3). Writes everything in memory to
/// the database first. Free with `dl_string_free`; null before `dl_start`.
#[unsafe(no_mangle)]
pub extern "C" fn dl_prompt_get(period: u8) -> *mut c_char {
    guard("dl_prompt_get", std::ptr::null_mut(), || {
        let (Some(e), Some(p)) = (engine(), Period::from_u8(period)) else { return std::ptr::null_mut() };
        let text = lock(&e).prompt(now(), p);
        cstring(&text).into_raw()
    })
}

/// # Safety
/// `s` is null or a string returned by this library, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dl_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

#[cfg(test)]
mod tests {
    use super::icon_tenths;

    #[test]
    fn icon_rounds_to_tenths_of_a_gb() {
        let gb = 1u64 << 30;
        assert_eq!(icon_tenths(0), 0);
        assert_eq!(icon_tenths(19 * gb + gb / 2), 195);
        assert_eq!(icon_tenths(19 * gb + gb * 44 / 100), 194);
        assert_eq!(icon_tenths(19 * gb + gb * 46 / 100), 195);
        assert_eq!(icon_tenths(24 * gb), 240);
    }
}
