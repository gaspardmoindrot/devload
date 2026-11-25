//! Graph columns: series already reduced by Rust to the pixel width, so the
//! Swift graph only draws. Pure functions, no database, no heap allocation.
//!
//! Input points come from the RAM ring (5 s ticks, 1 h range) or from minute
//! rows (6 h, 24 h, 7 d). Column `i` of `w` covers the real-valued interval
//! `[from + i*(to-from)/w, from + (i+1)*(to-from)/w)`; a point at `ts` falls in
//! column `floor((ts - from) * w / (to - from))`. Points outside `[from, to)`
//! are ignored.

use crate::model::Gid;

/// One machine sample. A 5 s tick has `lo == hi == used`; a minute row has
/// `lo = used_avg_mib`, `hi = used_max_mib`. `pressure` is a `Pressure` as u8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MPoint {
    pub ts: i64,
    pub lo_mib: u32,
    pub hi_mib: u32,
    pub pressure: u8,
}

/// One group at one sample time (its OWN processes, children not included).
/// A slice of these is sorted by `ts`; its timestamps match the `MPoint`s'.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GPoint {
    pub ts: i64,
    pub gid: Gid,
    pub mib: u32,
}

/// `level` value of a column that has no sample.
pub const NO_DATA: u8 = 255;

/// Column index of `ts`, or None outside `[from, to)`. Caller guarantees `to > from` and `w > 0`.
#[inline]
fn column(ts: i64, from: i64, to: i64, w: usize) -> Option<usize> {
    if ts < from || ts >= to {
        return None;
    }
    // i128: (ts - from) * w cannot overflow whatever the caller passes.
    let i = ((ts - from) as i128 * w as i128 / (to - from) as i128) as usize;
    Some(i.min(w - 1))
}

/// Pressure mode. Width = `lo.len()` (the three slices must have the same
/// length). Per column: `lo` = min of the points' `lo`, `hi` = max of their
/// `hi`, `level` = max pressure. Taking the max per column is what keeps a
/// 2-minute spike visible on a 7-day graph. Empty column: `lo = hi = 0`,
/// `level = NO_DATA`. `from >= to` gives only empty columns.
pub fn pressure_columns(
    points: &[MPoint],
    from: i64,
    to: i64,
    lo: &mut [f32],
    hi: &mut [f32],
    level: &mut [u8],
) {
    let w = lo.len();
    assert!(hi.len() == w && level.len() == w, "pressure_columns: slices of different widths");
    lo.fill(0.0);
    hi.fill(0.0);
    level.fill(NO_DATA);
    if w == 0 || to <= from {
        return;
    }
    for p in points {
        let Some(i) = column(p.ts, from, to, w) else { continue };
        let (l, h) = (p.lo_mib.min(p.hi_mib) as f32, p.hi_mib.max(p.lo_mib) as f32);
        if level[i] == NO_DATA {
            lo[i] = l;
            hi[i] = h;
            level[i] = p.pressure;
        } else {
            lo[i] = lo[i].min(l);
            hi[i] = hi[i].max(h);
            level[i] = level[i].max(p.pressure);
        }
    }
}

/// Capacity of the fixed table that sums MiB per (folded) group over the
/// period. Groups beyond it (never seen in practice: ~25 are active at a time)
/// are left out of the top-3 ranking but still end up in "other".
const MAX_GROUPS: usize = 1024;

/// Open-addressing table Gid -> sum of MiB, on the stack.
struct Sums {
    keys: [Gid; MAX_GROUPS],
    vals: [f64; MAX_GROUPS],
}

const EMPTY: Gid = Gid::MAX;

impl Sums {
    fn new() -> Self {
        Sums { keys: [EMPTY; MAX_GROUPS], vals: [0.0; MAX_GROUPS] }
    }

    #[inline]
    fn add(&mut self, gid: Gid, v: f64) {
        if gid == EMPTY {
            return;
        }
        let mut i = (gid.wrapping_mul(0x9E37_79B1) as usize) & (MAX_GROUPS - 1);
        for _ in 0..MAX_GROUPS {
            if self.keys[i] == gid {
                self.vals[i] += v;
                return;
            }
            if self.keys[i] == EMPTY {
                self.keys[i] = gid;
                self.vals[i] = v;
                return;
            }
            i = (i + 1) & (MAX_GROUPS - 1);
        }
        // Table full: ignored for ranking.
    }

