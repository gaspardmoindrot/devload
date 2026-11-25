//! 5 s ticks → one row per minute (machine, groups, top processes), and the
//! RAM ring that feeds the 1 h graph.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::model::{Gid, GroupMinute, GroupTick, MIB, MachineMinute, MachineTick, Minute, ProcMinute, ProcTick, to_mib};
use crate::series::{GPoint, MPoint};

/// A group is written for a minute only if it reached this size in it.
pub const GROUP_MIN_BYTES: u64 = 50 * MIB;
pub use super::aggregate::TOP_PROCS;

#[derive(Default)]
struct GAcc {
    sum: u64,
    max: u64,
    cpu_sum: u64,
}

struct PAcc {
    name: Arc<str>,
    args: Arc<str>,
    max: u64,
    cpu_sum: u64,
}

/// Accumulates the ticks of the current minute.
#[derive(Default)]
pub struct MinuteAcc {
    start: Option<i64>,
    ticks: u32,
    used_sum: u128,
    used_max: u64,
    total: u64,
    pressure_max: u8,
    avail_min: u8,
    cpu_sum: u64,
    cpu_max: u32,
    groups: HashMap<Gid, GAcc>,
    procs: HashMap<(Gid, i32), PAcc>,
}

impl MinuteAcc {
    /// Adds one tick. Returns the finished previous minute when `m.ts` is in
    /// a new one.
    pub fn push(&mut self, m: &MachineTick, groups: &[GroupTick], procs: &[ProcTick]) -> Option<Minute> {
        let minute = m.ts.div_euclid(60) * 60;
        let done = match self.start {
            Some(s) if s != minute => self.finish(),
            _ => None,
        };
        if self.start != Some(minute) {
            *self = MinuteAcc { start: Some(minute), avail_min: u8::MAX, ..Default::default() };
        }
        self.ticks += 1;
        self.used_sum += m.used as u128;
        self.used_max = self.used_max.max(m.used);
        self.total = m.total;
        self.pressure_max = self.pressure_max.max(m.pressure as u8);
        self.avail_min = self.avail_min.min(m.avail_pct);
        self.cpu_sum += m.cpu_pm as u64;
        self.cpu_max = self.cpu_max.max(m.cpu_pm);
        for g in groups {
            let a = self.groups.entry(g.gid).or_default();
            a.sum += g.footprint;
            a.max = a.max.max(g.footprint);
            a.cpu_sum += g.cpu_pm as u64;
        }
        for p in procs {
            let a = self.procs.entry((p.gid, p.pid)).or_insert_with(|| PAcc { name: p.name.clone(), args: p.args.clone(), max: 0, cpu_sum: 0 });
            a.max = a.max.max(p.footprint);
            a.cpu_sum += p.cpu_pm as u64;
        }
        done
    }

    /// A copy of the minute in progress, as if it ended now (Analyze, quit).
    /// The minute goes on: when it ends, the whole minute replaces the copy.
    pub fn current(&self) -> Option<Minute> {
        self.finish()
    }

    fn finish(&self) -> Option<Minute> {
        let ts = self.start?;
        let n = self.ticks.max(1) as u64;
        let machine = MachineMinute {
            ts,
            used_avg_mib: to_mib((self.used_sum / n as u128) as u64),
            used_max_mib: to_mib(self.used_max),
            total_mib: to_mib(self.total),
            pressure_max: self.pressure_max,
            avail_min_pct: if self.avail_min == u8::MAX { 0 } else { self.avail_min },
            cpu_avg_pm: (self.cpu_sum / n) as u32,
            cpu_max_pm: self.cpu_max,
        };
        let mut groups: Vec<GroupMinute> = self
            .groups
            .iter()
            .filter(|(_, a)| a.max >= GROUP_MIN_BYTES)
            .map(|(&gid, a)| GroupMinute {
                ts,
                gid,
                // Absent ticks count as 0.
                mib_avg: to_mib(a.sum / n),
                mib_max: to_mib(a.max),
                cpu_avg_pm: (a.cpu_sum / n) as u32,
            })
            .collect();
        groups.sort_unstable_by_key(|g| g.gid);

        let mut by_group: HashMap<Gid, Vec<(&i32, &PAcc)>> = HashMap::new();
        for ((gid, pid), a) in &self.procs {
            if self.groups.get(gid).is_some_and(|g| g.max >= GROUP_MIN_BYTES) {
                by_group.entry(*gid).or_default().push((pid, a));
            }
        }
        let mut procs = Vec::new();
        for (gid, mut list) in by_group {
            list.sort_unstable_by(|a, b| b.1.max.cmp(&a.1.max).then(a.0.cmp(b.0)));
            for (rank, (pid, a)) in list.into_iter().take(TOP_PROCS).enumerate() {
                procs.push(ProcMinute {
                    ts,
                    gid,
                    rank: rank as u8 + 1,
                    pid: *pid,
                    name: a.name.clone(),
                    args: a.args.clone(),
                    mib_max: to_mib(a.max),
                    cpu_avg_pm: (a.cpu_sum / n) as u32,
                });
            }
        }
        procs.sort_unstable_by_key(|p| (p.gid, p.rank));
        Some(Minute { machine, groups, procs })
    }
}

/// The last hour of ticks, for the 1 h graph. Group points are sorted by
/// time, like the series functions want.
#[derive(Default)]
pub struct Ring {
    machine: VecDeque<MPoint>,
    groups: VecDeque<GPoint>,
}

pub const RING_SECS: i64 = 3600;

