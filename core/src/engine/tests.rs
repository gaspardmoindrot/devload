use std::sync::Arc;

use super::*;
use crate::collect::Sample;
use crate::collect::classify::{Class, Facts, Owner, ProcIn};
use crate::collect::repo::RepoIndex;
use crate::collect::repo::tests::FakeFs;
use crate::model::MachineTick;

const GIB: u64 = 1024 * MIB;
const TOTAL: u64 = 24 * GIB;
/// A Tuesday, on a minute boundary.
const T0: i64 = 1_790_000_040;

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Dir {
        let p = std::env::temp_dir().join(format!("devload-engine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        Dir(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn engine(dir: &Path) -> Engine {
    let cfg = || Config { home: "/Users/me".into(), claude_home: dir.join("no-claude"), codex_lock_dir: "/nonexistent".into() };
    let mut fs = FakeFs::default();
    for i in 0..10 {
        fs = fs.repo(&format!("/w/p{i}"), &format!("ref: refs/heads/feat-{i}"));
    }
    let mut e = Engine::new(cfg(), dir, T0);
    e.col = Collector::with_repos(cfg(), RepoIndex::with_fs("/Users/me".into(), Box::new(fs)));
    e
}

/// A hand-built scan: `(owner, MiB)` per process, one process each.
fn scan(ts: i64, used_gib: u64, pressure: Pressure, procs: &[(Owner, u64)]) -> Scan {
    let mut s = Scan {
        machine: MachineTick { ts, used: used_gib * GIB, total: TOTAL, pressure, avail_pct: 40, cpu_pm: 2000 },
        pressure_since: T0,
        ncpu: 10,
        ..Scan::default()
    };
    for (i, (owner, mib)) in procs.iter().enumerate() {
        let pid = 100 + i as i32;
        let facts = Facts { name: "node".into(), argv: vec!["node".into()], label: "node".into(), ..Facts::default() };
        s.procs.push(ProcIn { pid, ppid: 1, start_abs: pid as u64, readable: true, facts: Arc::new(facts) });
        s.samples.push(Sample { footprint: mib * MIB, cpu_pm: 10, start_sec: 0 });
        s.classes.push(Class { owner: owner.clone(), runtime: None });
    }
    s
}

fn wt(i: usize) -> Owner {
    Owner::Worktree(format!("/w/p{i}").into())
}
fn app(name: &str) -> Owner {
    Owner::App(format!("/Applications/{name}.app").into())
}

/// 8 worktrees (800, 700 … 100 MiB), 7 apps (705, 605 … 105 MiB), System 3000 MiB.
fn busy() -> Vec<(Owner, u64)> {
    let mut v: Vec<(Owner, u64)> = (0..8).map(|i| (wt(i), 800 - 100 * i as u64)).collect();
    v.extend((0..7).map(|i| (app(&format!("App{i}")), 700 - 100 * i as u64 + 5)));
    v.push((Owner::System, 3000));
    v
}

#[test]
fn snapshot_sends_every_row_sorted() {
    let d = Dir::new("snap");
    let mut e = engine(&d.0);
    assert!(!e.snapshot().ready);
    let out = e.ingest(T0, scan(T0, 12, Pressure::Normal, &busy()));
    assert_eq!(out, TickOut { recorded: true, alert: None });
    let s = e.snapshot();
    assert!(s.ready);
    assert_eq!((s.used, s.total, s.cpu_pct), (12 * GIB, TOTAL, 20));
    let dev: Vec<&str> = s.dev.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(dev, (0..8).map(|i| format!("feat-{i}")).collect::<Vec<_>>());
    assert_eq!(s.dev_more, More::default());
    let apps: Vec<&str> = s.apps.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(apps, (0..7).map(|i| format!("App{i}")).collect::<Vec<_>>());
    assert_eq!(s.apps_more, More::default());
    assert!(!s.dev.iter().chain(&s.apps).any(|r| r.name == "System"), "System has no row");
}

#[test]
fn cut_keeps_the_first_rows_and_sums_the_rest() {
    let d = Dir::new("cut");
    let mut e = engine(&d.0);
    e.ingest(T0, scan(T0, 12, Pressure::Normal, &busy()));
    let (rows, more) = super::cut(e.snapshot().apps.clone(), 5);
    assert_eq!(rows.len(), 5);
    assert_eq!(more, More { count: 2, footprint: (105 + 205) * MIB });
    let (rows, more) = super::cut(e.snapshot().apps.clone(), 50);
    assert_eq!((rows.len(), more), (7, More::default()));
}

#[test]
fn first_tick_after_sleep_is_dropped() {
    let d = Dir::new("wake");
    let mut e = engine(&d.0);
    assert!(e.ingest(T0, scan(T0, 10, Pressure::Normal, &busy())).recorded);
    assert!(e.ingest(T0 + 5, scan(T0 + 5, 10, Pressure::Normal, &busy())).recorded);
    let s = scan(T0 + 3600, 20, Pressure::Normal, &busy());
    assert!(!e.ingest(T0 + 3600, s).recorded, "1 h gap: slept");
    assert_eq!(e.snapshot().used, 10 * GIB, "snapshot untouched by the dropped tick");
    assert!(e.ingest(T0 + 3605, scan(T0 + 3605, 11, Pressure::Normal, &busy())).recorded);
    assert_eq!(e.snapshot().used, 11 * GIB);
    // Clock going backwards (manual change) is treated the same way.
    assert!(!e.ingest(T0 + 100, scan(T0 + 100, 11, Pressure::Normal, &busy())).recorded);
}

/// Ticks every 5 s from `from` to `to` (excluded).
fn run(e: &mut Engine, from: i64, to: i64, used_gib: u64, p: Pressure) -> Vec<TickOut> {
    (from..to).step_by(5).map(|ts| e.ingest(ts, scan(ts, used_gib, p, &busy()))).collect()
}

#[test]
fn minutes_are_written_every_five_minutes() {
    let d = Dir::new("flush");
    let mut e = engine(&d.0);
    run(&mut e, T0, T0 + 295, 10, Pressure::Normal);
    let st = Store::open(&d.0).unwrap();
    assert!(st.machine_minutes(0, i64::MAX).unwrap().is_empty(), "nothing before 5 min");
    run(&mut e, T0 + 295, T0 + 305, 10, Pressure::Normal);
    let m = st.machine_minutes(0, i64::MAX).unwrap();
    assert_eq!(m.len(), 5, "minutes T0..T0+240 finished");
    assert_eq!(m[0].ts, T0);
    assert_eq!(m[0].used_avg_mib, 10 * 1024);
    let g = st.group_minutes(0, i64::MAX).unwrap();
    // 8 worktrees + 7 apps + System, all ≥ 50 MB.
    assert_eq!(g.len(), 5 * 16);
    let names: Vec<String> = st.groups().unwrap().into_iter().map(|g| g.name).collect();
    assert!(names.contains(&"feat-0".to_string()) && names.contains(&"System".to_string()));
    assert!(e.pending.is_empty());

    // Analyze closes the minute in progress and writes it.
    let p = e.prompt(T0 + 310, Period::H1);
    assert_eq!(st.machine_minutes(0, i64::MAX).unwrap().len(), 6);
    assert!(p.contains(&e.db_path().to_string_lossy().to_string()));
    assert!(p.contains("/w/p0"), "worktree folders listed");
    assert!(!p.contains("/Applications/App0.app"), "only worktrees");
}

type Bufs = (Vec<f32>, Vec<f32>, Vec<u8>, Vec<f32>, Vec<bool>);

fn bufs(w: usize) -> Bufs {
    (vec![0.0; w], vec![0.0; w], vec![0; w], vec![0.0; 4 * w], vec![false; w])
}

#[test]
fn graph_1h_from_the_ring_and_6h_from_minutes() {
    let d = Dir::new("series");
    let mut e = engine(&d.0);
    // 30 min at 10 GB, then 10 min at 20 GB, Tight.
    run(&mut e, T0, T0 + 1800, 10, Pressure::Normal);
    run(&mut e, T0 + 1800, T0 + 2400, 20, Pressure::Tight);
    let now = T0 + 2395;
    let w = 60;

    let (mut lo, mut hi, mut lv, mut st, mut hd) = bufs(w);
    let b = SeriesBufs { lo: &mut lo, hi: &mut hi, level: &mut lv, stack: &mut st, has_data: &mut hd };
    let meta = e.series(now, Period::H1, Mode::Pressure, b);
    assert_eq!((meta.to - meta.from, meta.total_mib), (3600, 24 * 1024));
    // 1 h over 60 columns = 1 min each; the first 20 columns predate T0.
    assert!(lv[..19].iter().all(|&l| l == series::NO_DATA));
    assert_eq!(lv[25], Pressure::Normal as u8);
    assert_eq!(lo[25], 10240.0);
    assert_eq!(lv[w - 2], Pressure::Tight as u8);
    assert_eq!(hi[w - 2], 20480.0);

    let (mut lo6, mut hi6, mut lv6, mut st6, mut hd6) = bufs(w);
    let b = SeriesBufs { lo: &mut lo6, hi: &mut hi6, level: &mut lv6, stack: &mut st6, has_data: &mut hd6 };
    let meta = e.series(now, Period::H6, Mode::Pressure, b);
    assert_eq!(meta.to - meta.from, 6 * 3600);
    // 6 min per column: 40 min of data = the last ~7 columns.
    let filled = lv6.iter().filter(|&&l| l != series::NO_DATA).count();
    assert!((6..=8).contains(&filled), "{filled} columns");
    assert_eq!(lv6[w - 1], Pressure::Tight as u8);
    assert_eq!(hi6[w - 1], 20480.0);
    assert_eq!(lo6[w - 7], 10240.0);
    assert!(e.loaded.is_none(), "popover closed: no cache kept");

    // Groups: top 3 by memory, System never in the legend.
    let (mut lo, mut hi, mut lv, mut st, mut hd) = bufs(w);
    let b = SeriesBufs { lo: &mut lo, hi: &mut hi, level: &mut lv, stack: &mut st, has_data: &mut hd };
    let meta = e.series(now, Period::H6, Mode::Groups, b);
    let names: Vec<&str> = meta.top.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(names, ["feat-0", "App0", "feat-1"], "App0 is 705 MiB");
    assert!(hd[w - 1] && !hd[0]);
    // Footprints sum to 3+3.6+... GiB, under used: no scaling. feat-0 is 800 MiB.
    assert_eq!(st[4 * (w - 1)], 800.0);
}

#[test]
fn graph_cache_lives_while_the_popover_is_open() {
    let d = Dir::new("cache");
    let mut e = engine(&d.0);
    run(&mut e, T0, T0 + 600, 10, Pressure::Normal);
    e.set_popover_open(true);
    let w = 30;
    let fill = |e: &mut Engine, now: i64, p: Period| {
        let (mut lo, mut hi, mut lv, mut st, mut hd) = bufs(w);
        let b = SeriesBufs { lo: &mut lo, hi: &mut hi, level: &mut lv, stack: &mut st, has_data: &mut hd };
        e.series(now, p, Mode::Pressure, b);
        lv.iter().filter(|&&l| l != series::NO_DATA).count()
    };
    let n = fill(&mut e, T0 + 600, Period::D7);
    assert!(n >= 1);
    let cached = e.loaded.as_ref().map(|l| l.machine.len()).unwrap();
    assert_eq!(cached, 5, "the 5 minutes written at T0+300");
    // A shorter period reuses the cache (from is later).
    fill(&mut e, T0 + 600, Period::H24);
    assert_eq!(e.loaded.as_ref().unwrap().from, T0 + 601 - 7 * 86400);
    // A flush while open appends to the cache.
    run(&mut e, T0 + 600, T0 + 905, 10, Pressure::Normal);
    assert_eq!(e.loaded.as_ref().unwrap().machine.len(), 15);
    e.set_popover_open(false);
    assert!(e.loaded.is_none());
}

#[test]
fn one_alert_per_episode() {
    let d = Dir::new("alert");
    let mut e = engine(&d.0);
    let outs = run(&mut e, T0, T0 + 60, 10, Pressure::Normal);
    assert!(outs.iter().all(|o| o.alert.is_none()));
    let outs = run(&mut e, T0 + 60, T0 + 120, 20, Pressure::Tight);
    assert_eq!(outs[0].alert, Some(Pressure::Tight));
    assert!(outs[1..].iter().all(|o| o.alert.is_none()));
    let outs = run(&mut e, T0 + 120, T0 + 180, 23, Pressure::Critical);
    assert!(outs.iter().all(|o| o.alert.is_none()), "escalation is the same episode");
    let ep = e.episodes.current().unwrap();
    assert_eq!(ep.level, Pressure::Critical);
    let top = e.reg.get(ep.top_gid.unwrap()).unwrap();
    assert_eq!(top.name, "feat-0", "biggest row: System (3 GB) has no row and is never named");
}

#[test]
fn runs_without_a_database() {
    let d = Dir::new("nodb");
    std::fs::create_dir_all(&d.0).unwrap();
    // A file where the data folder should be: the database can't open.
    let blocked = d.0.join("blocked");
    std::fs::write(&blocked, b"x").unwrap();
    let mut e = engine(&blocked);
    assert!(e.store.is_none());
    run(&mut e, T0, T0 + 900, 10, Pressure::Normal);
    assert!(e.snapshot().ready);
    assert_eq!(e.pending.len(), 14, "minutes kept in memory");
    let w = 36;
    let (mut lo, mut hi, mut lv, mut st, mut hd) = bufs(w);
    let b = SeriesBufs { lo: &mut lo, hi: &mut hi, level: &mut lv, stack: &mut st, has_data: &mut hd };
    e.series(T0 + 900, Period::H6, Mode::Pressure, b);
    assert!(lv[w - 1] != series::NO_DATA, "graph from pending minutes and the ring");
    let p = e.prompt(T0 + 900, Period::H1);
    assert!(p.contains("devload.sqlite"));
}

#[test]
fn pending_is_capped() {
    let d = Dir::new("cap");
    let blocked = d.0.join("blocked");
    std::fs::create_dir_all(&d.0).unwrap();
    std::fs::write(&blocked, b"x").unwrap();
    let mut e = engine(&blocked);
    for i in 0..(PENDING_MAX as i64 + 10) {
        // One tick per minute (gap of 60 s would look like sleep: feed 5 s apart
        // inside each minute by resetting the wall clock).
        e.last_wall = None;
        e.ingest(T0 + i * 60, scan(T0 + i * 60, 10, Pressure::Normal, &[]));
    }
    assert_eq!(e.pending.len(), PENDING_MAX);
    assert_eq!(e.pending[0].machine.ts, T0 + 9 * 60, "oldest dropped first");
}

#[test]
fn analyze_mid_minute_keeps_the_minute_whole() {
    let d = Dir::new("partial");
    let mut e = engine(&d.0);
    run(&mut e, T0, T0 + 300, 10, Pressure::Normal);
    e.set_popover_open(true);
    let w = 30;
    let (mut lo, mut hi, mut lv, mut st, mut hd) = bufs(w);
    let b = SeriesBufs { lo: &mut lo, hi: &mut hi, level: &mut lv, stack: &mut st, has_data: &mut hd };
    e.series(T0 + 300, Period::H6, Mode::Groups, b);
    assert!(e.loaded.is_some());
    // Minute T0+300: a peak, then Analyze, then the rest of the minute calmer.
    run(&mut e, T0 + 300, T0 + 330, 20, Pressure::Tight);
    e.prompt(T0 + 330, Period::H1);
    run(&mut e, T0 + 330, T0 + 665, 10, Pressure::Normal);
    let st = Store::open(&d.0).unwrap();
    let m = st.machine_minutes(T0 + 300, T0 + 360).unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].used_max_mib, 20 * 1024, "the peak before Analyze is kept");
    assert_eq!(m[0].used_avg_mib, 15 * 1024, "the whole minute, both parts");
    assert_eq!(m[0].pressure_max, Pressure::Tight as u8);
    let l = e.loaded.as_ref().unwrap();
    assert!(l.machine.windows(2).all(|p| p[0].ts < p[1].ts), "one graph point per minute");
    let at = |ts: i64| l.groups.iter().filter(|g| g.ts == ts).count();
    assert_eq!(at(T0 + 300), at(T0), "group points of the minute not doubled");
}

