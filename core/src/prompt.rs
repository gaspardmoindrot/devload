//! The Analyze prompt: the text the Analyze button copies. It tells an AI
//! coding agent (Claude Code, Codex…) where devload's SQLite database is, how
//! to read it and what to look for. devload launches nothing.

use crate::model::Pressure;
use std::fmt::Write;

/// One of the heaviest groups of the period.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptGroup {
    pub name: String,
    /// "dev project", "docker stack", "app" or "system".
    pub kind: &'static str,
    /// Bytes.
    pub footprint: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PromptInput {
    /// The period shown by the graph, Unix seconds.
    pub start_ts: i64,
    pub end_ts: i64,
    /// Absolute path of the database (or starting with `~/`).
    pub db_path: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub pressure: Pressure,
    /// Since when the pressure has been at its current level, Unix seconds.
    pub since_ts: i64,
    /// Whole-machine CPU, 0–100.
    pub cpu_pct: f32,
    /// Up to 5, heaviest first.
    pub top_groups: Vec<PromptGroup>,
    /// Folders of the dev projects involved.
    pub worktree_paths: Vec<String>,
}

pub fn build(input: &PromptInput) -> String {
    let end = local_time(input.end_ts);
    let start = local_time(input.start_ts);
    let total = gb(input.total_bytes);
    let mut s = String::with_capacity(2048);

    // The start always carries its day; the end and "since" drop it when on
    // the same day as the time they follow.
    let _ = write!(
        s,
        "My Mac ({total} GB of RAM) was under load between {} ({}) and {} ({}). \
         Find what used the memory and the CPU, why, and what I can change.\n\n",
        start.format(None),
        input.start_ts,
        end.format(Some(&start)),
        input.end_ts,
    );
    let _ = write!(
        s,
        "The measurements are in a SQLite database kept by devload:\n  {}\n\n",
        shell_quote(&input.db_path)
    );
    s.push_str(
        "Open it read-only (sqlite3 -readonly) and never write to it. Start with .schema: every column is commented. \
         Times are Unix seconds, memory is in MiB, CPU is in per-mille of one core. \
         The data is up to date until meta.last_flush_ts.\n\n\
         Tables:\n\
         - machine_minute (7 days) and machine_hour (90 days): memory used, pressure and CPU of the whole machine\n\
         - groups: the dev projects (worktrees), Docker stacks and apps, with their folder\n\
         - group_minute (7 days) and group_hour (90 days): every group above 50 MiB over time\n\
         - process_minute (7 days) and process_hour (90 days): the 3 biggest processes of each group; names and arguments are in strings\n\
         - pressure_episodes: the orange and red episodes\n\n",
    );
    let pressure = match input.pressure {
        Pressure::Normal => "Normal",
        Pressure::Tight => "Tight",
        Pressure::Critical => "Critical",
    };
    let cpu = if input.cpu_pct.is_finite() { input.cpu_pct.clamp(0.0, 100.0) } else { 0.0 };
    let _ = writeln!(
        s,
        "Right now: {} / {total} GB used, pressure {pressure} since {} ({}), CPU {cpu:.0}%.",
        gb(input.used_bytes),
        local_time(input.since_ts).format(Some(&end)),
        input.since_ts,
    );
    if !input.top_groups.is_empty() {
        let list: Vec<String> = input
            .top_groups
            .iter()
            .map(|g| format!("{} ({}) {} GB", g.name, g.kind, gb(g.footprint)))
            .collect();
        let _ = writeln!(s, "Biggest consumers: {}.", list.join(", "));
    }
    s.push('\n');
    if !input.worktree_paths.is_empty() {
        s.push_str(
            "Dev projects involved, whose code you can read (dev server config, watchers, docker-compose, scripts):\n",
        );
        for p in &input.worktree_paths {
            let _ = writeln!(s, "- {p}");
        }
        s.push('\n');
    }
    s.push_str(
        "Answer with: what used the memory and the CPU, when, and how much; \
         the probable cause, with file:line when it is in the code; what I can change. \
         Do not modify any file without asking me.\n",
    );
    s
}

