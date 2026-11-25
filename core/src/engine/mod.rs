//! The engine: turns collector scans into groups, rows, minutes and alerts,
//! keeps the 1 h ring, writes to the store, and answers the UI (snapshot,
//! graph series, Analyze prompt).
//!
//! It is single-threaded: the FFI layer wraps it in a mutex. The collector
//! thread calls [`Engine::tick`] every 5 s; the docker thread hands its
//! stacks to [`Engine::set_stacks`].

pub mod aggregate;
pub mod episodes;
pub mod groups;
pub mod minute;
pub mod parts;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aggregate::{ArgsCache, Row, RowKind, aggregate};
use episodes::Episodes;
use groups::Registry;
use minute::{MinuteAcc, Ring};
use parts::{Members, Parts};

use crate::collect::{Collector, Config, Scan};
use crate::docker::{Provider, Stack};
use crate::model::{Gid, GroupKind, MIB, Minute, Pressure};
use crate::prompt::{self, PromptGroup, PromptInput};
use crate::quit::Targets;
use crate::series::{self, GPoint, MPoint};
use crate::store::Store;

/// Rows sent per list. The app shows the first few and unfolds the rest on
/// click; past this cap the remainder is only summed as "+N more".
pub const LIST_ROWS: usize = 50;
/// Seconds between writes to the database.
pub const FLUSH_SECS: i64 = 300;
pub const MAINTENANCE_SECS: i64 = 3600;
/// A wall-clock gap this long between ticks means the Mac slept: the first
/// tick after it measures the whole sleep and is dropped.
pub const WAKE_GAP_SECS: i64 = 15;
/// Docker stacks older than this are stale (the docker thread is stuck).
pub const STACKS_MAX_AGE: i64 = 90;
/// Minutes kept in RAM while the database can't be written (one day).
const PENDING_MAX: usize = 1440;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Period {
    H1 = 0,
    H6 = 1,
    H24 = 2,
    D7 = 3,
}

impl Period {
    pub fn secs(self) -> i64 {
        match self {
            Period::H1 => 3600,
            Period::H6 => 6 * 3600,
            Period::H24 => 86400,
            Period::D7 => 7 * 86400,
        }
    }
    pub fn from_u8(v: u8) -> Option<Self> {
        [Period::H1, Period::H6, Period::H24, Period::D7].get(v as usize).copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    Pressure = 0,
    Groups = 1,
}

/// "+N more" under a list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct More {
    pub count: u32,
    pub footprint: u64,
}

/// What the popover and the icon show.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// False until the first recorded tick.
    pub ready: bool,
    pub ts: i64,
    pub used: u64,
    pub total: u64,
    pub pressure: Pressure,
    pub since: i64,
    /// Whole machine, 0–100.
    pub cpu_pct: u32,
    pub dev: Vec<Row>,
    pub dev_more: More,
    pub apps: Vec<Row>,
    pub apps_more: More,
}

/// What a graph fill returns besides the buffers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeriesMeta {
    pub from: i64,
    pub to: i64,
    /// Top of the y axis: the machine's memory.
    pub total_mib: u32,
    /// Groups mode: the legend, biggest first.
    pub top: Vec<(Gid, String)>,
}

/// Graph output buffers, `w` columns. Pressure mode fills lo/hi/level;
/// Groups mode fills stack (4 per column) and has_data.
pub struct SeriesBufs<'a> {
    pub lo: &'a mut [f32],
    pub hi: &'a mut [f32],
    pub level: &'a mut [u8],
    pub stack: &'a mut [f32],
    pub has_data: &'a mut [bool],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickOut {
    /// False for the first tick after a sleep (dropped).
    pub recorded: bool,
    /// An episode started: notify at this level.
    pub alert: Option<Pressure>,
}

/// Minute rows read from the database for the graph, kept while the popover
/// is open (and dropped when it closes, to give the memory back).
struct Loaded {
    from: i64,
    machine: Vec<MPoint>,
    groups: Vec<GPoint>,
}

pub struct Engine {
    col: Collector,
    reg: Registry,
    args: ArgsCache,
    /// What the rows of the last tick are made of.
    members: Members,
    store: Option<Store>,
    db_path: PathBuf,
    ring: Ring,
    minute: MinuteAcc,
    /// Finished minutes not yet written.
    pending: Vec<Minute>,
    episodes: Episodes,
    stacks: Option<(i64, Vec<Stack>)>,
    /// Who runs the Docker engine the stacks come from.
    docker_provider: Provider,
    last_wall: Option<i64>,
    last_flush: i64,
    last_maintenance: i64,
    snap: Snapshot,
    popover_open: bool,
    loaded: Option<Loaded>,
    /// CPU cost of the last tick (collector + engine), nanoseconds.
    pub last_cost_ns: u64,
}