#[test]
fn analyze_mid_minute_without_a_database_keeps_one_pending_minute() {
    let d = Dir::new("partial-nodb");
    std::fs::create_dir_all(&d.0).unwrap();
    let blocked = d.0.join("blocked");
    std::fs::write(&blocked, b"x").unwrap();
    let mut e = engine(&blocked);
    run(&mut e, T0, T0 + 30, 20, Pressure::Normal);
    e.prompt(T0 + 30, Period::H1);
    run(&mut e, T0 + 30, T0 + 65, 10, Pressure::Normal);
    assert_eq!(e.pending.len(), 1);
    assert_eq!(e.pending[0].machine.used_max_mib, 20 * 1024);
}

#[test]
fn a_purged_group_seen_again_gets_its_row_back() {
    let d = Dir::new("purge");
    let mut e = engine(&d.0);
    e.ingest(T0, scan(T0, 10, Pressure::Normal, &busy()));
    e.flush_now(T0, true);
    let st = Store::open(&d.0).unwrap();
    let feat0 = st.groups().unwrap().into_iter().find(|g| g.name == "feat-0").unwrap().id;
    // An hour later: first maintenance, nothing to purge yet.
    e.last_wall = None;
    e.ingest(T0 + 3600, scan(T0 + 3600, 10, Pressure::Normal, &[(Owner::System, 3000)]));
    // 91 days later only System runs: the daily purge drops the others.
    let later = T0 + 91 * 86400;
    e.last_wall = None;
    e.ingest(later, scan(later, 10, Pressure::Normal, &[(Owner::System, 3000)]));
    let names: Vec<String> = st.groups().unwrap().into_iter().map(|g| g.name).collect();
    assert!(!names.contains(&"feat-0".to_string()), "purged: {names:?}");
    assert!(e.reg.get(feat0).is_none(), "the engine forgot it too");
    // feat-0 comes back; System never left.
    e.ingest(later + 5, scan(later + 5, 10, Pressure::Normal, &busy()));
    e.flush_now(later + 5, true);
    let ids: Vec<Gid> = st.groups().unwrap().into_iter().map(|g| g.id).collect();
    let rows = st.group_minutes(later, i64::MAX).unwrap();
    assert!(!rows.is_empty());
    for g in &rows {
        assert!(ids.contains(&g.gid), "group_minute of {} has no groups row", g.gid);
    }
    assert!(st.groups().unwrap().iter().any(|g| g.name == "feat-0"));
}