    /// The 3 biggest sums, biggest first; ties broken by the smaller gid.
    fn top3(&self) -> [Option<Gid>; 3] {
        let mut top: [(f64, Gid); 3] = [(f64::NEG_INFINITY, EMPTY); 3];
        for (k, v) in self.keys.iter().zip(self.vals.iter()) {
            if *k == EMPTY || *v <= 0.0 {
                continue;
            }
            let better = |a: (f64, Gid), b: (f64, Gid)| a.0 > b.0 || (a.0 == b.0 && a.1 < b.1);
            let mut cand = (*v, *k);
            for slot in top.iter_mut() {
                if better(cand, *slot) {
                    std::mem::swap(slot, &mut cand);
                }
            }
        }
        top.map(|(_, g)| (g != EMPTY).then_some(g))
    }
}

/// Groups mode. Width = `has_data.len()`; `stack` holds 4 floats per column
/// (`stack.len() == 4 * width`): top1, top2, top3, other, in MiB.
///
/// - Each group is folded into `parent(gid)` when it has one (a docker stack
///   into its worktree) by summing.
/// - The top 3 are the folded groups with `eligible(root)` that have the
///   biggest average MiB over the period (a group absent at a sample counts 0).
/// - In each column the breakdown is taken at the sample with the maximum
///   `hi` of that column (so the stack height equals the pressure graph's
///   `hi`), and `other = hi - (top1 + top2 + top3)`, floored at 0.
/// - When the groups at that sample add up to more than `hi` (footprints
///   overlap), every group is scaled by `hi / sum` first.
/// - Empty column: all four values 0 and `has_data = false`.
///
/// Returns the top 3 ids, biggest first (None when fewer groups had data).
#[allow(clippy::too_many_arguments)]
pub fn group_columns(
    points: &[MPoint],
    gpoints: &[GPoint],
    parent: impl Fn(Gid) -> Option<Gid>,
    eligible: impl Fn(Gid) -> bool,
    from: i64,
    to: i64,
    stack: &mut [f32],
    has_data: &mut [bool],
) -> [Option<Gid>; 3] {
    let w = has_data.len();
    assert!(stack.len() == 4 * w, "group_columns: stack must hold 4 floats per column");
    stack.fill(0.0);
    has_data.fill(false);
    if w == 0 || to <= from {
        return [None; 3];
    }
    let root = |g: Gid| parent(g).unwrap_or(g);

    // 1. Top 3 by average over the period. Every group shares the same
    //    denominator (the number of samples), so ranking by sum is the same.
    let mut sums = Sums::new();
    let lo = gpoints.partition_point(|g| g.ts < from);
    let hi = gpoints.partition_point(|g| g.ts < to);
    let period = &gpoints[lo..hi];
    for g in period {
        let r = root(g.gid);
        if eligible(r) {
            sums.add(r, g.mib as f64);
        }
    }
    let top = sums.top3();

    // 2. Per column: the sample with the biggest hi (first one on ties).
    //    Columns are filled in order, so one pass over the sorted points works;
    //    the column's best (ts, hi) is kept in stack[4i] / stack[4i+3] until step 3.
    let mut best_ts = i64::MIN;
    let mut cur: Option<usize> = None;
    let mut best_hi: u32 = 0;
    let mut gcursor = 0usize; // into `period`, only moves forward
    let finish = |i: usize, ts: i64, h: u32, gcursor: &mut usize, stack: &mut [f32]| {
        // Advance to the group points of `ts` (points and gpoints are both sorted).
        while *gcursor < period.len() && period[*gcursor].ts < ts {
            *gcursor += 1;
        }
        let mut part = [0f32; 3];
        let mut all = 0f32;
        let mut j = *gcursor;
        while j < period.len() && period[j].ts == ts {
            let r = root(period[j].gid);
            all += period[j].mib as f32;
            for k in 0..3 {
                if top[k] == Some(r) {
                    part[k] += period[j].mib as f32;
                }
            }
            j += 1;
        }
        let total = h as f32;
        // Footprints overlap (shared and compressed pages): their sum can be
        // well above the machine's used memory. Each group then gets its
        // share of `used`, so the stack never rises above the line.
        if all > total && all > 0.0 {
            let f = total / all;
            part.iter_mut().for_each(|p| *p *= f);
        }
        let s = &mut stack[4 * i..4 * i + 4];
        s[0] = part[0];
        s[1] = part[1];
        s[2] = part[2];
        s[3] = (total - (part[0] + part[1] + part[2])).max(0.0);
    };
    for p in points {
        let Some(i) = column(p.ts, from, to, w) else { continue };
        let h = p.hi_mib.max(p.lo_mib);
        if cur != Some(i) {
            if let Some(c) = cur {
                finish(c, best_ts, best_hi, &mut gcursor, stack);
            }
            cur = Some(i);
            has_data[i] = true;
            best_ts = p.ts;
            best_hi = h;
        } else if h > best_hi {
            best_ts = p.ts;
            best_hi = h;
        }
    }
    if let Some(c) = cur {
        finish(c, best_ts, best_hi, &mut gcursor, stack);
    }
    top
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp(ts: i64, lo: u32, hi: u32, p: u8) -> MPoint {
        MPoint { ts, lo_mib: lo, hi_mib: hi, pressure: p }
    }
    fn gp(ts: i64, gid: Gid, mib: u32) -> GPoint {
        GPoint { ts, gid, mib }
    }

    #[test]
    fn spike_of_two_minutes_stays_visible_over_7_days() {
        let from = 0i64;
        let to = 7 * 86400;
        let mut pts: Vec<MPoint> = (0..7 * 1440).map(|m| mp(m * 60, 8000, 9000, 0)).collect();
        // 2-minute spike at day 3.
        let at = 3 * 1440 + 17;
        pts[at as usize] = mp(at * 60, 8000, 22000, 2);
        pts[at as usize + 1] = mp((at + 1) * 60, 8100, 21000, 2);
        let (mut lo, mut hi, mut lv) = ([0f32; 340], [0f32; 340], [0u8; 340]);
        pressure_columns(&pts, from, to, &mut lo, &mut hi, &mut lv);
        let col = (at * 60 * 340 / to) as usize;
        assert_eq!(hi[col], 22000.0);
        assert_eq!(lv[col], 2);
        assert_eq!(lo[col], 8000.0);
        assert_eq!(hi.iter().filter(|&&h| h == 22000.0).count(), 1);
        assert!(lv.iter().all(|&l| l != NO_DATA));
        assert_eq!(lv.iter().filter(|&&l| l == 2).count(), 1);
    }

    #[test]
    fn empty_columns_and_out_of_range() {
        let pts = [mp(-5, 1, 1, 1), mp(10, 100, 200, 1), mp(100, 1, 1, 2)];
        let (mut lo, mut hi, mut lv) = ([9f32; 10], [9f32; 10], [0u8; 10]);
        pressure_columns(&pts, 0, 100, &mut lo, &mut hi, &mut lv);
        assert_eq!(lv[1], 1);
        assert_eq!((lo[1], hi[1]), (100.0, 200.0));
        for i in (0..10).filter(|&i| i != 1) {
            assert_eq!((lo[i], hi[i], lv[i]), (0.0, 0.0, NO_DATA));
        }
    }

    #[test]
    fn width_one_and_from_eq_to() {
        let pts = [mp(0, 10, 20, 0), mp(50, 5, 30, 1)];
        let (mut lo, mut hi, mut lv) = ([0f32; 1], [0f32; 1], [0u8; 1]);
        pressure_columns(&pts, 0, 60, &mut lo, &mut hi, &mut lv);
        assert_eq!((lo[0], hi[0], lv[0]), (5.0, 30.0, 1));
        pressure_columns(&pts, 60, 60, &mut lo, &mut hi, &mut lv);
        assert_eq!((lo[0], hi[0], lv[0]), (0.0, 0.0, NO_DATA));
        pressure_columns(&pts, 60, 0, &mut lo, &mut hi, &mut lv);
        assert_eq!(lv[0], NO_DATA);
        // width 0: nothing happens, no panic
        pressure_columns(&pts, 0, 60, &mut [], &mut [], &mut []);

        let gps = [gp(0, 1, 5), gp(50, 1, 7)];
        let (mut st, mut hd) = ([1f32; 4], [false; 1]);
        let top = group_columns(&pts, &gps, |_| None, |_| true, 0, 60, &mut st, &mut hd);
        assert_eq!(top, [Some(1), None, None]);
        assert!(hd[0]);
        // breakdown at the max-hi point (ts 50, hi 30)
        assert_eq!(st, [7.0, 0.0, 0.0, 23.0]);
        let top = group_columns(&pts, &gps, |_| None, |_| true, 60, 60, &mut st, &mut hd);
        assert_eq!(top, [None; 3]);
        assert_eq!((st, hd), ([0.0; 4], [false]));
        assert_eq!(group_columns(&pts, &gps, |_| None, |_| true, 0, 60, &mut [], &mut []), [None; 3]);
    }

    #[test]
    fn parent_folding_and_top3() {
        // gid 1 worktree, 2 its docker stack, 3 and 4 apps, 5 ineligible (System).
        let pts: Vec<MPoint> = (0..4).map(|i| mp(i * 10, 1000, 1000, 0)).collect();
        let mut gps = vec![];
        for i in 0..4 {
            let ts = i * 10;
            gps.push(gp(ts, 1, 100));
            gps.push(gp(ts, 2, 150)); // folded: worktree 1 = 250
            gps.push(gp(ts, 3, 200));
            gps.push(gp(ts, 4, 50));
            gps.push(gp(ts, 5, 400));
            if i == 0 {
                gps.push(gp(ts, 6, 600)); // 600/4 = 150 average: below 3
            }
        }
        let parent = |g: Gid| (g == 2).then_some(1);
        let eligible = |g: Gid| g != 5;
        let mut st = [0f32; 8];
        let mut hd = [false; 2];
        let top = group_columns(&pts, &gps, parent, eligible, 0, 40, &mut st, &mut hd);
        assert_eq!(top, [Some(1), Some(3), Some(6)]);
        assert_eq!(hd, [true, true]);
        // column 0 holds ts 0 and 10, both hi 1000: first one (ts 0) wins.
        // All groups at ts 0 sum to 1500 > hi 1000: each scaled by 2/3.
        let want = [250.0 * 2.0 / 3.0, 200.0 * 2.0 / 3.0, 400.0, 300.0];
        assert!(st[0..4].iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01), "{:?}", &st[0..4]);
        assert_eq!(&st[4..8], &[250.0, 200.0, 0.0, 550.0]);
    }

    #[test]
    fn other_is_never_negative_and_breakdown_follows_max_hi() {
        let pts = [mp(0, 100, 100, 0), mp(10, 300, 500, 0), mp(20, 100, 120, 0)];
        let gps = [gp(0, 1, 90), gp(10, 1, 450), gp(10, 2, 400), gp(20, 1, 110)];
        let mut st = [0f32; 4];
        let mut hd = [false; 1];
        let top = group_columns(&pts, &gps, |_| None, |_| true, 0, 30, &mut st, &mut hd);
        assert_eq!(top, [Some(1), Some(2), None]);
        // 450 + 400 > hi 500: both scaled by 500/850, the stack is exactly hi.
        assert!((st[0] - 450.0 * 500.0 / 850.0).abs() < 0.01, "{st:?}");
        assert!((st[1] - 400.0 * 500.0 / 850.0).abs() < 0.01, "{st:?}");
        assert_eq!(st[3], 0.0);
        assert!((st.iter().sum::<f32>() - 500.0).abs() < 0.01);
        assert!(st.iter().all(|&v| v >= 0.0));
    }

    #[test]
    fn overlapping_footprints_share_used_memory() {
        // Top group 1 and an "other" group 3; together 1.5× used.
        let pts = [mp(0, 1000, 1000, 0)];
        let gps = [gp(0, 1, 900), gp(0, 3, 600)];
        let mut st = [0f32; 4];
        let mut hd = [false; 1];
        group_columns(&pts, &gps, |_| None, |g| g == 1, 0, 10, &mut st, &mut hd);
        assert_eq!(st, [600.0, 0.0, 0.0, 400.0]);
    }

    #[test]
    fn group_columns_empty_columns() {
        let pts = [mp(5, 100, 100, 0), mp(95, 200, 200, 0)];
        let gps = [gp(5, 1, 60), gp(95, 1, 80)];
        let mut st = [9f32; 40];
        let mut hd = [true; 10];
        group_columns(&pts, &gps, |_| None, |_| true, 0, 100, &mut st, &mut hd);
        assert!(hd[0] && hd[9]);
        assert!(hd[1..9].iter().all(|&h| !h));
        assert!(st[4..36].iter().all(|&v| v == 0.0));
        assert_eq!(&st[0..4], &[60.0, 0.0, 0.0, 40.0]);
        assert_eq!(&st[36..40], &[80.0, 0.0, 0.0, 120.0]);
    }
}