impl Engine {
    /// Opens the database in `data_dir` (created if needed). If it can't be
    /// opened the engine still runs, without history.
    pub fn new(cfg: Config, data_dir: &Path, now: i64) -> Engine {
        let _ = std::fs::create_dir_all(data_dir);
        let store = match Store::open(data_dir) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("devload: database unavailable: {e}");
                None
            }
        };
        let existing = store.as_ref().and_then(|s| s.groups().ok()).unwrap_or_default();
        let last_episode = store.as_ref().and_then(|s| s.last_episode().ok().flatten());
        let db_path = store.as_ref().map(|s| s.path().to_path_buf()).unwrap_or_else(|| data_dir.join("devload.sqlite"));
        Engine {
            reg: Registry::new(&cfg.home, existing),
            col: Collector::new(cfg),
            args: ArgsCache::default(),
            members: Members::default(),
            store,
            db_path,
            ring: Ring::default(),
            minute: MinuteAcc::default(),
            pending: Vec::new(),
            episodes: Episodes::resuming(last_episode),
            stacks: None,
            docker_provider: Provider::Unknown,
            last_wall: None,
            last_flush: now,
            last_maintenance: now,
            snap: Snapshot::default(),
            popover_open: false,
            loaded: None,
            last_cost_ns: 0,
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snap
    }

    /// What row `gid` was made of at the last tick. None: no such row.
    pub fn parts(&self, gid: Gid) -> Option<Parts> {
        self.members.parts(gid)
    }

    /// What quitting row `gid` would signal and stop. None: no such row.
    /// macOS's own apps (Finder, Dock, loginwindow…) are never quit: ending
    /// loginwindow ends the session.
    pub fn quit_targets(&self, gid: Gid) -> Option<Targets> {
        let row = self.snap.dev.iter().chain(self.snap.apps.iter()).find(|r| r.gid == gid);
        if row.is_some_and(|r| r.kind == RowKind::App && groups::is_system_bundle(&r.path)) {
            return None;
        }
        self.members.targets(gid)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Latest docker scan (None: Docker isn't running), from the engine
    /// `provider` runs.
    pub fn set_stacks(&mut self, now: i64, provider: Provider, stacks: Option<Vec<Stack>>) {
        self.docker_provider = provider;
        self.stacks = stacks.map(|s| (now, s));
    }

    pub fn popover_open(&self) -> bool {
        self.popover_open
    }

    pub fn set_popover_open(&mut self, open: bool) {
        self.popover_open = open;
        if !open {
            self.loaded = None;
        }
    }

    /// One 5 s tick. `now`: wall clock, Unix seconds.
    pub fn tick(&mut self, now: i64) -> TickOut {
        let t0 = crate::sys::thread_cpu_ns();
        let scan = self.col.tick(now);
        let out = self.ingest(now, scan);
        self.last_cost_ns = crate::sys::thread_cpu_ns().saturating_sub(t0);
        out
    }

    /// Everything after the collector: groups, rows, minutes, alerts, writes.
    pub(crate) fn ingest(&mut self, now: i64, scan: Scan) -> TickOut {
        let slept = self.last_wall.is_some_and(|w| now - w > WAKE_GAP_SECS || now < w);
        self.last_wall = Some(now);
        if slept {
            // CPU and idle counters span the whole sleep: drop this sample.
            return TickOut { recorded: false, alert: None };
        }

        let stacks = self.stacks.as_ref().filter(|(ts, _)| now - ts <= STACKS_MAX_AGE).map(|(_, s)| s.as_slice());
        let agg = aggregate(&scan, &mut self.reg, self.col.repos(), self.docker_provider, stacks, &mut self.args, &mut self.members);

        let m = &scan.machine;
        self.ring.push(m, &agg.groups);
        if let Some(done) = self.minute.push(m, &agg.groups, &agg.procs) {
            self.push_pending(done);
        }

        let top_gid = agg.rows.iter().max_by_key(|r| r.footprint).map(|r| r.gid);
        let alert = self.episodes.update(m.pressure, scan.pressure_since, top_gid);

        self.snap = snapshot_of(&scan, agg.rows);

        if now - self.last_flush >= FLUSH_SECS {
            self.flush(now);
        }
        if now - self.last_maintenance >= MAINTENANCE_SECS {
            self.last_maintenance = now;
            self.maintenance(now);
        }
        TickOut { recorded: true, alert }
    }

    /// Hourly upkeep: the database's, then forget cached repo roots (a
    /// folder `git init`ed since is picked up) and the groups it purged.
    fn maintenance(&mut self, now: i64) {
        self.col.repos().clear();
        let purged = match &mut self.store {
            Some(s) => match s.maintenance(now) {
                Ok(rep) => rep.purged_groups,
                Err(e) => {
                    eprintln!("devload: maintenance failed: {e}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        self.reg.purged(&purged);
    }

    /// Queues a minute to write. A minute already queued by a partial flush
    /// (Analyze while the database is unavailable) is replaced.
    fn push_pending(&mut self, m: Minute) {
        if let Some(last) = self.pending.last_mut()
            && last.machine.ts == m.machine.ts
        {
            *last = m;
            return;
        }
        self.pending.push(m);
        if self.pending.len() > PENDING_MAX {
            let extra = self.pending.len() - PENDING_MAX;
            self.pending.drain(..extra);
        }
    }

    /// Writes finished minutes, changed groups and episodes. With `partial`,
    /// a copy of the minute in progress is written too (Analyze, quit); the
    /// whole minute replaces it when it ends.
    pub fn flush_now(&mut self, now: i64, partial: bool) -> bool {
        if partial && let Some(m) = self.minute.current() {
            self.push_pending(m);
        }
        self.flush(now)
    }

    /// The app quits: an episode still open ends now, then everything in
    /// memory is written.
    pub fn stop(&mut self, now: i64) {
        self.episodes.close(now);
        self.flush_now(now, true);
    }

    fn flush(&mut self, now: i64) -> bool {
        self.last_flush = now;
        let Some(store) = &mut self.store else { return false };
        let groups = self.reg.take_dirty();
        let episodes = self.episodes.pending();
        match store.flush(now, &groups, &self.pending, &episodes) {
            Ok(()) => {
                if let Some(l) = &mut self.loaded {
                    // Keep the graph cache in step with what was just written.
                    for m in &self.pending {
                        if m.machine.ts >= l.from {
                            // A minute written in part earlier (Analyze) is replaced.
                            while l.machine.last().is_some_and(|p| p.ts >= m.machine.ts) {
                                l.machine.pop();
                            }
                            while l.groups.last().is_some_and(|p| p.ts >= m.machine.ts) {
                                l.groups.pop();
                            }
                            l.machine.push(minute_point(&m.machine));
                            l.groups.extend(m.groups.iter().map(|g| GPoint { ts: g.ts, gid: g.gid, mib: g.mib_avg }));
                        }
                    }
                }
                self.pending.clear();
                self.episodes.written();
                true
            }
            Err(e) => {
                eprintln!("devload: flush failed: {e}");
                self.reg.mark_dirty(&groups);
                false
            }
        }
    }

    /// Fills the graph buffers for `period` ending at `now`.
    pub fn series(&mut self, now: i64, period: Period, mode: Mode, bufs: SeriesBufs) -> SeriesMeta {
        let to = now + 1;
        let from = to - period.secs();
        let total_mib = (self.snap.total / MIB) as u32;
        let (machine, groups): (Vec<MPoint>, Vec<GPoint>);
        let (mp, gp): (&[MPoint], &[GPoint]) = if period == Period::H1 {
            self.ring.slices()
        } else {
            (machine, groups) = self.minute_points(from);
            (&machine, &groups)
        };
        let mut meta = SeriesMeta { from, to, total_mib, top: Vec::new() };
        match mode {
            Mode::Pressure => series::pressure_columns(mp, from, to, bufs.lo, bufs.hi, bufs.level),
            Mode::Groups => {
                let reg = &self.reg;
                let top = series::group_columns(
                    mp,
                    gp,
                    |g| reg.parent_of(g),
                    |g| reg.get(g).is_some_and(|g| g.kind != GroupKind::System),
                    from,
                    to,
                    bufs.stack,
                    bufs.has_data,
                );
                meta.top = top.iter().flatten().map(|&g| (g, reg.get(g).map(|g| g.name.clone()).unwrap_or_default())).collect();
            }
        }
        meta
    }

    /// Minute points from `from` to now: the database (cached while the
    /// popover is open), minutes not yet written, then the ticks of the
    /// minute in progress.
    fn minute_points(&mut self, from: i64) -> (Vec<MPoint>, Vec<GPoint>) {
        let have = self.loaded.as_ref().is_some_and(|l| l.from <= from);
        if !have && let Some(store) = &self.store {
            let machine = store.machine_minutes(from, i64::MAX).unwrap_or_default().iter().map(minute_point).collect();
            let groups = store
                .group_minutes(from, i64::MAX)
                .unwrap_or_default()
                .iter()
                .map(|g| GPoint { ts: g.ts, gid: g.gid, mib: g.mib_avg })
                .collect();
            self.loaded = Some(Loaded { from, machine, groups });
        }
        let mut machine: Vec<MPoint> = Vec::new();
        let mut groups: Vec<GPoint> = Vec::new();
        if let Some(l) = &self.loaded {
            let i = l.machine.partition_point(|p| p.ts < from);
            machine.extend_from_slice(&l.machine[i..]);
            let j = l.groups.partition_point(|p| p.ts < from);
            groups.extend_from_slice(&l.groups[j..]);
        }
        let last = machine.last().map_or(i64::MIN, |p| p.ts);
        for m in self.pending.iter().filter(|m| m.machine.ts > last) {
            machine.push(minute_point(&m.machine));
            groups.extend(m.groups.iter().map(|g| GPoint { ts: g.ts, gid: g.gid, mib: g.mib_avg }));
        }
        let last = machine.last().map_or(i64::MIN, |p| p.ts + 59);
        let (rm, rg) = self.ring.slices();
        machine.extend(rm.iter().filter(|p| p.ts > last));
        groups.extend(rg.iter().filter(|p| p.ts > last));
        if !self.popover_open {
            self.loaded = None;
        }
        (machine, groups)
    }

    /// The Analyze prompt for `period` ending now. Writes everything first so
    /// the agent finds it in the database.
    pub fn prompt(&mut self, now: i64, period: Period) -> String {
        self.flush_now(now, true);
        let from = now - period.secs();
        let s = &self.snap;
        let mut rows: Vec<&Row> = s.dev.iter().chain(&s.apps).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.footprint));
        let top_groups = rows
            .iter()
            .take(5)
            .map(|r| PromptGroup {
                name: r.name.clone(),
                kind: if r.kind == RowKind::App { "app" } else { "dev project" },
                footprint: r.footprint,
            })
            .collect();
        let input = PromptInput {
            start_ts: from,
            end_ts: now,
            db_path: self.db_path.to_string_lossy().into_owned(),
            total_bytes: s.total,
            used_bytes: s.used,
            pressure: s.pressure,
            since_ts: s.since,
            cpu_pct: s.cpu_pct as f32,
            top_groups,
            worktree_paths: self.worktrees_in(from),
        };
        prompt::build(&input)
    }

    /// Folders of the dev projects that weighed the most since `from`
    /// (docker stacks count for their worktree), biggest first, at most 8.
    fn worktrees_in(&mut self, from: i64) -> Vec<String> {
        let mut sums: HashMap<Gid, u64> = HashMap::new();
        let rows = self.store.as_ref().and_then(|s| s.group_minutes(from, i64::MAX).ok()).unwrap_or_default();
        for g in &rows {
            let root = self.reg.parent_of(g.gid).unwrap_or(g.gid);
            *sums.entry(root).or_default() += g.mib_avg as u64;
        }
        // Projects live right now count too (the last minutes may be tiny).
        for r in &self.snap.dev {
            *sums.entry(r.gid).or_default() += r.footprint / MIB;
        }
        let mut list: Vec<(u64, &str)> = sums
            .iter()
            .filter_map(|(gid, &sum)| {
                let g = self.reg.get(*gid)?;
                (g.kind == GroupKind::Worktree && g.path.starts_with('/')).then_some((sum, g.path.as_str()))
            })
            .collect();
        list.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        list.into_iter().take(8).map(|(_, p)| p.to_string()).collect()
    }
}

fn minute_point(m: &crate::model::MachineMinute) -> MPoint {
    MPoint { ts: m.ts, lo_mib: m.used_avg_mib, hi_mib: m.used_max_mib, pressure: m.pressure_max }
}

fn snapshot_of(scan: &Scan, rows: Vec<Row>) -> Snapshot {
    let m = &scan.machine;
    let (dev, apps): (Vec<Row>, Vec<Row>) = rows.into_iter().partition(|r| r.kind == RowKind::Dev);
    let (dev, dev_more) = cut(dev, LIST_ROWS);
    let (apps, apps_more) = cut(apps, LIST_ROWS);
    let ncpu = scan.ncpu.max(1);
    Snapshot {
        ready: true,
        ts: m.ts,
        used: m.used,
        total: m.total,
        pressure: m.pressure,
        since: scan.pressure_since,
        cpu_pct: (m.cpu_pm / ncpu / 10).min(100),
        dev,
        dev_more,
        apps,
        apps_more,
    }
}

/// The first `n` rows; the rest summed as "+N more".
pub(crate) fn cut(mut rows: Vec<Row>, n: usize) -> (Vec<Row>, More) {
    let rest = if rows.len() > n { rows.split_off(n) } else { Vec::new() };
    let more = More { count: rest.len() as u32, footprint: rest.iter().map(|r| r.footprint).sum() };
    (rows, more)
}

#[cfg(test)]
mod tests;