#[test]
fn hourly_maintenance_forgets_cached_repo_roots() {
    let d = Dir::new("repos");
    let fs = FakeFs::default();
    let cfg = || Config { home: "/Users/me".into(), claude_home: d.0.join("no-claude"), codex_lock_dir: "/nonexistent".into() };
    let mut e = Engine::new(cfg(), &d.0, T0);
    e.col = Collector::with_repos(cfg(), RepoIndex::with_fs("/Users/me".into(), Box::new(fs.clone())));
    e.ingest(T0, scan(T0, 10, Pressure::Normal, &[]));
    assert_eq!(e.col.repos().root_of("/w/new/src"), None);
    // `git init` later: picked up after the next hourly maintenance.
    let _ = fs.repo("/w/new", "ref: refs/heads/feat");
    assert_eq!(e.col.repos().root_of("/w/new/src"), None, "cached");
    e.last_wall = None;
    e.ingest(T0 + 3600, scan(T0 + 3600, 10, Pressure::Normal, &[]));
    assert_eq!(e.col.repos().root_of("/w/new/src").as_deref(), Some("/w/new"));
}

#[test]
fn stop_closes_the_open_episode() {
    let d = Dir::new("stop");
    let mut e = engine(&d.0);
    let outs = run(&mut e, T0, T0 + 60, 20, Pressure::Tight);
    assert_eq!(outs[0].alert, Some(Pressure::Tight));
    e.stop(T0 + 60);
    let st = Store::open(&d.0).unwrap();
    let eps = st.episodes().unwrap();
    assert_eq!((eps.len(), eps[0].start, eps[0].end), (1, T0, Some(T0 + 60)));
    drop(e);
    // Relaunched long after, still tight: a new episode, the old one stays closed.
    let later = T0 + 60 + episodes::RESUME_SECS + 60;
    let mut e = engine(&d.0);
    let mut s = scan(later, 20, Pressure::Tight, &busy());
    s.pressure_since = later;
    assert_eq!(e.ingest(later, s).alert, Some(Pressure::Tight));
    e.stop(later + 60);
    let eps = st.episodes().unwrap();
    let bounds: Vec<(i64, Option<i64>)> = eps.iter().map(|e| (e.start, e.end)).collect();
    assert_eq!(bounds, [(T0, Some(T0 + 60)), (later, Some(later + 60))]);
}

