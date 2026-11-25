//! Proof harness for the store and the graph series.
//!
//! Simulates `DAYS` days (default 91) of a busy dev machine on a fast clock,
//! through the real `Store::flush` every 5 minutes and `Store::maintenance`
//! every hour, then prints:
//! - the database size (target < 50 MB at 90 days),
//! - the bytes written per hour in steady state, WAL included (target < 2 MB/h),
//! - the time to read 7 days of minute rows and reduce them to 340 columns
//!   (target about 1-2 ms).
//!
//! Volumes follow a busy dev machine: ~25 groups over 50 MiB at a
//! time, top 3 processes each, a process identity changes 2 % of the minutes,
//! ~0.3 new argument strings per minute with lengths drawn from the measured
//! distribution (`examples/arglens.txt`, truncated to 300 chars like the collector).
//!
//! usage: cargo run --release --example store_fill [-- DIR [DAYS]]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use devload_core::model::{Episode, Gid, Group, GroupKind, GroupMinute, MachineMinute, Minute, Pressure, ProcMinute};
use devload_core::series::{GPoint, MPoint, group_columns, pressure_columns};
use devload_core::store::Store;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn normal(&mut self) -> f64 {
        let (u, v) = (self.f().max(1e-12), self.f());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
    fn lognormal(&mut self, median: f64, sigma: f64) -> f64 {
        median * (sigma * self.normal()).exp()
    }
    fn text(&mut self, n: usize) -> String {
        const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789/-_.= ";
        (0..n).map(|_| A[self.below(A.len())] as char).collect()
    }
}

/// (pid, name, args, share of the group)
type ProcId = (i32, Arc<str>, Arc<str>, f64);

struct Sim {
    rng: Rng,
    groups: Vec<Group>,
    /// Typical size of each group, MiB.
    size: Vec<f64>,
    active: Vec<Gid>,
    names: Vec<Arc<str>>,
    arglens: Vec<usize>,
    /// Per group: its few argument strings.
    group_args: HashMap<Gid, Vec<Arc<str>>>,
    /// (gid, rank) -> (pid, name, args, size factor)
    procs: HashMap<(Gid, u8), ProcId>,
    next_pid: i32,
    episode: Option<Episode>,
}

impl Sim {
    fn new() -> Sim {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let arglens: Vec<usize> = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/arglens.txt"))
            .map(|s| s.lines().filter_map(|l| l.trim().parse::<usize>().ok()).map(|n| n.min(300)).collect())
            .unwrap_or_else(|_| vec![250]);
        let names = (0..250).map(|_| { let n = 6 + rng.below(14); Arc::from(rng.text(n)) }).collect();
        let mut sim = Sim {
            rng,
            groups: vec![],
            size: vec![],
            active: vec![],
            names,
            arglens,
            group_args: HashMap::new(),
            procs: HashMap::new(),
            next_pid: 400,
            episode: None,
        };
        sim.new_group(GroupKind::System);
        for _ in 0..40 {
            sim.new_group_random();
        }
        sim.active = (1..=25).collect();
        sim
    }

    fn new_group(&mut self, kind: GroupKind) -> Gid {
        let id = self.groups.len() as Gid + 1;
        let parent = (kind == GroupKind::Docker)
            .then(|| self.groups.iter().filter(|g| g.kind == GroupKind::Worktree).map(|g| g.id).next_back())
            .flatten();
        let name = self.rng.text(12);
        self.groups.push(Group { id, kind, name: name.clone(), path: format!("/Users/x/git/{name}/{}", self.rng.text(20)), parent });
        let size = if kind == GroupKind::System { 3000.0 } else { 60.0 + self.rng.lognormal(250.0, 1.0) };
        self.size.push(size);
        id
    }

    fn new_group_random(&mut self) -> Gid {
        let r = self.rng.f();
        let kind = if r < 0.5 {
            GroupKind::Worktree
        } else if r < 0.8 {
            GroupKind::App
        } else {
            GroupKind::Docker
        };
        self.new_group(kind)
    }

