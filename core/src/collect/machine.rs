//! Machine-level metrics: memory used, CPU, and the displayed pressure colour.

use crate::model::Pressure;
use crate::sys;

/// Kernel `memorystatus_vm_pressure_level` values.
const KERNEL_WARN: i32 = 2;
const KERNEL_CRITICAL: i32 = 4;

/// Below this % of available memory for two ticks in a row, the colour turns
/// orange before the kernel says "warn" (which comes late). Calibrated during
/// the 24 h soak.
pub const TIGHT_AVAIL_PCT: i32 = 25;
/// A colour only steps down after this long at a lower level.
pub const STEP_DOWN_SECS: i64 = 60;

/// Turns raw kernel signals into the colour shown in the menu bar, with
/// hysteresis so it doesn't flicker.
#[derive(Debug, Default)]
pub struct PressureState {
    shown: Pressure,
    since: i64,
    low_avail_ticks: u32,
    /// While the raw level stays below `shown`: when that started and the
    /// highest raw level seen since.
    below: Option<(i64, Pressure)>,
    started: bool,
}

impl PressureState {
    pub fn shown(&self) -> Pressure {
        self.shown
    }

    /// Start of the current colour, Unix seconds.
    pub fn since(&self) -> i64 {
        self.since
    }

    /// Feeds one tick. Returns the colour to display.
    pub fn update(&mut self, now: i64, kernel_level: Option<i32>, avail_pct: Option<i32>) -> Pressure {
        let low = avail_pct.is_some_and(|p| p < TIGHT_AVAIL_PCT);
        self.low_avail_ticks = if low { self.low_avail_ticks + 1 } else { 0 };
        let raw = match kernel_level {
            Some(KERNEL_CRITICAL) => Pressure::Critical,
            Some(KERNEL_WARN) => Pressure::Tight,
            _ if self.low_avail_ticks >= 2 => Pressure::Tight,
            _ => Pressure::Normal,
        };
        if !self.started {
            self.started = true;
            self.shown = raw;
            self.since = now;
            return raw;
        }
        if raw >= self.shown {
            if raw > self.shown {
                self.shown = raw;
                self.since = now;
            }
            self.below = None;
            return self.shown;
        }
        let (start, max) = match self.below {
            Some((start, max)) => (start, max.max(raw)),
            None => (now, raw),
        };
        if now - start >= STEP_DOWN_SECS {
            self.shown = max;
            self.since = now;
            // Still below the new colour? Keep counting from now.
            self.below = (raw < max).then_some((now, raw));
        } else {
            self.below = Some((start, max));
        }
        self.shown
    }
}

/// Whole-machine CPU from `HOST_CPU_LOAD_INFO` tick deltas.
#[derive(Debug, Default)]
pub struct CpuMeter {
    prev: Option<[u64; 4]>,
}

impl CpuMeter {
    /// Busy fraction of the whole machine since the previous call, 0.0–1.0.
    pub fn sample(&mut self, ticks: [u64; 4]) -> f64 {
        let busy_frac = match self.prev {
            Some(p) => busy_fraction(p, ticks),
            None => 0.0,
        };
        self.prev = Some(ticks);
        busy_frac
    }
}

/// ticks = [user, system, idle, nice].
pub fn busy_fraction(prev: [u64; 4], now: [u64; 4]) -> f64 {
    let d = |i: usize| now[i].saturating_sub(prev[i]);
    let busy = d(0) + d(1) + d(3);
    let total = busy + d(2);
    if total == 0 { 0.0 } else { busy as f64 / total as f64 }
}

/// Number of logical CPUs (for per-mille of one core ↔ % of the machine).
pub fn logical_cpus() -> u32 {
    sys::sysctl_i32(c"hw.logicalcpu").filter(|&n| n > 0).unwrap_or(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use Pressure::*;

    #[test]
    fn kernel_levels_map_to_colours() {
        let mut p = PressureState::default();
        assert_eq!(p.update(0, Some(1), Some(60)), Normal);
        assert_eq!(p.update(5, Some(4), Some(60)), Critical);
        assert_eq!(p.since(), 5);
    }

    #[test]
    fn low_available_memory_turns_orange_on_the_second_tick() {
        let mut p = PressureState::default();
        assert_eq!(p.update(0, Some(1), Some(20)), Normal);
        assert_eq!(p.update(5, Some(1), Some(20)), Tight);
        assert_eq!(p.since(), 5);
    }

    #[test]
    fn one_low_tick_then_recovery_stays_green() {
        let mut p = PressureState::default();
        p.update(0, Some(1), Some(50));
        p.update(5, Some(1), Some(20));
        assert_eq!(p.update(10, Some(1), Some(30)), Normal);
        assert_eq!(p.update(15, Some(1), Some(20)), Normal);
    }

    #[test]
    fn steps_down_only_after_60_s() {
        let mut p = PressureState::default();
        p.update(0, Some(4), Some(10));
        for t in (5..60).step_by(5) {
            assert_eq!(p.update(t, Some(1), Some(60)), Critical, "t={t}");
        }
        assert_eq!(p.update(60, Some(1), Some(60)), Critical);
        assert_eq!(p.update(65, Some(1), Some(60)), Normal);
        assert_eq!(p.since(), 65);
    }

    #[test]
    fn steps_down_to_the_highest_level_seen_meanwhile() {
        let mut p = PressureState::default();
        p.update(0, Some(4), None);
        p.update(5, Some(2), None); // below starts at 5, max Tight
        for t in (10..=65).step_by(5) {
            p.update(t, Some(1), None);
        }
        assert_eq!(p.shown(), Tight);
        // ...and then to green 60 s later.
        let mut t = 65;
        while p.shown() != Normal {
            t += 5;
            p.update(t, Some(1), None);
            assert!(t < 200);
        }
        assert!(t >= 125, "{t}");
    }

    #[test]
    fn a_spike_during_step_down_resets_the_wait() {
        let mut p = PressureState::default();
        p.update(0, Some(2), None);
        p.update(30, Some(1), None);
        p.update(50, Some(2), None); // back to Tight: wait restarts
        assert_eq!(p.update(95, Some(1), None), Tight);
        assert_eq!(p.since(), 0, "colour never changed");
    }

    #[test]
    fn cpu_busy_fraction() {
        assert_eq!(busy_fraction([0; 4], [0; 4]), 0.0);
        let f = busy_fraction([100, 50, 800, 0], [130, 60, 850, 10]);
        assert!((f - 0.5).abs() < 1e-9, "{f}");
        let mut m = CpuMeter::default();
        assert_eq!(m.sample([1, 1, 1, 1]), 0.0);
        assert!(m.sample([2, 1, 2, 1]) > 0.0);
    }

    #[test]
    fn live_machine_reads() {
        assert!(logical_cpus() >= 1);
        let v = sys::vm_stats().unwrap();
        let total = sys::total_memory();
        assert!(v.used_bytes() > 0 && v.used_bytes() < total);
    }
}