#[test]
fn a_relaunch_while_tight_continues_the_episode_without_an_alert() {
    let d = Dir::new("relaunch");
    let mut e = engine(&d.0);
    let outs = run(&mut e, T0, T0 + 60, 23, Pressure::Critical);
    assert_eq!(outs[0].alert, Some(Pressure::Critical));
    e.stop(T0 + 60);
    drop(e);
    // Relaunched 30 s later, still red then orange: no alert, same episode.
    let mut e = engine(&d.0);
    let mut s = scan(T0 + 90, 23, Pressure::Critical, &busy());
    s.pressure_since = T0 + 90;
    assert_eq!(e.ingest(T0 + 90, s).alert, None);
    let mut s = scan(T0 + 95, 20, Pressure::Tight, &busy());
    s.pressure_since = T0 + 95;
    assert_eq!(e.ingest(T0 + 95, s).alert, None);
    e.stop(T0 + 120);
    let eps = Store::open(&d.0).unwrap().episodes().unwrap();
    let rows: Vec<(i64, Option<i64>, Pressure)> = eps.iter().map(|e| (e.start, e.end, e.level)).collect();
    assert_eq!(rows, [(T0, Some(T0 + 120), Pressure::Critical)]);
}

#[test]
fn a_relaunch_after_a_crash_mid_episode_continues_it() {
    let d = Dir::new("crash");
    let mut e = engine(&d.0);
    let outs = run(&mut e, T0, T0 + 60, 20, Pressure::Tight);
    assert_eq!(outs[0].alert, Some(Pressure::Tight));
    // Written while open, then the process dies without stop().
    assert!(e.flush_now(T0 + 60, false));
    drop(e);
    // 4 min later; the store closed the episode at the last write.
    let mut e = engine(&d.0);
    let mut s = scan(T0 + 300, 20, Pressure::Tight, &busy());
    s.pressure_since = T0 + 300;
    assert_eq!(e.ingest(T0 + 300, s).alert, None);
    assert_eq!(e.episodes.current().unwrap().start, T0);
}