    fn args_for(&mut self, gid: Gid) -> Arc<str> {
        let fresh = self.rng.f() < 0.2 || !self.group_args.contains_key(&gid);
        if fresh {
            let len = self.arglens[self.rng.below(self.arglens.len())];
            let s: Arc<str> = Arc::from(self.rng.text(len));
            self.group_args.entry(gid).or_default().push(s.clone());
            s
        } else {
            let v = &self.group_args[&gid];
            v[self.rng.below(v.len())].clone()
        }
    }

    /// Rotates the active set a little (every 30 min): ~120 new groups a month.
    fn churn(&mut self) {
        for _ in 0..2 {
            let i = 1 + self.rng.below(self.active.len() - 1); // never drop System
            let g = if self.rng.f() < 0.12 { self.new_group_random() } else { 1 + self.rng.below(self.groups.len()) as Gid };
            if !self.active.contains(&g) {
                self.active[i] = g;
            }
        }
    }

    fn minute(&mut self, ts: i64) -> Minute {
        let mut groups = vec![];
        let mut procs = vec![];
        let mut used = 6000.0;
        let active = self.active.clone();
        for gid in active {
            let base = self.size[gid as usize - 1];
            let avg = (base * (0.85 + 0.3 * self.rng.f())).max(50.0);
            let max = avg * (1.0 + 0.3 * self.rng.f());
            used += avg;
            let cpu = (self.rng.lognormal(20.0, 1.5)) as u32;
            groups.push(GroupMinute { ts, gid, mib_avg: avg as u32, mib_max: max as u32, cpu_avg_pm: cpu });
            for rank in 1..=3u8 {
                let key = (gid, rank);
                if !self.procs.contains_key(&key) || self.rng.f() < 0.02 {
                    self.next_pid = if self.next_pid > 99_000 { 400 } else { self.next_pid + 1 };
                    let name = self.names[self.rng.below(self.names.len())].clone();
                    let args = self.args_for(gid);
                    let share = [0.5, 0.25, 0.1][rank as usize - 1] * (0.8 + 0.4 * self.rng.f());
                    self.procs.insert(key, (self.next_pid, name, args, share));
                }
                let (pid, name, args, share) = self.procs[&key].clone();
                procs.push(ProcMinute {
                    ts,
                    gid,
                    rank,
                    pid,
                    name,
                    args,
                    mib_max: (max * share) as u32,
                    cpu_avg_pm: cpu / (rank as u32 + 1),
                });
            }
        }
        let pressure = match &self.episode {
            Some(e) => e.level as u8,
            None => 0,
        };
        let used = used.min(24_000.0);
        Minute {
            machine: MachineMinute {
                ts,
                used_avg_mib: used as u32,
                used_max_mib: (used * (1.0 + 0.05 * self.rng.f())) as u32,
                total_mib: 24_576,
                pressure_max: pressure,
                avail_min_pct: if pressure > 0 { 18 } else { 35 + self.rng.below(20) as u8 },
                cpu_avg_pm: 800 + self.rng.below(2000) as u32,
                cpu_max_pm: 3000 + self.rng.below(6000) as u32,
            },
            groups,
            procs,
        }
    }

    /// ~3 episodes a day of about 10 minutes. Returns the episode rows to (re)send.
    fn episodes(&mut self, ts: i64) -> Vec<Episode> {
        match self.episode {
            Some(e) if ts - e.start >= 600 => {
                self.episode = None;
                vec![Episode { end: Some(ts), ..e }]
            }
            None if self.rng.f() < 3.0 / 1440.0 => {
                let e = Episode {
                    start: ts,
                    end: None,
                    level: if self.rng.f() < 0.2 { Pressure::Critical } else { Pressure::Tight },
                    top_gid: Some(self.active[1 + self.rng.below(self.active.len() - 1)]),
                };
                self.episode = Some(e);
                vec![e]
            }
            _ => vec![],
        }
    }
}

