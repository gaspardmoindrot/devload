//! Types shared by the collector, the store, the graph series and the prompt.
//!
//! Units, everywhere in the core: timestamps in Unix seconds (wall clock),
//! memory in bytes in RAM and in MiB (integers) once aggregated to minutes,
//! CPU in per-mille of ONE core (a process using two full cores = 2000).

use std::sync::Arc;

/// Group id: `groups.rowid` in the database. Stable across restarts.
pub type Gid = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum GroupKind {
    /// A git worktree, or a plain folder an agent runs in. "Dev projects".
    Worktree = 0,
    /// A macOS app (outermost .app bundle). "Apps".
    App = 1,
    /// A docker compose stack (or a lone container). Child of a worktree
    /// when its compose working_dir is inside one.
    Docker = 2,
    /// Everything else, including processes we cannot read. One group.
    System = 3,
}

impl GroupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GroupKind::Worktree => "worktree",
            GroupKind::App => "app",
            GroupKind::Docker => "docker",
            GroupKind::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "worktree" => GroupKind::Worktree,
            "app" => GroupKind::App,
            "docker" => GroupKind::Docker,
            "system" => GroupKind::System,
            _ => return None,
        })
    }
}

/// A row of the `groups` table. Identity is (kind, path): the worktree root,
/// the .app bundle path, the compose project key ("<project>@<working_dir>")
/// or "" for System.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: Gid,
    pub kind: GroupKind,
    /// Display name: branch, folder, app name, compose project, "System".
    pub name: String,
    pub path: String,
    /// Docker stacks under a worktree point at it.
    pub parent: Option<Gid>,
}

/// Displayed pressure colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Pressure {
    #[default]
    Normal = 0,
    Tight = 1,
    Critical = 2,
}

impl Pressure {
    pub fn from_u8(v: u8) -> Self {
        match v {
            2 => Pressure::Critical,
            1 => Pressure::Tight,
            _ => Pressure::Normal,
        }
    }
}

// ------------------------------------------------------------------ 5 s ticks

/// Machine state at one tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MachineTick {
    pub ts: i64,
    pub used: u64,
    pub total: u64,
    pub pressure: Pressure,
    /// `kern.memorystatus_level`, % of memory available.
    pub avail_pct: u8,
    pub cpu_pm: u32,
}

/// One group at one tick. `footprint` is the group's OWN processes (a worktree
/// does not include its docker stacks: the graph sums children into parents).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupTick {
    pub gid: Gid,
    pub footprint: u64,
    pub cpu_pm: u32,
}

/// One of the biggest processes of a group at one tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcTick {
    pub gid: Gid,
    pub pid: i32,
    /// Short program name ("node", "postgres", "Google Chrome Helper").
    pub name: Arc<str>,
    /// Arguments, already scrubbed of secrets and truncated (~300 chars).
    pub args: Arc<str>,
    pub footprint: u64,
    pub cpu_pm: u32,
}

// ------------------------------------------------------------------ minutes

/// `machine_minute` / `machine_hour` row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MachineMinute {
    /// Start of the minute (or hour), Unix seconds, multiple of 60 (3600).
    pub ts: i64,
    pub used_avg_mib: u32,
    pub used_max_mib: u32,
    pub total_mib: u32,
    /// Max `Pressure` as u8.
    pub pressure_max: u8,
    pub avail_min_pct: u8,
    pub cpu_avg_pm: u32,
    pub cpu_max_pm: u32,
}

/// `group_minute` / `group_hour` row. Only groups whose max is >= 50 MiB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupMinute {
    pub ts: i64,
    pub gid: Gid,
    pub mib_avg: u32,
    pub mib_max: u32,
    pub cpu_avg_pm: u32,
}

/// `process_minute` / `process_hour` row: top 3 processes of a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcMinute {
    pub ts: i64,
    pub gid: Gid,
    /// 1, 2, 3 by footprint max.
    pub rank: u8,
    pub pid: i32,
    pub name: Arc<str>,
    pub args: Arc<str>,
    pub mib_max: u32,
    pub cpu_avg_pm: u32,
}

/// Everything aggregated for one finished minute.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Minute {
    pub machine: MachineMinute,
    pub groups: Vec<GroupMinute>,
    pub procs: Vec<ProcMinute>,
}

/// An orange or red period. `end` is None while it lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Episode {
    pub start: i64,
    pub end: Option<i64>,
    pub level: Pressure,
    /// Heaviest group (own + children) during the episode.
    pub top_gid: Option<Gid>,
}

pub const MIB: u64 = 1 << 20;

pub fn to_mib(bytes: u64) -> u32 {
    ((bytes + MIB / 2) / MIB).min(u32::MAX as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_kinds_round_trip_through_their_database_names() {
        for k in [GroupKind::Worktree, GroupKind::App, GroupKind::Docker, GroupKind::System] {
            assert_eq!(GroupKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(GroupKind::parse("App"), None);
    }

    #[test]
    fn pressure_from_a_byte_never_fails() {
        assert_eq!([0, 1, 2].map(Pressure::from_u8), [Pressure::Normal, Pressure::Tight, Pressure::Critical]);
        // A value from a newer build reads as Normal rather than panicking.
        assert_eq!(Pressure::from_u8(7), Pressure::Normal);
    }

    #[test]
    fn to_mib_rounds_and_saturates() {
        assert_eq!([0, MIB / 2 - 1, MIB / 2, 3 * MIB].map(to_mib), [0, 0, 1, 3]);
        assert_eq!(to_mib(u64::MAX / 2), u32::MAX);
    }
}