#[test]
fn quitting_a_row_targets_its_processes_and_never_system() {
    let d = Dir::new("targets");
    let mut e = engine(&d.0);
    e.ingest(T0, scan(T0, 12, Pressure::Normal, &busy()));
    // busy(): worktrees are pids 100..108, apps 108..115, System 115.
    let app0 = e.snapshot().apps[0].gid;
    let t = e.quit_targets(app0).unwrap();
    assert_eq!(t.procs, [crate::quit::Proc { pid: 108, start_abs: 108 }]);
    assert!(t.containers.is_empty());
    let feat0 = e.snapshot().dev[0].gid;
    assert_eq!(e.quit_targets(feat0).unwrap().procs[0].pid, 100);
    // System has no row, but its id is a real gid: it must not be quittable.
    let system = e.reg.of_owner(&Owner::System, e.col.repos());
    assert_eq!(e.quit_targets(system), None);
    assert_eq!(e.quit_targets(Gid::MAX), None);
}

#[test]
fn a_failed_flush_loses_no_minute_and_no_group() {
    let d = Dir::new("flush-fail");
    let mut e = engine(&d.0);
    // A second connection makes every minute write fail, as a full disk would.
    let db = rusqlite::Connection::open(d.0.join("devload.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER boom BEFORE INSERT ON machine_minute BEGIN SELECT RAISE(ABORT, 'boom'); END;").unwrap();
    run(&mut e, T0, T0 + 65, 10, Pressure::Normal);
    assert!(!e.flush_now(T0 + 65, false));
    assert_eq!(e.pending.len(), 1, "kept for the next flush");
    db.execute_batch("DROP TRIGGER boom;").unwrap();
    assert!(e.flush_now(T0 + 70, false));
    assert!(e.pending.is_empty());
    let st = Store::open(&d.0).unwrap();
    assert_eq!(st.machine_minutes(0, i64::MAX).unwrap().len(), 1);
    let ids: Vec<Gid> = st.groups().unwrap().into_iter().map(|g| g.id).collect();
    let rows = st.group_minutes(0, i64::MAX).unwrap();
    assert!(!rows.is_empty());
    for g in &rows {
        assert!(ids.contains(&g.gid), "group_minute of {} has no groups row", g.gid);
    }
}