/// (frames, distinct pages) in a WAL file.
fn wal_stats(path: &Path) -> (u64, u64) {
    let Ok(data) = std::fs::read(path) else { return (0, 0) };
    if data.len() < 32 {
        return (0, 0);
    }
    let frame = 4096 + 24;
    let n = (data.len() - 32) / frame;
    let mut pages = HashSet::new();
    for i in 0..n {
        let o = 32 + i * frame;
        pages.insert(u32::from_be_bytes(data[o..o + 4].try_into().unwrap()));
    }
    (n as u64, pages.len() as u64)
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = args.get(1).map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("devload-store-fill"));
    let days: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(91);
    let _ = std::fs::remove_dir_all(&dir);
    let mut store = Store::open(&dir).expect("open");
    let wal = PathBuf::from(format!("{}-wal", store.path().display()));

    let end = 1_795_600_800i64; // a whole hour
    let start = end - days * 86_400;
    let mut sim = Sim::new();
    let mut pending: Vec<Minute> = Vec::with_capacity(5);
    let mut pending_eps: Vec<Episode> = vec![];
    let mut flush_ms = vec![];
    let mut maint_ms = vec![];
    // steady state = the last 24 simulated hours
    let steady_from = end - 86_400;
    let (mut wal_bytes, mut ckpt_bytes, mut steady_hours) = (0u64, 0u64, 0u64);
    let mut vacuums = 0;
    let t_all = Instant::now();

    let mut ts = start;
    while ts < end {
        if (ts - start) % 1800 == 0 {
            sim.churn();
        }
        pending_eps.extend(sim.episodes(ts));
        pending.push(sim.minute(ts));
        ts += 60;
        if ts % 300 == 0 {
            let t = Instant::now();
            let groups: Vec<Group> = sim.active.iter().map(|g| sim.groups[*g as usize - 1].clone()).collect();
            store.flush(ts, &groups, &pending, &pending_eps).expect("flush");
            flush_ms.push(t.elapsed().as_secs_f64() * 1e3);
            pending.clear();
            pending_eps.clear();
        }
        if ts % 3600 == 0 {
            let t = Instant::now();
            let (flush_frames, _) = wal_stats(&wal);
            let rep = store.maintenance_before_checkpoint(ts).expect("maintenance");
            let (frames, pages) = wal_stats(&wal);
            store.checkpoint().expect("checkpoint");
            maint_ms.push(t.elapsed().as_secs_f64() * 1e3);
            vacuums += rep.vacuumed as u32;
            if ts > steady_from {
                if std::env::var_os("VERBOSE").is_some() {
                    println!("  hour {}: flush frames {flush_frames}, maintenance frames {}, distinct pages {pages}, {:.0} ms, {rep:?}", (ts - steady_from) / 3600, frames - flush_frames, maint_ms.last().unwrap());
                }
                wal_bytes += 32 + frames * (4096 + 24);
                ckpt_bytes += pages * 4096;
                steady_hours += 1;
            }
        }
    }
    let sim_s = t_all.elapsed().as_secs_f64();

    let size = std::fs::metadata(store.path()).map(|m| m.len()).unwrap_or(0);
    let wal_size = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    println!("simulated {days} days in {sim_s:.1} s ({} groups created, {} strings in RAM)", sim.groups.len(), store.string_count());
    println!("database: {:.1} MB ({:.1} MiB), WAL after checkpoint {wal_size} B, incremental vacuums run: {vacuums}", size as f64 / 1e6, size as f64 / (1 << 20) as f64);
    let (w, c) = (wal_bytes as f64 / steady_hours as f64, ckpt_bytes as f64 / steady_hours as f64);
    println!(
        "writes, steady state over the last {steady_hours} h: WAL {:.2} MB/h + checkpoint into the db {:.2} MB/h = {:.2} MB/h",
        w / 1e6,
        c / 1e6,
        (w + c) / 1e6
    );
    println!("flush (5 min of rows): median {:.2} ms; maintenance: median {:.1} ms, max {:.1} ms", median(&mut flush_ms), median(&mut maint_ms), maint_ms.iter().cloned().fold(0.0, f64::max));

    // Graph: 7 days -> 340 columns, Pressure and Groups modes.
    let (from, to) = (end - 7 * 86_400, end);
    let parents: HashMap<Gid, Gid> = store.groups().unwrap().iter().filter_map(|g| g.parent.map(|p| (g.id, p))).collect();
    let system: Gid = 1;
    let (mut lo, mut hi, mut lv) = ([0f32; 340], [0f32; 340], [0u8; 340]);
    let (mut stack, mut has) = ([0f32; 4 * 340], [false; 340]);
    let (mut t_mread, mut t_gread, mut t_press, mut t_groups, mut t_total) = (vec![], vec![], vec![], vec![], vec![]);
    let mut top = [None; 3];
    let mut n_rows = (0, 0);
    for _ in 0..30 {
        let t0 = Instant::now();
        let mm = store.machine_minutes(from, to).unwrap();
        let t1 = Instant::now();
        let gm = store.group_minutes(from, to).unwrap();
        let t2 = Instant::now();
        let mp: Vec<MPoint> = mm.iter().map(|m| MPoint { ts: m.ts, lo_mib: m.used_avg_mib, hi_mib: m.used_max_mib, pressure: m.pressure_max }).collect();
        pressure_columns(&mp, from, to, &mut lo, &mut hi, &mut lv);
        let t3 = Instant::now();
        let gp: Vec<GPoint> = gm.iter().map(|g| GPoint { ts: g.ts, gid: g.gid, mib: g.mib_max }).collect();
        top = group_columns(&mp, &gp, |g| parents.get(&g).copied(), |g| g != system, from, to, &mut stack, &mut has);
        let t4 = Instant::now();
        n_rows = (mm.len(), gm.len());
        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
        t_mread.push(ms(t0, t1));
        t_gread.push(ms(t1, t2));
        t_press.push(ms(t2, t3));
        t_groups.push(ms(t3, t4));
        t_total.push(ms(t0, t4));
    }
    println!("graph 7 d -> 340 columns ({} machine rows, {} group rows), medians of 30 runs:", n_rows.0, n_rows.1);
    println!("  read machine_minute {:.2} ms + pressure_columns {:.3} ms  => Pressure mode {:.2} ms", median(&mut t_mread), median(&mut t_press), median(&mut t_mread) + median(&mut t_press));
    println!("  read group_minute {:.2} ms + group_columns {:.3} ms", median(&mut t_gread), median(&mut t_groups));
    println!("  everything (both modes) {:.2} ms; top 3 = {top:?}", median(&mut t_total));
    let empty = lv.iter().filter(|&&l| l == 255).count();
    println!("  columns without data: {empty}, red columns: {}", lv.iter().filter(|&&l| l == 2).count());

    // Pure reduction micro-bench (no DB): 7 d of minutes, and 1 h of 5 s ticks.
    let mm = store.machine_minutes(from, to).unwrap();
    let gm = store.group_minutes(from, to).unwrap();
    let mp: Vec<MPoint> = mm.iter().map(|m| MPoint { ts: m.ts, lo_mib: m.used_avg_mib, hi_mib: m.used_max_mib, pressure: m.pressure_max }).collect();
    let gp: Vec<GPoint> = gm.iter().map(|g| GPoint { ts: g.ts, gid: g.gid, mib: g.mib_max }).collect();
    let n = 200;
    let t = Instant::now();
    for _ in 0..n {
        pressure_columns(std::hint::black_box(&mp), from, to, &mut lo, &mut hi, &mut lv);
    }
    let p = t.elapsed().as_secs_f64() * 1e6 / n as f64;
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(group_columns(std::hint::black_box(&mp), &gp, |g| parents.get(&g).copied(), |g| g != system, from, to, &mut stack, &mut has));
    }
    let g = t.elapsed().as_secs_f64() * 1e6 / n as f64;
    println!("series only, 7 d of minutes: pressure_columns {p:.0} us, group_columns {g:.0} us");
    let tick_from = end - 3600;
    let mp1: Vec<MPoint> = (0..720).map(|i| MPoint { ts: tick_from + i * 5, lo_mib: 15000 + i as u32, hi_mib: 15000 + i as u32, pressure: 0 }).collect();
    let gp1: Vec<GPoint> = (0..720 * 25).map(|i| GPoint { ts: tick_from + (i / 25) as i64 * 5, gid: (i % 25) as Gid + 1, mib: 300 }).collect();
    let t = Instant::now();
    for _ in 0..n {
        pressure_columns(std::hint::black_box(&mp1), tick_from, end, &mut lo, &mut hi, &mut lv);
        std::hint::black_box(group_columns(&mp1, &gp1, |g| parents.get(&g).copied(), |g| g != system, tick_from, end, &mut stack, &mut has));
    }
    println!("series only, 1 h of 5 s ticks (720 + 18000 points), both modes: {:.0} us", t.elapsed().as_secs_f64() * 1e6 / n as f64);
    println!("db left at {}", store.path().display());
}