impl Ring {
    pub fn push(&mut self, m: &MachineTick, groups: &[GroupTick]) {
        let used = to_mib(m.used);
        self.machine.push_back(MPoint { ts: m.ts, lo_mib: used, hi_mib: used, pressure: m.pressure as u8 });
        for g in groups {
            // Tiny groups only add noise and points: the same 50 MB floor as minutes.
            if g.footprint >= GROUP_MIN_BYTES {
                self.groups.push_back(GPoint { ts: m.ts, gid: g.gid, mib: to_mib(g.footprint) });
            }
        }
        let cutoff = m.ts - RING_SECS;
        while self.machine.front().is_some_and(|p| p.ts < cutoff) {
            self.machine.pop_front();
        }
        while self.groups.front().is_some_and(|p| p.ts < cutoff) {
            self.groups.pop_front();
        }
    }

    /// Contiguous slices (machine, groups).
    pub fn slices(&mut self) -> (&[MPoint], &[GPoint]) {
        (self.machine.make_contiguous(), self.groups.make_contiguous())
    }

    pub fn len(&self) -> usize {
        self.machine.len()
    }

    pub fn is_empty(&self) -> bool {
        self.machine.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Pressure;

    fn mt(ts: i64, used_mib: u64, p: Pressure) -> MachineTick {
        MachineTick { ts, used: used_mib * MIB, total: 24 * 1024 * MIB, pressure: p, avail_pct: 40, cpu_pm: 500 }
    }
    fn gt(gid: Gid, mib: u64, cpu: u32) -> GroupTick {
        GroupTick { gid, footprint: mib * MIB, cpu_pm: cpu }
    }
    fn pt(gid: Gid, pid: i32, mib: u64) -> ProcTick {
        ProcTick { gid, pid, name: format!("p{pid}").into(), args: "".into(), footprint: mib * MIB, cpu_pm: 10 }
    }

    #[test]
    fn a_minute_of_ticks_becomes_one_row() {
        let mut acc = MinuteAcc::default();
        // 12 ticks in minute 120..180; group 1 always there, group 2 only half
        // the time, group 3 always small.
        for i in 0..12 {
            let ts = 120 + i * 5;
            let mut g = vec![gt(1, 1000 + i as u64, 100), gt(3, 10, 0)];
            if i % 2 == 0 {
                g.push(gt(2, 200, 20));
            }
            let p = [pt(1, 10, 600), pt(1, 11, 300 + i as u64), pt(1, 12, 50), pt(1, 13, 40), pt(2, 20, 200)];
            let p = if i % 2 == 0 { &p[..] } else { &p[..4] };
            let pressure = if i == 7 { Pressure::Critical } else { Pressure::Normal };
            assert!(acc.push(&mt(ts, 10_000 + i as u64 * 10, pressure), &g, p).is_none());
        }
        let m = acc.push(&mt(180, 10_000, Pressure::Normal), &[], &[]).expect("minute 120 is done");
        assert_eq!(m.machine.ts, 120);
        assert_eq!(m.machine.used_avg_mib, 10_055);
        assert_eq!(m.machine.used_max_mib, 10_110);
        assert_eq!(m.machine.pressure_max, Pressure::Critical as u8);
        assert_eq!(m.machine.cpu_avg_pm, 500);
        let g: Vec<(Gid, u32, u32, u32)> = m.groups.iter().map(|g| (g.gid, g.mib_avg, g.mib_max, g.cpu_avg_pm)).collect();
        // Group 1 avg 1005.5 rounds to 1006. Group 2: present 6 of 12 ticks → avg
        // 100. Group 3 under 50 MB: dropped.
        assert_eq!(g, [(1, 1006, 1011, 100), (2, 100, 200, 10)]);
        let p: Vec<(Gid, u8, i32, u32)> = m.procs.iter().map(|p| (p.gid, p.rank, p.pid, p.mib_max)).collect();
        assert_eq!(p, [(1, 1, 10, 600), (1, 2, 11, 311), (1, 3, 12, 50), (2, 1, 20, 200)]);
    }

    #[test]
    fn current_copies_the_minute_in_progress() {
        let mut acc = MinuteAcc::default();
        assert!(acc.current().is_none());
        acc.push(&mt(65, 100, Pressure::Tight), &[gt(1, 60, 0)], &[]);
        let m = acc.current().unwrap();
        assert_eq!((m.machine.ts, m.groups.len()), (60, 1));
        // The minute goes on: the whole minute keeps the first part's peak and groups.
        acc.push(&mt(70, 50, Pressure::Normal), &[], &[]);
        let m = acc.push(&mt(120, 50, Pressure::Normal), &[], &[]).expect("minute 60 is done");
        assert_eq!((m.machine.ts, m.machine.used_max_mib, m.machine.used_avg_mib), (60, 100, 75));
        assert_eq!(m.machine.pressure_max, Pressure::Tight as u8);
        assert_eq!(m.groups.len(), 1);
    }

    #[test]
    fn ring_keeps_one_hour_sorted() {
        let mut r = Ring::default();
        for i in 0..800 {
            let ts = i * 5;
            r.push(&mt(ts, 1000, Pressure::Normal), &[gt(1, 100, 0), gt(2, 10, 0)]);
        }
        let (m, g) = r.slices();
        assert_eq!(m.first().unwrap().ts, 799 * 5 - 3600);
        assert_eq!(m.len(), 721);
        assert_eq!(g.len(), 721, "small group 2 left out");
        assert!(g.windows(2).all(|w| w[0].ts <= w[1].ts));
    }
}
