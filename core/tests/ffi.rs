//! The C ABI end to end, on this machine: start, events, snapshot, graph,
//! prompt, single instance, stop. One test: the core is a process-wide
//! singleton.

use std::ffi::{CStr, CString, c_void};
use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use devload_core::ffi::*;

static TICKS: AtomicU32 = AtomicU32::new(0);
static LAST: Mutex<Option<(u64, u64)>> = Mutex::new(None);

extern "C" fn on_event(ctx: *mut c_void, ev: DlEvent) {
    assert_eq!(ctx as usize, 0xC0FFEE, "ctx passed through");
    if ev.kind == DL_EVENT_TICK {
        TICKS.fetch_add(1, Ordering::SeqCst);
        *LAST.lock().unwrap() = Some((ev.used, ev.total));
        // Calling back into the core from the callback must not deadlock.
        let s = dl_snapshot_get();
        assert!(!s.is_null());
        unsafe { dl_snapshot_free(s) };
    }
}

fn wait_ticks(n: u32) {
    let t = Instant::now();
    while TICKS.load(Ordering::SeqCst) < n {
        assert!(t.elapsed() < Duration::from_secs(20), "no tick after 20 s");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn s(p: *const std::ffi::c_char) -> String {
    unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string()
}

/// Parts of real rows (read only: never quits one).
fn check_parts() {
    // A tick may land between the snapshot and the parts: retry until both
    // come from the same one.
    for attempt in 0.. {
        let p = dl_snapshot_get();
        let snap = unsafe { &*p };
        let dev = unsafe { std::slice::from_raw_parts(snap.dev, snap.dev_len as usize) };
        let apps = unsafe { std::slice::from_raw_parts(snap.apps, snap.apps_len as usize) };
        let mut consistent = true;
        let mut ids = Vec::new();
        for r in dev.iter().chain(apps) {
            ids.push(r.id);
            let path = s(r.path);
            if r.kind == 1 {
                assert!(path.ends_with(".app"), "{path}");
            }
            let parts = dl_parts_get(r.id);
            if parts.is_null() {
                consistent = false; // the row went away at the last tick
                continue;
            }
            let ps = unsafe { &*parts };
            let list = unsafe { std::slice::from_raw_parts(ps.parts, ps.len as usize) };
            assert!((1..=8).contains(&ps.len));
            assert!(ps.more_count == 0 || ps.len == 8);
            assert!(list.windows(2).all(|w| w[0].footprint >= w[1].footprint), "sorted");
            for x in list {
                assert!(!s(x.label).is_empty() && x.kind <= DL_PART_DOCKER);
                assert!(x.kind == DL_PART_DOCKER || s(x.detail).is_empty());
                assert!(x.idle <= x.count);
            }
            let sum: u64 = list.iter().map(|x| x.footprint).sum::<u64>() + ps.more_footprint;
            consistent &= sum == r.footprint;
            // `processes` may be 0: a row of terminal shells has nothing to quit.
            assert!(list.iter().all(|x| x.count > 0 || x.kind == DL_PART_DOCKER));
            unsafe { dl_parts_free(parts) };
        }
        unsafe { dl_snapshot_free(p) };
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "row ids are unique");
        if consistent {
            break;
        }
        assert!(attempt < 20, "parts never added up to their rows");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(dl_parts_get(u32::MAX).is_null());
    unsafe { dl_parts_free(std::ptr::null_mut()) };
    assert!(!dl_quit(u32::MAX), "no such row");
}

#[test]
fn c_abi_end_to_end() {
    let dir = std::env::temp_dir().join(format!("devload-ffi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // Nothing works before start, without crashing.
    assert!(dl_snapshot_get().is_null());
    assert!(dl_prompt_get(0).is_null());
    assert!(dl_parts_get(1).is_null());
    assert!(!dl_quit(1));
    dl_set_popover_open(true);
    dl_stop();

    // Another process holding the lock: refused.
    std::fs::create_dir_all(&dir).unwrap();
    let other = std::fs::File::options().create(true).truncate(false).write(true).open(dir.join("devload.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(other.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    let cdir = CString::new(dir.to_str().unwrap()).unwrap();
    let ctx = 0xC0FFEE as *mut c_void;
    assert_eq!(unsafe { dl_start(cdir.as_ptr(), Some(on_event), ctx) }, DL_OTHER_INSTANCE);
    drop(other);

    // Bad arguments.
    let empty = CString::new("").unwrap();
    assert_eq!(unsafe { dl_start(empty.as_ptr(), Some(on_event), ctx) }, DL_ERROR);

    // Fast ticks for the test.
    assert_eq!(start(&dir, Some(on_event), ctx, Duration::from_millis(300)), DL_OK);
    assert_eq!(unsafe { dl_start(cdir.as_ptr(), Some(on_event), ctx) }, DL_ALREADY_STARTED);
    // The first tick always reaches Swift (the icon has nothing yet).
    wait_ticks(1);
    // Popover open: every tick does.
    dl_set_popover_open(true);
    wait_ticks(4);

    let (used, total) = LAST.lock().unwrap().unwrap();
    assert!(total > 4 << 30 && used > 0 && used <= total, "used {used} total {total}");

    // Snapshot.
    let p = dl_snapshot_get();
    let snap = unsafe { &*p };
    assert!(snap.ready);
    assert_eq!(snap.total, total);
    assert!(snap.pressure <= 2 && snap.cpu_pct <= 100);
    assert!(snap.apps_len >= 1, "this Mac runs at least one app");
    let apps = unsafe { std::slice::from_raw_parts(snap.apps, snap.apps_len as usize) };
    let dev = unsafe { std::slice::from_raw_parts(snap.dev, snap.dev_len as usize) };
    assert!(apps.iter().all(|r| r.kind == 1) && dev.iter().all(|r| r.kind == 0));
    assert!(apps.windows(2).all(|w| w[0].footprint >= w[1].footprint), "sorted");
    for r in dev.iter().chain(apps) {
        assert!(!s(r.name).is_empty());
        let _ = s(r.detail);
    }
    assert!(snap.apps_len <= 50 && snap.dev_len <= 50);
    unsafe { dl_snapshot_free(p) };
    unsafe { dl_snapshot_free(std::ptr::null_mut()) };

    check_parts();

    // Graph, both modes, all periods.
    dl_set_popover_open(true);
    let w = 200usize;
    for period in 0..4u8 {
        let (mut lo, mut hi, mut lv) = (vec![0f32; w], vec![0f32; w], vec![0u8; w]);
        let mut meta = DlSeriesMeta::default();
        let ok = unsafe {
            dl_series_fill(period, 0, w as u32, lo.as_mut_ptr(), hi.as_mut_ptr(), lv.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), &mut meta)
        };
        assert!(ok);
        assert_eq!(meta.total_mib as u64, total >> 20);
        assert!(lv[w - 1] <= 2, "period {period}: the last column has data");
        assert!(hi[w - 1] >= lo[w - 1] && lo[w - 1] > 0.0);
        assert!(meta.top_names.is_null());

        let (mut st, mut hd) = (vec![0f32; 4 * w], vec![false; w]);
        let ok = unsafe {
            dl_series_fill(period, 1, w as u32, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), st.as_mut_ptr(), hd.as_mut_ptr(), &mut meta)
        };
        assert!(ok && hd[w - 1]);
        // Groups under 50 MiB are left out: a quiet machine (a CI runner) can have none.
        assert!(meta.top_len <= 3);
        assert_eq!(meta.top_names.is_null(), meta.top_len == 0);
        if meta.top_len > 0 {
            let names = s(meta.top_names);
            assert_eq!(names.lines().count() as u32, meta.top_len);
            assert!(!names.contains("System"));
            unsafe { dl_string_free(meta.top_names) };
        }
        let col: f32 = st[4 * (w - 1)..4 * w].iter().sum();
        assert!((col - hi[w - 1]).abs() <= hi[w - 1] * 0.2, "stack {col} ≈ used {}", hi[w - 1]);
    }
    // Wrong arguments: refused, nothing written.
    let mut meta = DlSeriesMeta::default();
    let n = std::ptr::null_mut();
    assert!(!unsafe { dl_series_fill(9, 0, 10, n, n, std::ptr::null_mut(), n, std::ptr::null_mut(), &mut meta) });
    assert!(!unsafe { dl_series_fill(0, 0, 10, n, n, std::ptr::null_mut(), n, std::ptr::null_mut(), &mut meta) });
    assert!(!unsafe { dl_series_fill(0, 1, 10, n, n, std::ptr::null_mut(), n, std::ptr::null_mut(), &mut meta) });
    dl_set_popover_open(false);

    // Prompt: flushes, names the database.
    let pr = dl_prompt_get(1);
    let text = s(pr);
    unsafe { dl_string_free(pr) };
    let db = dir.join("devload.sqlite");
    assert!(text.contains(db.to_str().unwrap()), "{text}");
    assert!(dl_prompt_get(7).is_null());

    // Stop: threads end quickly, the minute in progress is written.
    let t = Instant::now();
    dl_stop();
    assert!(t.elapsed() < Duration::from_secs(2), "stop took {:?}", t.elapsed());
    let ticks = TICKS.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(TICKS.load(Ordering::SeqCst), ticks, "no tick after stop");
    assert!(dl_snapshot_get().is_null());
    dl_stop();

    let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let minutes: i64 = conn.query_row("SELECT count(*) FROM machine_minute", [], |r| r.get(0)).unwrap();
    assert!(minutes >= 1, "minute written at stop");

    // The lock is free again: a restart works.
    assert_eq!(start(&dir, None, std::ptr::null_mut(), Duration::from_millis(300)), DL_OK);
    dl_stop();
    let _ = std::fs::remove_dir_all(&dir);
}