/// Bytes → GB (2^30, like Activity Monitor) with one decimal: "19.5".
pub fn gb(bytes: u64) -> String {
    // Integer tenths, rounded half up: no float drift on exact halves.
    let tenths = (bytes as u128 * 10 + (1u128 << 29)) >> 30;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// A path that survives a copy-paste into a shell: left bare when it only has
/// safe characters, single-quoted otherwise (`'` written as `'\''`). A leading
/// `~/` stays outside the quotes so the shell still expands it.
pub fn shell_quote(path: &str) -> String {
    let (home, rest) = match path.strip_prefix("~/") {
        Some(r) => ("~/", r),
        None => ("", path),
    };
    let safe = !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+@%:,=".contains(&b));
    if safe {
        return format!("{home}{rest}");
    }
    format!("{home}'{}'", rest.replace('\'', r"'\''"))
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

#[derive(Clone, Copy, Debug)]
struct LocalTime {
    year: i32,
    yday: i32,
    month: usize,
    mday: i32,
    hour: i32,
    min: i32,
}

impl LocalTime {
    /// "14:05" when on the same local day as `same_day_as`, else "Nov 24, 14:05".
    fn format(&self, same_day_as: Option<&LocalTime>) -> String {
        match same_day_as {
            Some(o) if o.year == self.year && o.yday == self.yday => format!("{:02}:{:02}", self.hour, self.min),
            _ => format!("{} {}, {:02}:{:02}", MONTHS[self.month], self.mday, self.hour, self.min),
        }
    }
}

fn local_time(ts: i64) -> LocalTime {
    let t = ts as libc::time_t;
    // SAFETY: localtime_r writes into `tm`, which we own; a zeroed tm is valid.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = !unsafe { libc::localtime_r(&t, &mut tm) }.is_null();
    if !ok {
        return LocalTime { year: 70, yday: 0, month: 0, mday: 1, hour: 0, min: 0 };
    }
    LocalTime {
        year: tm.tm_year,
        yday: tm.tm_yday,
        month: tm.tm_mon.clamp(0, 11) as usize,
        mday: tm.tm_mday,
        hour: tm.tm_hour,
        min: tm.tm_min,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    unsafe extern "C" {
        fn tzset();
    }

    /// Puts the test process in UTC. Only `all_time_dependent_text` calls
    /// it, so no other test races on TZ.
    fn set_utc() {
        // SAFETY: called once, from the only test that reads local time.
        unsafe {
            libc::setenv(c"TZ".as_ptr(), c"UTC".as_ptr(), 1);
            tzset();
        }
    }

    fn input() -> PromptInput {
        PromptInput {
            // 2026-11-24 14:05 → 15:05 UTC
            start_ts: 1_795_529_100,
            end_ts: 1_795_532_700,
            db_path: "/Users/me/Library/Application Support/devload/devload.sqlite".into(),
            total_bytes: 32 * GIB,
            used_bytes: 19 * GIB + GIB / 2,
            pressure: Pressure::Tight,
            since_ts: 1_795_530_000, // 14:20
            cpu_pct: 37.4,
            top_groups: vec![
                PromptGroup { name: "feat-checkout".into(), kind: "dev project", footprint: 4 * GIB + GIB / 5 },
                PromptGroup { name: "shop".into(), kind: "docker stack", footprint: 3 * GIB },
                PromptGroup { name: "Google Chrome".into(), kind: "app", footprint: 2 * GIB + GIB / 2 },
                PromptGroup { name: "System".into(), kind: "system", footprint: 3 * GIB / 2 },
            ],
            worktree_paths: vec!["/Users/me/git/shop-wt/feat-checkout".into(), "/Users/me/git/shop".into()],
        }
    }

    const GOLDEN: &str = "\
My Mac (32.0 GB of RAM) was under load between Nov 24, 14:05 (1795529100) and 15:05 (1795532700). Find what used the memory and the CPU, why, and what I can change.

The measurements are in a SQLite database kept by devload:
  '/Users/me/Library/Application Support/devload/devload.sqlite'

Open it read-only (sqlite3 -readonly) and never write to it. Start with .schema: every column is commented. Times are Unix seconds, memory is in MiB, CPU is in per-mille of one core. The data is up to date until meta.last_flush_ts.

Tables:
- machine_minute (7 days) and machine_hour (90 days): memory used, pressure and CPU of the whole machine
- groups: the dev projects (worktrees), Docker stacks and apps, with their folder
- group_minute (7 days) and group_hour (90 days): every group above 50 MiB over time
- process_minute (7 days) and process_hour (90 days): the 3 biggest processes of each group; names and arguments are in strings
- pressure_episodes: the orange and red episodes

Right now: 19.5 / 32.0 GB used, pressure Tight since 14:20 (1795530000), CPU 37%.
Biggest consumers: feat-checkout (dev project) 4.2 GB, shop (docker stack) 3.0 GB, Google Chrome (app) 2.5 GB, System (system) 1.5 GB.

Dev projects involved, whose code you can read (dev server config, watchers, docker-compose, scripts):
- /Users/me/git/shop-wt/feat-checkout
- /Users/me/git/shop

Answer with: what used the memory and the CPU, when, and how much; the probable cause, with file:line when it is in the code; what I can change. Do not modify any file without asking me.
";

    /// Every assert that depends on the local time zone, in ONE test.
    #[test]
    fn all_time_dependent_text() {
        let _tz = crate::TZ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_utc();
        let i = input();
        assert_eq!(build(&i), GOLDEN);

        // A period over two days: both ends carry their day.
        let mut j = i.clone();
        j.start_ts -= 86_400;
        let out = build(&j);
        assert!(out.contains("between Nov 23, 14:05 (1795442700) and Nov 24, 15:05 (1795532700)."), "{out}");

        // Pressure that started before the period, the day before: still printed, with its day.
        let mut k = i.clone();
        k.since_ts = i.start_ts - 86_400;
        k.pressure = Pressure::Critical;
        let out = build(&k);
        assert!(out.contains("pressure Critical since Nov 23, 14:05 (1795442700), CPU 37%."), "{out}");

        // Midnight boundary and zero padding.
        assert_eq!(local_time(1_795_564_800).format(None), "Nov 25, 00:00");
        assert_eq!(local_time(1_795_564_799).format(Some(&local_time(1_795_564_800))), "Nov 24, 23:59");
    }

    #[test]
    fn empty_lists_drop_their_blocks() {
        let mut i = input();
        i.top_groups.clear();
        i.worktree_paths.clear();
        let out = build(&i);
        assert!(!out.contains("Biggest consumers"));
        assert!(!out.contains("Dev projects involved"));
        assert!(out.contains("CPU 37%.\n\nAnswer with:"), "{out}");
        assert!(!out.contains("\n\n\n"));
        assert!(out.ends_with("without asking me.\n"));
    }

    #[test]
    fn paths_are_shell_safe() {
        assert_eq!(shell_quote("/Users/me/.devload/devload.sqlite"), "/Users/me/.devload/devload.sqlite");
        assert_eq!(
            shell_quote("/Users/me/Library/Application Support/devload/devload.sqlite"),
            "'/Users/me/Library/Application Support/devload/devload.sqlite'"
        );
        assert_eq!(shell_quote("/tmp/it's here/db"), r"'/tmp/it'\''s here/db'");
        assert_eq!(shell_quote("~/Library/Application Support/x.db"), "~/'Library/Application Support/x.db'");
        assert_eq!(shell_quote("~/x.db"), "~/x.db");
        assert_eq!(shell_quote("/a/$HOME/`b`"), "'/a/$HOME/`b`'");
        assert_eq!(shell_quote(""), "''");

        let mut i = input();
        i.db_path = "/tmp/it's here/devload.sqlite".into();
        assert!(build(&i).contains("\n  '/tmp/it'\\''s here/devload.sqlite'\n"));
    }

    #[test]
    fn gb_has_one_decimal_like_activity_monitor() {
        assert_eq!(gb(0), "0.0");
        assert_eq!(gb(GIB), "1.0");
        assert_eq!(gb(19 * GIB + GIB / 2), "19.5");
        assert_eq!(gb(GIB / 4), "0.3"); // 0.25 exactly: half rounds up
        assert_eq!(gb(GIB / 4 - 1), "0.2");
        assert_eq!(gb(GIB - GIB / 20), "1.0"); // 0.95 → 1.0, not "0.10"
        assert_eq!(gb(GIB - GIB / 10), "0.9");
        assert_eq!(gb(300 << 20), "0.3");
        assert_eq!(gb(64 * GIB), "64.0");
        assert_eq!(gb(u64::MAX), "17179869184.0");
    }
}
