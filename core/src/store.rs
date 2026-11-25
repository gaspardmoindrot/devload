//! The SQLite store: `<data dir>/devload.sqlite`.
//!
//! The present lives in RAM (the engine); this module receives one
//! transaction every 5 minutes (`flush`) and runs the hourly upkeep
//! (`maintenance`): rollup of minutes into hours, retention, purge of unused
//! strings, checkpoint, conditional incremental vacuum.
//!
//! Only the AI agent (through `sqlite3`) and the graph reads below query the
//! database. The schema comments are the documentation that agent reads with
//! `sqlite3 devload.sqlite .schema`, so they say units and meaning precisely.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::model::{Episode, Gid, Group, GroupKind, GroupMinute, MachineMinute, Minute, Pressure};

pub const SCHEMA_VERSION: i64 = 1;
pub const TICK_SECONDS: i64 = 5;
pub const RETENTION_MINUTE_DAYS: i64 = 7;
pub const RETENTION_HOUR_DAYS: i64 = 90;
const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;
/// `incremental_vacuum(N)` runs only when free pages exceed this share of the file.
const VACUUM_FREE_PCT: i64 = 10;
const VACUUM_PAGES: i64 = 256;

/// Tables, in creation order. The `--` comments are kept by SQLite in
/// `sqlite_master` (everything after the opening parenthesis is) and shown by
/// `.schema`.
const SCHEMA: &str = r#"
CREATE TABLE meta( -- Settings and bookkeeping of devload, one row per key. Every value is an integer.
  key   TEXT PRIMARY KEY NOT NULL, -- 'schema_version': version of this schema (1).
                                   -- 'last_flush_ts': Unix s of the last write. Minutes after it are still in the app's RAM, not in this file.
                                   -- 'tick_seconds': sampling interval (5 s): a minute row summarizes ~12 samples.
                                   -- 'retention_minute_days': *_minute rows older than this many days (7) are deleted; use *_hour rows beyond.
                                   -- 'retention_hour_days': *_hour rows and pressure_episodes older than this many days (90) are deleted.
                                   -- 'rollup_done_until': Unix s; every hour that starts before it has its *_hour rows.
                                   -- 'strings_purged_ts': Unix s of the last daily purge of unreferenced strings and stale groups.
  value INTEGER NOT NULL           -- the value, in the unit given above.
) WITHOUT ROWID;

CREATE TABLE machine_minute( -- The whole Mac, one row per minute. Kept 7 days (older: machine_hour). A missing minute = app not running or Mac asleep.
  ts            INTEGER PRIMARY KEY NOT NULL, -- start of the minute, Unix s (UTC), multiple of 60.
  used_avg_mib  INTEGER NOT NULL, -- memory used (app + wired + compressed, like Activity Monitor's "Memory Used"), average of the minute's samples, MiB.
  used_max_mib  INTEGER NOT NULL, -- same, highest sample of the minute, MiB.
  total_mib     INTEGER NOT NULL, -- physical RAM of the Mac, MiB.
  pressure_max  INTEGER NOT NULL, -- worst memory-pressure colour shown during the minute: 0 normal (green), 1 tight (orange), 2 critical (red).
  avail_min_pct INTEGER NOT NULL, -- lowest kern.memorystatus_level of the minute: % of memory the kernel considers available (lower = worse).
  cpu_avg_pm    INTEGER NOT NULL, -- CPU of the whole Mac, average, per-mille of ONE core (12 busy cores = 12000).
  cpu_max_pm    INTEGER NOT NULL  -- same, highest sample.
) WITHOUT ROWID;

CREATE TABLE machine_hour( -- Same as machine_minute, one row per hour, rolled up from it. Kept 90 days.
  ts            INTEGER PRIMARY KEY NOT NULL, -- start of the hour, Unix s (UTC), multiple of 3600.
  used_avg_mib  INTEGER NOT NULL, -- average of the hour's minute averages, MiB.
  used_max_mib  INTEGER NOT NULL, -- highest minute max, MiB.
  total_mib     INTEGER NOT NULL, -- physical RAM, MiB.
  pressure_max  INTEGER NOT NULL, -- worst colour of the hour: 0 normal, 1 tight (orange), 2 critical (red).
  avail_min_pct INTEGER NOT NULL, -- lowest % of available memory of the hour.
  cpu_avg_pm    INTEGER NOT NULL, -- average CPU, per-mille of one core.
  cpu_max_pm    INTEGER NOT NULL, -- highest sample, per-mille of one core.
  minutes       INTEGER NOT NULL  -- number of minute rows rolled into this hour (60 = the app ran the whole hour).
) WITHOUT ROWID;

CREATE TABLE groups( -- What memory is attributed to. groups.id is the group_id of every other table.
  id        INTEGER PRIMARY KEY, -- stable id.
  kind      TEXT NOT NULL, -- 'worktree': a git worktree or a folder an agent (Claude Code, Codex) runs in ("dev projects").
                           -- 'app': a macOS app (outermost .app bundle; helpers count in their app).
                           -- 'docker': a docker compose stack or a lone container (its containers' memory, taken out of the VM process that holds it: Docker Desktop's or OrbStack's app, System for Colima).
                           -- 'system': everything else, one group; includes processes of root and other users, whose memory cannot be read (counted as 0).
  name      TEXT NOT NULL, -- display name: git branch or folder name, app name, compose project, 'System'.
  path      TEXT NOT NULL, -- identity, with kind: worktree root, .app bundle path, '<compose project>@<compose working dir>' for docker, '' for system.
  parent_id INTEGER,       -- for a docker stack whose compose dir is inside a worktree: that worktree's id. NULL otherwise.
  first_ts  INTEGER NOT NULL, -- Unix s when first seen.
  last_ts   INTEGER NOT NULL  -- Unix s, start of the last minute with a group_minute row (= first_ts if it never had one). Refreshed hourly, so it can lag group_minute by up to an hour. Groups without rows for 90 days are deleted.
);

CREATE TABLE group_minute( -- Memory of each group, one row per minute, ONLY for groups whose max that minute is >= 50 MiB (no row = smaller or absent). Kept 7 days.
  ts         INTEGER NOT NULL, -- start of the minute, Unix s (same ts as machine_minute).
  group_id   INTEGER NOT NULL, -- groups.id. The group's OWN processes only: a docker stack is a separate group (its parent_id is the worktree) and is NOT included in the worktree's row. Worktree total = its row + its children's rows at the same ts.
  mib_avg    INTEGER NOT NULL, -- sum of the processes' physical footprint (Activity Monitor's "Memory" column; for docker: containers' usage minus inactive file cache), average over the minute, MiB.
  mib_max    INTEGER NOT NULL, -- same, highest sample of the minute, MiB.
  cpu_avg_pm INTEGER NOT NULL, -- CPU of the group's processes, average, per-mille of ONE core.
  PRIMARY KEY (ts, group_id)
) WITHOUT ROWID;

CREATE TABLE group_hour( -- Same as group_minute, one row per hour and group, rolled up from it. Kept 90 days.
  ts         INTEGER NOT NULL, -- start of the hour, Unix s, multiple of 3600.
  group_id   INTEGER NOT NULL, -- groups.id (own processes only, see group_minute).
  mib_avg    INTEGER NOT NULL, -- average of the group's minute averages, over the minutes it had a row (>= 50 MiB), MiB.
  mib_max    INTEGER NOT NULL, -- highest minute max, MiB.
  cpu_avg_pm INTEGER NOT NULL, -- average CPU over the same minutes, per-mille of one core.
  minutes    INTEGER NOT NULL, -- number of group_minute rows rolled into this hour (how long it was >= 50 MiB).
  PRIMARY KEY (ts, group_id)
) WITHOUT ROWID;

CREATE TABLE process_minute( -- The 3 biggest processes of each group_minute row. Kept 7 days. Names and args are ids into strings.
  ts         INTEGER NOT NULL, -- start of the minute, Unix s (same as group_minute.ts).
  group_id   INTEGER NOT NULL, -- groups.id.
  rank       INTEGER NOT NULL, -- 1, 2, 3 by mib_max within the group and minute.
  pid        INTEGER NOT NULL, -- process id (macOS reuses pids: compare name and args too).
  name_id    INTEGER NOT NULL, -- strings.id of the program name ('node', 'postgres', 'Google Chrome Helper').
  args_id    INTEGER,          -- strings.id of the command-line arguments (secrets scrubbed, truncated to ~300 chars). NULL if none.
  mib_max    INTEGER NOT NULL, -- physical footprint, highest sample of the minute, MiB.
  cpu_avg_pm INTEGER NOT NULL, -- CPU, average over the minute, per-mille of ONE core.
  PRIMARY KEY (ts, group_id, rank)
) WITHOUT ROWID;

CREATE TABLE process_hour( -- The 3 biggest processes of each group and hour, rolled up from process_minute, to explain a peak older than 7 days. Kept 90 days.
  ts         INTEGER NOT NULL, -- start of the hour, Unix s, multiple of 3600.
  group_id   INTEGER NOT NULL, -- groups.id.
  rank       INTEGER NOT NULL, -- 1, 2, 3 by mib_max over the hour.
  pid        INTEGER NOT NULL, -- process id.
  name_id    INTEGER NOT NULL, -- strings.id of the program name.
  args_id    INTEGER,          -- strings.id of the arguments, NULL if none.
  mib_max    INTEGER NOT NULL, -- highest minute footprint of this process in the hour, MiB.
  cpu_avg_pm INTEGER NOT NULL, -- CPU averaged over the minutes it was in its group's top 3, per-mille of one core.
  PRIMARY KEY (ts, group_id, rank)
) WITHOUT ROWID;

CREATE TABLE strings( -- Process names and arguments, each text stored once. Use: JOIN strings n ON n.id = p.name_id LEFT JOIN strings a ON a.id = p.args_id.
  id   INTEGER PRIMARY KEY, -- referenced by process_minute and process_hour (name_id, args_id).
  text TEXT NOT NULL        -- no unique index on purpose (devload dedups in RAM). Rows no longer referenced are deleted once a day.
);

CREATE TABLE pressure_episodes( -- Periods when the menu bar showed orange or red. Kept 90 days.
  start        INTEGER PRIMARY KEY NOT NULL, -- Unix s when the colour turned orange or red.
  end          INTEGER,          -- Unix s when it went back down, or when the app stopped during it; NULL while ongoing.
  level        INTEGER NOT NULL, -- 1 tight (orange), 2 critical (red).
  top_group_id INTEGER           -- groups.id of the heaviest group during the episode, its docker children included. NULL if unknown.
) WITHOUT ROWID;
"#;

/// What one `maintenance` pass did (for logs and the proof harness).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// Hours that got their `*_hour` rows in this pass.
    pub rolled_hours: u32,
    /// Rows deleted by retention (all tables).
    pub deleted_rows: u64,
    /// Rows of `strings` purged (daily).
    pub strings_purged: u64,
    /// Ids of the `groups` rows purged (daily): the engine forgets them.
    pub purged_groups: Vec<Gid>,
    /// Whether `incremental_vacuum` ran.
    pub vacuumed: bool,
    /// Frames in the WAL when it was checkpointed (each frame = 4096 + 24 bytes).
    pub wal_frames: i64,
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
    /// text -> string, the dedup the database does not do (no UNIQUE index).
    /// Same rows as the `strings` table.
    strings: HashMap<Box<str>, Str>,
    next_string_id: i64,
    /// Groups as last written. A group that did not change is not rewritten
    /// at each flush; its `last_ts` is refreshed by the hourly rollup.
    written: HashMap<Gid, Group>,
    /// `max(process_minute.ts)` when the store was opened: rows up to it were
    /// written by an earlier run, so their strings have no `last_used`.
    minute_ts_at_open: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
struct Str {
    id: i64,
    /// Latest `process_minute.ts` written by this run that references it
    /// (i64::MIN if none). Lets the daily purge skip scanning process_minute.
    last_used: i64,
}

fn floor_hour(ts: i64) -> i64 {
    ts.div_euclid(HOUR) * HOUR
}

fn mismatch(msg: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH), Some(msg))
}

impl Store {
    /// Opens (or creates) `<dir>/devload.sqlite`, creating `dir` if needed.
    pub fn open(dir: &Path) -> rusqlite::Result<Store> {
        std::fs::create_dir_all(dir).map_err(|e| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                Some(format!("cannot create {}: {e}", dir.display())),
            )
        })?;
        let path = dir.join("devload.sqlite");
        let conn = Connection::open(&path)?;
        let exists: bool = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
        if !exists {
            // Order matters: page_size and auto_vacuum only apply before the
            // first table exists; WAL is switched on after the schema.
            conn.execute_batch("PRAGMA page_size = 4096; PRAGMA auto_vacuum = INCREMENTAL;")?;
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA)?;
            {
                let mut ins = tx.prepare("INSERT INTO meta(key, value) VALUES (?1, ?2)")?;
                for (k, v) in [
                    ("schema_version", SCHEMA_VERSION),
                    ("tick_seconds", TICK_SECONDS),
                    ("retention_minute_days", RETENTION_MINUTE_DAYS),
                    ("retention_hour_days", RETENTION_HOUR_DAYS),
                ] {
                    ins.execute(params![k, v])?;
                }
            }
            tx.commit()?;
        } else {
            let v: Option<i64> = conn
                .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0))
                .optional()?;
            if v != Some(SCHEMA_VERSION) {
                return Err(mismatch(format!(
                    "{}: schema_version {v:?}, this build knows {SCHEMA_VERSION}",
                    path.display()
                )));
            }
        }
        // journal_mode persists in the file; the other two are per connection.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(mismatch(format!("journal_mode is {mode}, expected wal")));
        }
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        conn.query_row("PRAGMA wal_autocheckpoint = 0", [], |_| Ok(()))?;
        conn.set_prepared_statement_cache_capacity(32);

        // An episode still open was left by a crash: it ended at the last write at the latest.
        conn.execute(
            "UPDATE pressure_episodes SET end = max(start, coalesce((SELECT value FROM meta WHERE key = 'last_flush_ts'), start))
              WHERE end IS NULL",
            [],
        )?;
        let minute_ts_at_open = conn.query_row("SELECT max(ts) FROM process_minute", [], |r| r.get(0))?;
        let mut store =
            Store { conn, path, strings: HashMap::new(), next_string_id: 1, written: HashMap::new(), minute_ts_at_open };
        store.load_strings()?;
        store.written = store.groups()?.into_iter().map(|g| (g.id, g)).collect();
        Ok(store)
    }

    /// Path of the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load_strings(&mut self) -> rusqlite::Result<()> {
        self.strings.clear();
        let mut max = 0i64;
        let mut st = self.conn.prepare("SELECT id, text FROM strings")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, text) = row?;
            max = max.max(id);
            self.strings.insert(text.into_boxed_str(), Str { id, last_used: i64::MIN });
        }
        self.next_string_id = max + 1;
        Ok(())
    }

    /// Number of distinct strings known (RAM map = `strings` table).
    pub fn string_count(&self) -> usize {
        self.strings.len()
    }

    /// An integer of the `meta` table.
    pub fn meta(&self, key: &str) -> rusqlite::Result<Option<i64>> {
        self.conn
            .prepare_cached("SELECT value FROM meta WHERE key = ?1")?
            .query_row([key], |r| r.get(0))
            .optional()
    }

    /// All known groups, by id. The engine loads them at start so ids stay
    /// stable across restarts, and assigns new ids itself (max + 1).
    pub fn groups(&self) -> rusqlite::Result<Vec<Group>> {
        let mut st = self.conn.prepare_cached("SELECT id, kind, name, path, parent_id FROM groups ORDER BY id")?;
        let rows = st.query_map([], |r| {
            let kind: String = r.get(1)?;
            let kind = GroupKind::parse(&kind).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, format!("unknown group kind {kind:?}").into())
            })?;
            Ok(Group { id: r.get(0)?, kind, name: r.get(2)?, path: r.get(3)?, parent: r.get(4)? })
        })?;
        rows.collect()
    }

    /// Writes everything in ONE transaction: upserts the `groups` that are
    /// new or changed (kind, name, path, parent; passing unchanged groups
    /// costs nothing), inserts the minute rows (a minute written twice is
    /// replaced), upserts `episodes` (keyed by start: re-send one when it
    /// ends) and sets `meta.last_flush_ts = now`.
    pub fn flush(&mut self, now: i64, groups: &[Group], minutes: &[Minute], episodes: &[Episode]) -> rusqlite::Result<()> {
        let changed: Vec<&Group> = groups.iter().filter(|g| self.written.get(&g.id) != Some(*g)).collect();
        let Store { conn, strings, next_string_id, .. } = self;
        let first_new_id = *next_string_id;
        let mut added: Vec<Box<str>> = Vec::new();
        let res = (|| {
            let tx = conn.transaction()?;
            write_all(&tx, now, &changed, minutes, episodes, strings, next_string_id, &mut added)?;
            tx.commit()
        })();
        match res {
            Ok(()) => {
                for g in changed {
                    self.written.insert(g.id, g.clone());
                }
            }
            Err(_) => {
                // The rows were rolled back: forget the strings they introduced.
                for s in &added {
                    strings.remove(s);
                }
                *next_string_id = first_new_id;
            }
        }
        res
    }

    /// Hourly upkeep. Rolls every complete hour not yet rolled into the
    /// `*_hour` tables, applies retention, purges unreferenced strings and
    /// stale groups once a day, runs `incremental_vacuum` only if free pages
    /// exceed 10 % of the file, then `wal_checkpoint(TRUNCATE)`.
    pub fn maintenance(&mut self, now: i64) -> rusqlite::Result<MaintenanceReport> {
        let mut report = self.maintenance_before_checkpoint(now)?;
        report.wal_frames = self.checkpoint()?;
        Ok(report)
    }

    /// `maintenance` without the final checkpoint. Exposed so the proof
    /// harness can measure the WAL before it is truncated; use `maintenance`.
    pub fn maintenance_before_checkpoint(&mut self, now: i64) -> rusqlite::Result<MaintenanceReport> {
        let mut rep = MaintenanceReport::default();
        let mut doomed_strings: HashSet<i64> = HashSet::new();
        let mut doomed_groups: Vec<Gid> = Vec::new();
        {
            let tx = self.conn.transaction()?;
            let get = |k: &str| -> rusqlite::Result<Option<i64>> {
                tx.prepare_cached("SELECT value FROM meta WHERE key = ?1")?.query_row([k], |r| r.get(0)).optional()
            };
            let set = |k: &str, v: i64| -> rusqlite::Result<()> {
                tx.prepare_cached("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)")?.execute(params![k, v])?;
                Ok(())
            };

            // 1. Rollup of complete hours: an hour is complete once `now` has
            //    left it and a later minute (or its last one) has been written.
            let span: (Option<i64>, Option<i64>) =
                tx.query_row("SELECT min(ts), max(ts) FROM machine_minute", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let done = get("rollup_done_until")?;
            if let (Some(min_ts), Some(max_ts)) = span {
                let from = done.unwrap_or(floor_hour(min_ts)).max(floor_hour(min_ts));
                let until = floor_hour(now).min(floor_hour(max_ts + 60));
                if until > from {
                    rep.rolled_hours = rollup(&tx, from, until)?;
                    set("rollup_done_until", until)?;
                }
            }
            let done = get("rollup_done_until")?.unwrap_or(i64::MIN);

            // 2. Retention. Minute rows are never deleted before being rolled up.
            let cut_min = (now - RETENTION_MINUTE_DAYS * DAY).min(done);
            let cut_hour = now - RETENTION_HOUR_DAYS * DAY;
            for (sql, cut) in [
                ("DELETE FROM machine_minute WHERE ts < ?1", cut_min),
                ("DELETE FROM group_minute WHERE ts < ?1", cut_min),
                ("DELETE FROM process_minute WHERE ts < ?1", cut_min),
                ("DELETE FROM machine_hour WHERE ts < ?1", cut_hour),
                ("DELETE FROM group_hour WHERE ts < ?1", cut_hour),
                ("DELETE FROM process_hour WHERE ts < ?1", cut_hour),
                ("DELETE FROM pressure_episodes WHERE coalesce(end, start) < ?1", cut_hour),
            ] {
                rep.deleted_rows += tx.prepare_cached(sql)?.execute([cut])? as u64;
            }

            // 3. Once a day: strings no longer referenced, groups with no row for 90 days.
            let last_purge = get("strings_purged_ts")?.unwrap_or(i64::MIN);
            if now.saturating_sub(last_purge) >= DAY {
                // A string is still referenced if process_hour uses it, or a
                // remaining process_minute row does: rows of this run are
                // known by `last_used` (no scan), rows of an earlier run are
                // scanned, only while they are younger than 7 days.
                let mut keep: HashSet<i64> = HashSet::new();
                let mut scan = |sql: &str, arg: Option<i64>| -> rusqlite::Result<()> {
                    let mut st = tx.prepare_cached(sql)?;
                    let mut rows = match arg {
                        Some(a) => st.query([a])?,
                        None => st.query([])?,
                    };
                    while let Some(r) = rows.next()? {
                        keep.insert(r.get(0)?);
                        if let Some(a) = r.get::<_, Option<i64>>(1)? {
                            keep.insert(a);
                        }
                    }
                    Ok(())
                };
                scan("SELECT name_id, args_id FROM process_hour", None)?;
                if let Some(m) = self.minute_ts_at_open.filter(|&m| m >= cut_min) {
                    scan("SELECT name_id, args_id FROM process_minute WHERE ts <= ?1", Some(m))?;
                }
                let mut del = tx.prepare_cached("DELETE FROM strings WHERE id = ?1")?;
                for s in self.strings.values() {
                    // i64::MIN = no row of this run references it, whatever cut_min is.
                    if (s.last_used == i64::MIN || s.last_used < cut_min) && !keep.contains(&s.id) {
                        del.execute([s.id])?;
                        doomed_strings.insert(s.id);
                    }
                }
                drop(del);
                rep.strings_purged = doomed_strings.len() as u64;
                let mut st = tx.prepare_cached(
                    "DELETE FROM groups WHERE last_ts < ?1
                       AND id NOT IN (SELECT top_group_id FROM pressure_episodes WHERE top_group_id IS NOT NULL)
                       AND id NOT IN (SELECT parent_id FROM groups WHERE parent_id IS NOT NULL AND last_ts >= ?1)
                     RETURNING id",
                )?;
                doomed_groups = st.query_map([cut_hour], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                drop(st);
                set("strings_purged_ts", now)?;
            }
            tx.commit()?;
            if self.minute_ts_at_open.is_some_and(|m| m < cut_min) {
                self.minute_ts_at_open = None; // those rows are gone
            }
        }
        if !doomed_strings.is_empty() {
            self.strings.retain(|_, s| !doomed_strings.contains(&s.id));
        }
        for g in &doomed_groups {
            self.written.remove(g);
        }
        rep.purged_groups = doomed_groups;

        // 4. Vacuum only when retention freed a lot: freed pages are normally reused.
        let (pages, free): (i64, i64) = self.conn.query_row(
            "SELECT (SELECT page_count FROM pragma_page_count), (SELECT freelist_count FROM pragma_freelist_count)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if free * 100 > pages * VACUUM_FREE_PCT {
            let mut st = self.conn.prepare(&format!("PRAGMA incremental_vacuum({VACUUM_PAGES})"))?;
            let mut rows = st.query([])?;
            while rows.next()?.is_some() {}
            rep.vacuumed = true;
        }
        Ok(rep)
    }

    /// `wal_checkpoint(TRUNCATE)`; returns the number of frames the WAL held.
    pub fn checkpoint(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(1))
    }

    /// `machine_minute` rows with `from <= ts < to`, by ts.
    pub fn machine_minutes(&self, from: i64, to: i64) -> rusqlite::Result<Vec<MachineMinute>> {
        let mut st = self.conn.prepare_cached(
            "SELECT ts, used_avg_mib, used_max_mib, total_mib, pressure_max, avail_min_pct, cpu_avg_pm, cpu_max_pm
               FROM machine_minute WHERE ts >= ?1 AND ts < ?2 ORDER BY ts",
        )?;
        let rows = st.query_map([from, to], |r| {
            Ok(MachineMinute {
                ts: r.get(0)?,
                used_avg_mib: r.get(1)?,
                used_max_mib: r.get(2)?,
                total_mib: r.get(3)?,
                pressure_max: r.get(4)?,
                avail_min_pct: r.get(5)?,
                cpu_avg_pm: r.get(6)?,
                cpu_max_pm: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// All `pressure_episodes` rows, by start.
    pub fn episodes(&self) -> rusqlite::Result<Vec<Episode>> {
        let mut st = self.conn.prepare_cached("SELECT start, end, level, top_group_id FROM pressure_episodes ORDER BY start")?;
        let rows = st.query_map([], |r| {
            Ok(Episode { start: r.get(0)?, end: r.get(1)?, level: Pressure::from_u8(r.get(2)?), top_gid: r.get(3)? })
        })?;
        rows.collect()
    }

    /// The latest `pressure_episodes` row (by start), if any.
    pub fn last_episode(&self) -> rusqlite::Result<Option<Episode>> {
        self.conn
            .prepare_cached("SELECT start, end, level, top_group_id FROM pressure_episodes ORDER BY start DESC LIMIT 1")?
            .query_row([], |r| {
                Ok(Episode { start: r.get(0)?, end: r.get(1)?, level: Pressure::from_u8(r.get(2)?), top_gid: r.get(3)? })
            })
            .optional()
    }

    /// `group_minute` rows with `from <= ts < to`, by ts then group id.
    pub fn group_minutes(&self, from: i64, to: i64) -> rusqlite::Result<Vec<GroupMinute>> {
        let mut st = self.conn.prepare_cached(
            "SELECT ts, group_id, mib_avg, mib_max, cpu_avg_pm
               FROM group_minute WHERE ts >= ?1 AND ts < ?2 ORDER BY ts, group_id",
        )?;
        let rows = st.query_map([from, to], |r| {
            Ok(GroupMinute { ts: r.get(0)?, gid: r.get(1)?, mib_avg: r.get(2)?, mib_max: r.get(3)?, cpu_avg_pm: r.get(4)? })
        })?;
        rows.collect()
    }
}

/// The body of `flush`, inside its transaction.
#[allow(clippy::too_many_arguments)]
fn write_all(
    tx: &Transaction,
    now: i64,
    groups: &[&Group],
    minutes: &[Minute],
    episodes: &[Episode],
    strings: &mut HashMap<Box<str>, Str>,
    next_id: &mut i64,
    added: &mut Vec<Box<str>>,
) -> rusqlite::Result<()> {
    // (first, last) minute of each group in this batch.
    let mut seen: HashMap<Gid, (i64, i64)> = HashMap::new();
    for m in minutes {
        for g in &m.groups {
            let e = seen.entry(g.gid).or_insert((g.ts, g.ts));
            e.0 = e.0.min(g.ts);
            e.1 = e.1.max(g.ts);
        }
    }
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO groups(id, kind, name, path, parent_id, first_ts, last_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, coalesce(?7, ?6))
             ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, name = excluded.name, path = excluded.path,
               parent_id = excluded.parent_id,
               last_ts = CASE WHEN ?7 IS NULL THEN last_ts ELSE max(last_ts, ?7) END",
        )?;
        for g in groups {
            let (first, last) = match seen.get(&g.id) {
                Some(&(a, b)) => (a, Some(b)),
                None => (now, None),
            };
            st.execute(params![g.id, g.kind.as_str(), g.name, g.path, g.parent, first, last])?;
        }
    }

    let mut mach = tx.prepare_cached("INSERT OR REPLACE INTO machine_minute VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?;
    let mut grp = tx.prepare_cached("INSERT OR REPLACE INTO group_minute VALUES (?1, ?2, ?3, ?4, ?5)")?;
    let mut prc = tx.prepare_cached("INSERT OR REPLACE INTO process_minute VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?;
    let mut ins_str = tx.prepare_cached("INSERT INTO strings(id, text) VALUES (?1, ?2)")?;
    let mut intern = |s: &str, ts: i64| -> rusqlite::Result<i64> {
        if let Some(e) = strings.get_mut(s) {
            e.last_used = e.last_used.max(ts);
            return Ok(e.id);
        }
        let id = *next_id;
        ins_str.execute(params![id, s])?;
        *next_id += 1;
        strings.insert(s.into(), Str { id, last_used: ts });
        added.push(s.into());
        Ok(id)
    };
    let mut del_grp = tx.prepare_cached("DELETE FROM group_minute WHERE ts = ?1")?;
    let mut del_prc = tx.prepare_cached("DELETE FROM process_minute WHERE ts = ?1")?;
    for m in minutes {
        let x = &m.machine;
        // A minute written before in part (Analyze) is replaced whole: drop
        // the rows of groups only its first part had.
        del_grp.execute([x.ts])?;
        del_prc.execute([x.ts])?;
        mach.execute(params![
            x.ts,
            x.used_avg_mib,
            x.used_max_mib,
            x.total_mib,
            x.pressure_max,
            x.avail_min_pct,
            x.cpu_avg_pm,
            x.cpu_max_pm
        ])?;
        for g in &m.groups {
            grp.execute(params![g.ts, g.gid, g.mib_avg, g.mib_max, g.cpu_avg_pm])?;
        }
        for p in &m.procs {
            let name_id = intern(&p.name, p.ts)?;
            let args_id = if p.args.is_empty() { None } else { Some(intern(&p.args, p.ts)?) };
            prc.execute(params![p.ts, p.gid, p.rank, p.pid, name_id, args_id, p.mib_max, p.cpu_avg_pm])?;
        }
    }

    let mut ep = tx.prepare_cached(
        "INSERT INTO pressure_episodes(start, end, level, top_group_id) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(start) DO UPDATE SET end = excluded.end, level = excluded.level, top_group_id = excluded.top_group_id",
    )?;
    for e in episodes {
        ep.execute(params![e.start, e.end, e.level as u8, e.top_gid])?;
    }
    tx.prepare_cached("INSERT OR REPLACE INTO meta(key, value) VALUES ('last_flush_ts', ?1)")?.execute([now])?;
    Ok(())
}

/// Rolls the minutes of `[from, until)` (whole hours) into the `*_hour`
/// tables. Returns the number of hours written.
fn rollup(tx: &Transaction, from: i64, until: i64) -> rusqlite::Result<u32> {
    let hours = tx.prepare_cached(
        "INSERT OR REPLACE INTO machine_hour
         SELECT ts / 3600 * 3600, CAST(round(avg(used_avg_mib)) AS INTEGER), max(used_max_mib), max(total_mib),
                max(pressure_max), min(avail_min_pct), CAST(round(avg(cpu_avg_pm)) AS INTEGER), max(cpu_max_pm), count(*)
           FROM machine_minute WHERE ts >= ?1 AND ts < ?2 GROUP BY ts / 3600",
    )?
    .execute([from, until])?;
    tx.prepare_cached(
        "INSERT OR REPLACE INTO group_hour
         SELECT ts / 3600 * 3600, group_id, CAST(round(avg(mib_avg)) AS INTEGER), max(mib_max),
                CAST(round(avg(cpu_avg_pm)) AS INTEGER), count(*)
           FROM group_minute WHERE ts >= ?1 AND ts < ?2 GROUP BY ts / 3600, group_id",
    )?
    .execute([from, until])?;
    // groups.last_ts is not rewritten at each flush: refresh it here.
    tx.prepare_cached(
        "UPDATE groups SET last_ts = m.mx
           FROM (SELECT group_id, max(ts) AS mx FROM group_minute WHERE ts >= ?1 AND ts < ?2 GROUP BY group_id) AS m
          WHERE groups.id = m.group_id AND m.mx > groups.last_ts",
    )?
    .execute([from, until])?;
    // One process = (pid, name, args) within the hour; keep the 3 biggest by
    // their max minute footprint.
    tx.prepare_cached(
        "WITH p AS (
           SELECT ts / 3600 * 3600 AS h, group_id, pid, name_id, args_id, max(mib_max) AS mx,
                  CAST(round(avg(cpu_avg_pm)) AS INTEGER) AS cpu
             FROM process_minute WHERE ts >= ?1 AND ts < ?2
            GROUP BY h, group_id, pid, name_id, args_id),
         r AS (
           SELECT *, row_number() OVER (PARTITION BY h, group_id ORDER BY mx DESC, pid) AS rk FROM p)
         INSERT OR REPLACE INTO process_hour SELECT h, group_id, rk, pid, name_id, args_id, mx, cpu FROM r WHERE rk <= 3",
    )?
    .execute([from, until])?;
    Ok(hours as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProcMinute;
    use std::sync::Arc;

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("devload-store-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            TmpDir(p)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const T0: i64 = 1_795_600_800; // a whole hour

    fn group(id: Gid, kind: GroupKind, name: &str, parent: Option<Gid>) -> Group {
        Group { id, kind, name: name.into(), path: format!("/p/{name}"), parent }
    }

    #[allow(clippy::too_many_arguments)]
    fn proc_(ts: i64, gid: Gid, rank: u8, pid: i32, name: &str, args: &str, mib: u32, cpu: u32) -> ProcMinute {
        ProcMinute { ts, gid, rank, pid, name: Arc::from(name), args: Arc::from(args), mib_max: mib, cpu_avg_pm: cpu }
    }

    fn minute(ts: i64, used: u32) -> Minute {
        Minute {
            machine: MachineMinute {
                ts,
                used_avg_mib: used,
                used_max_mib: used + 100,
                total_mib: 24576,
                pressure_max: 0,
                avail_min_pct: 40,
                cpu_avg_pm: 500,
                cpu_max_pm: 900,
            },
            groups: vec![],
            procs: vec![],
        }
    }

    fn count(s: &Store, table: &str) -> i64 {
        s.conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn schema_and_pragmas() {
        let d = TmpDir::new("schema");
        let s = Store::open(&d.0.join("nested")).unwrap();
        assert!(s.path().ends_with("nested/devload.sqlite"));
        let q = |sql: &str| -> i64 { s.conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(q("PRAGMA auto_vacuum"), 2);
        assert_eq!(q("PRAGMA page_size"), 4096);
        assert_eq!(q("PRAGMA synchronous"), 1);
        assert_eq!(q("PRAGMA wal_autocheckpoint"), 0);
        let mode: String = s.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(s.meta("schema_version").unwrap(), Some(1));
        assert_eq!(s.meta("tick_seconds").unwrap(), Some(5));
        assert_eq!(s.meta("retention_hour_days").unwrap(), Some(90));
        // series tables WITHOUT ROWID, no secondary index, every column commented
        let mut st = s.conn.prepare("SELECT name, sql FROM sqlite_master").unwrap();
        let all: Vec<(String, Option<String>)> =
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect();
        for (name, sql) in &all {
            assert!(!name.starts_with("sqlite_autoindex") || sql.is_none(), "{name}");
            let Some(sql) = sql else { continue };
            assert!(sql.starts_with("CREATE TABLE"), "unexpected object {name}");
            if name.contains("minute") || name.contains("hour") || name == "pressure_episodes" {
                assert!(sql.contains("WITHOUT ROWID"), "{name}");
            }
            for line in sql.lines().skip(1) {
                let l = line.trim();
                if l.is_empty() || l.starts_with("--") || l.starts_with(')') || l.starts_with("PRIMARY KEY") {
                    continue;
                }
                assert!(l.contains("--"), "column without comment in {name}: {l}");
            }
        }
        assert_eq!(all.iter().filter(|(_, s)| s.is_some()).count(), 10);
    }

    #[test]
    fn flush_and_read_back() {
        let d = TmpDir::new("flush");
        let mut s = Store::open(&d.0).unwrap();
        let groups = [group(1, GroupKind::Worktree, "wt", None), group(2, GroupKind::Docker, "pg", Some(1))];
        let mut m0 = minute(T0, 8000);
        m0.groups = vec![
            GroupMinute { ts: T0, gid: 1, mib_avg: 300, mib_max: 350, cpu_avg_pm: 20 },
            GroupMinute { ts: T0, gid: 2, mib_avg: 145, mib_max: 150, cpu_avg_pm: 5 },
        ];
        m0.procs = vec![proc_(T0, 1, 1, 100, "node", "server.js --port 3000", 300, 10), proc_(T0, 2, 1, 200, "postgres", "", 145, 5)];
        let mut m1 = minute(T0 + 60, 8100);
        m1.groups = vec![GroupMinute { ts: T0 + 60, gid: 1, mib_avg: 310, mib_max: 360, cpu_avg_pm: 30 }];
        m1.procs = vec![proc_(T0 + 60, 1, 1, 100, "node", "server.js --port 3000", 310, 12)];
        let ep = Episode { start: T0 + 30, end: None, level: Pressure::Tight, top_gid: Some(1) };
        s.flush(T0 + 120, &groups, &[m0.clone(), m1.clone()], &[ep]).unwrap();

        assert_eq!(s.machine_minutes(T0, T0 + 120).unwrap(), vec![m0.machine, m1.machine]);
        assert_eq!(s.machine_minutes(T0 + 60, T0 + 61).unwrap(), vec![m1.machine]);
        let gm = s.group_minutes(T0, T0 + 3600).unwrap();
        assert_eq!(gm.len(), 3);
        assert_eq!(gm[0..2], m0.groups[..]);
        assert_eq!(s.groups().unwrap(), groups.to_vec());
        assert_eq!(s.meta("last_flush_ts").unwrap(), Some(T0 + 120));
        assert_eq!(count(&s, "strings"), 3); // node, args, postgres ("" args -> NULL)
        let nulls: i64 = s.conn.query_row("SELECT count(*) FROM process_minute WHERE args_id IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(nulls, 1);
        let (fts, lts): (i64, i64) = s.conn.query_row("SELECT first_ts, last_ts FROM groups WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((fts, lts), (T0, T0 + 60));

        // episode re-sent when it ends; same minute re-flushed is replaced
        let ep2 = Episode { end: Some(T0 + 400), level: Pressure::Critical, ..ep };
        let mut m1b = m1.clone();
        m1b.machine.used_avg_mib = 9999;
        s.flush(T0 + 420, &groups, &[m1b.clone()], &[ep2]).unwrap();
        assert_eq!(s.machine_minutes(T0 + 60, T0 + 120).unwrap(), vec![m1b.machine]);
        let e: (i64, Option<i64>, u8, Option<u32>) = s
            .conn
            .query_row("SELECT start, end, level, top_group_id FROM pressure_episodes", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        assert_eq!(e, (T0 + 30, Some(T0 + 400), 2, Some(1)));
        assert_eq!(count(&s, "strings"), 3);
    }

    #[test]
    fn a_minute_written_again_replaces_all_its_rows() {
        let d = TmpDir::new("rewrite");
        let mut s = Store::open(&d.0).unwrap();
        let groups = [group(1, GroupKind::Worktree, "wt", None), group(2, GroupKind::App, "app", None)];
        // First part of the minute (Analyze): groups 1 and 2.
        let mut part = minute(T0, 8000);
        part.groups = vec![
            GroupMinute { ts: T0, gid: 1, mib_avg: 300, mib_max: 350, cpu_avg_pm: 20 },
            GroupMinute { ts: T0, gid: 2, mib_avg: 100, mib_max: 100, cpu_avg_pm: 5 },
        ];
        part.procs = vec![proc_(T0, 1, 1, 100, "node", "", 300, 10), proc_(T0, 2, 1, 200, "app", "", 100, 5)];
        s.flush(T0 + 30, &groups, &[part], &[]).unwrap();
        // The whole minute, without group 2: its rows of that minute go.
        let mut full = minute(T0, 8100);
        full.groups = vec![GroupMinute { ts: T0, gid: 1, mib_avg: 310, mib_max: 360, cpu_avg_pm: 20 }];
        full.procs = vec![proc_(T0, 1, 1, 100, "node", "", 310, 10)];
        s.flush(T0 + 60, &groups, &[full.clone()], &[]).unwrap();
        assert_eq!(s.group_minutes(T0, T0 + 60).unwrap(), full.groups);
        assert_eq!(count(&s, "process_minute"), 1);
    }

    #[test]
    fn open_closes_an_episode_left_open_by_a_crash() {
        let d = TmpDir::new("orphan-ep");
        {
            let mut s = Store::open(&d.0).unwrap();
            let ep = Episode { start: T0, end: None, level: Pressure::Tight, top_gid: None };
            s.flush(T0 + 300, &[], &[], &[ep]).unwrap();
        }
        let s = Store::open(&d.0).unwrap();
        let closed = Episode { start: T0, end: Some(T0 + 300), level: Pressure::Tight, top_gid: None };
        assert_eq!(s.episodes().unwrap(), [closed]);
        assert_eq!(s.last_episode().unwrap(), Some(closed));
    }

    #[test]
    fn last_episode_is_the_latest_start() {
        let d = TmpDir::new("last-ep");
        let mut s = Store::open(&d.0).unwrap();
        assert_eq!(s.last_episode().unwrap(), None);
        let a = Episode { start: T0, end: Some(T0 + 60), level: Pressure::Critical, top_gid: Some(1) };
        let b = Episode { start: T0 + 600, end: Some(T0 + 700), level: Pressure::Tight, top_gid: None };
        s.flush(T0 + 700, &[], &[], &[b, a]).unwrap();
        assert_eq!(s.last_episode().unwrap(), Some(b));
    }

    #[test]
    fn group_upsert() {
        let d = TmpDir::new("upsert");
        let mut s = Store::open(&d.0).unwrap();
        let mut g = group(5, GroupKind::Docker, "stack", None);
        s.flush(T0, &[g.clone()], &[], &[]).unwrap();
        let ts = |s: &Store| -> (i64, i64) { s.conn.query_row("SELECT first_ts, last_ts FROM groups WHERE id = 5", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap() };
        assert_eq!(ts(&s), (T0, T0));
        g.name = "renamed".into();
        g.parent = Some(1);
        let mut m = minute(T0 + 600, 1);
        m.groups = vec![GroupMinute { ts: T0 + 600, gid: 5, mib_avg: 60, mib_max: 60, cpu_avg_pm: 0 }];
        s.flush(T0 + 700, &[g.clone()], &[m], &[]).unwrap();
        assert_eq!(s.groups().unwrap(), vec![g.clone()]);
        assert_eq!(ts(&s), (T0, T0 + 600));
        // no rows: last_ts unchanged; older rows never move it back
        s.flush(T0 + 900, &[g.clone()], &[], &[]).unwrap();
        assert_eq!(ts(&s), (T0, T0 + 600));
        let mut m = minute(T0 + 120, 1);
        m.groups = vec![GroupMinute { ts: T0 + 120, gid: 5, mib_avg: 60, mib_max: 60, cpu_avg_pm: 0 }];
        s.flush(T0 + 950, &[g], &[m], &[]).unwrap();
        assert_eq!(ts(&s), (T0, T0 + 600));
    }

    #[test]
    fn rollup_correctness() {
        let d = TmpDir::new("rollup");
        let mut s = Store::open(&d.0).unwrap();
        let groups = [group(1, GroupKind::Worktree, "a", None), group(2, GroupKind::App, "b", None)];
        let mut mins = vec![];
        // Hour T0: 60 minutes. Group 1 present all hour, group 2 only 30 minutes.
        for i in 0..60i64 {
            let ts = T0 + i * 60;
            let mut m = minute(ts, 1000 + i as u32);
            m.machine.pressure_max = if i == 10 { 2 } else { 0 };
            m.machine.avail_min_pct = if i == 20 { 7 } else { 40 };
            m.machine.cpu_max_pm = 900 + i as u32;
            m.groups.push(GroupMinute { ts, gid: 1, mib_avg: 100, mib_max: 100 + i as u32, cpu_avg_pm: 10 });
            if i < 30 {
                m.groups.push(GroupMinute { ts, gid: 2, mib_avg: 200, mib_max: 210, cpu_avg_pm: 40 });
            }
            // 4 distinct processes rotate through group 1's top 3
            let pids: [(i32, u32); 3] = match i % 3 {
                0 => [(10, 500), (11, 300), (12, 100)],
                1 => [(10, 450), (13, 400), (11, 50)],
                _ => [(11, 250), (12, 240), (10, 230)],
            };
            for (r, (pid, mib)) in pids.iter().enumerate() {
                m.procs.push(proc_(ts, 1, r as u8 + 1, *pid, "p", &format!("args{pid}"), *mib, *pid as u32));
            }
            mins.push(m);
        }
        // and the first minute of the next hour (so hour T0 is complete)
        mins.push(minute(T0 + 3600, 5000));
        s.flush(T0 + 3700, &groups, &mins, &[]).unwrap();
        let rep = s.maintenance(T0 + 3700).unwrap();
        assert_eq!(rep.rolled_hours, 1);
        assert_eq!(s.meta("rollup_done_until").unwrap(), Some(T0 + 3600));
        let mh: (i64, i64, i64, i64, i64, i64, i64, i64) = s
            .conn
            .query_row(
                "SELECT used_avg_mib, used_max_mib, total_mib, pressure_max, avail_min_pct, cpu_avg_pm, cpu_max_pm, minutes FROM machine_hour WHERE ts = ?1",
                [T0],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
            )
            .unwrap();
        // avg(1000..1059) = 1029.5 -> 1030 ; max = 1059 + 100
        assert_eq!(mh, (1030, 1159, 24576, 2, 7, 500, 959, 60));
        let gh: Vec<(i64, i64, i64, i64, i64)> = s
            .conn
            .prepare("SELECT group_id, mib_avg, mib_max, cpu_avg_pm, minutes FROM group_hour WHERE ts = ?1 ORDER BY group_id")
            .unwrap()
            .query_map([T0], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(gh, vec![(1, 100, 159, 10, 60), (2, 200, 210, 40, 30)]);
        let ph: Vec<(i64, i64, i64, i64)> = s
            .conn
            .prepare("SELECT rank, pid, mib_max, cpu_avg_pm FROM process_hour WHERE ts = ?1 AND group_id = 1 ORDER BY rank")
            .unwrap()
            .query_map([T0], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(ph, vec![(1, 10, 500, 10), (2, 13, 400, 13), (3, 11, 300, 11)]);
        // Partial hour T0+3600 not rolled yet; a second pass rolls nothing.
        assert_eq!(s.maintenance(T0 + 3800).unwrap().rolled_hours, 0);
        assert_eq!(count(&s, "machine_hour"), 1);
        // Minute rows are still there (7-day retention).
        assert_eq!(count(&s, "machine_minute"), 61);
    }

    #[test]
    fn rollup_across_a_gap() {
        let d = TmpDir::new("gap");
        let mut s = Store::open(&d.0).unwrap();
        // 10:00-10:29 written, app stopped, restarted 3 h later.
        let mins: Vec<Minute> = (0..30).map(|i| minute(T0 + i * 60, 100)).collect();
        s.flush(T0 + 1800, &[], &mins, &[]).unwrap();
        assert_eq!(s.maintenance(T0 + 1800).unwrap().rolled_hours, 0);
        s.flush(T0 + 3 * 3600 + 120, &[], &[minute(T0 + 3 * 3600, 100)], &[]).unwrap();
        assert_eq!(s.maintenance(T0 + 3 * 3600 + 120).unwrap().rolled_hours, 1);
        let n: i64 = s.conn.query_row("SELECT minutes FROM machine_hour WHERE ts = ?1", [T0], |r| r.get(0)).unwrap();
        assert_eq!(n, 30);
        assert_eq!(s.meta("rollup_done_until").unwrap(), Some(T0 + 3 * 3600));
    }

    #[test]
    fn retention() {
        let d = TmpDir::new("retention");
        let mut s = Store::open(&d.0).unwrap();
        let g = [group(1, GroupKind::App, "old", None), group(2, GroupKind::App, "new", None)];
        let old = T0 - 100 * DAY;
        let mut m_old = minute(old, 1);
        m_old.groups = vec![GroupMinute { ts: old, gid: 1, mib_avg: 60, mib_max: 60, cpu_avg_pm: 0 }];
        m_old.procs = vec![proc_(old, 1, 1, 1, "oldproc", "old args", 60, 0)];
        let mid = T0 - 10 * DAY;
        let mut m_mid = minute(mid, 1);
        m_mid.groups = vec![GroupMinute { ts: mid, gid: 2, mib_avg: 60, mib_max: 60, cpu_avg_pm: 0 }];
        m_mid.procs = vec![proc_(mid, 2, 1, 2, "midproc", "mid args", 60, 0)];
        let mut m_new = minute(T0, 1);
        m_new.groups = vec![GroupMinute { ts: T0, gid: 2, mib_avg: 60, mib_max: 60, cpu_avg_pm: 0 }];
        m_new.procs = vec![proc_(T0, 2, 1, 3, "newproc", "", 60, 0)];
        let eps = [
            Episode { start: old, end: Some(old + 60), level: Pressure::Tight, top_gid: None },
            Episode { start: mid, end: Some(mid + 60), level: Pressure::Tight, top_gid: Some(2) },
        ];
        s.flush(old + 120, &g, &[m_old, m_mid, m_new], &eps).unwrap();
        let rep = s.maintenance(T0 + 120).unwrap();
        assert_eq!(rep.purged_groups, vec![1], "the engine is told which groups went");
        // minutes: only T0 kept; hours: old one gone (100 d), mid kept; episodes: mid kept
        let ts_of = |t: &str| -> Vec<i64> {
            s.conn.prepare(&format!("SELECT ts FROM {t} ORDER BY ts")).unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        assert_eq!(ts_of("machine_minute"), vec![T0]);
        assert_eq!(ts_of("group_minute"), vec![T0]);
        assert_eq!(ts_of("process_minute"), vec![T0]);
        assert_eq!(ts_of("machine_hour"), vec![mid / 3600 * 3600]);
        assert_eq!(ts_of("group_hour"), vec![mid / 3600 * 3600]);
        assert_eq!(ts_of("process_hour"), vec![mid / 3600 * 3600]);
        assert_eq!(count(&s, "pressure_episodes"), 1);
        // daily purge: strings of the old process gone, group 1 gone (no row for 90 d)
        let texts: Vec<String> = s.conn.prepare("SELECT text FROM strings ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(texts, vec!["midproc", "mid args", "newproc"]);
        assert_eq!(s.string_count(), 3);
        assert_eq!(s.groups().unwrap().iter().map(|g| g.id).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn string_dedup_and_purge() {
        let d = TmpDir::new("strings");
        let mut s = Store::open(&d.0).unwrap();
        let g = [group(1, GroupKind::App, "a", None)];
        let mk = |ts: i64, args: &str| {
            let mut m = minute(ts, 1);
            m.groups = vec![GroupMinute { ts, gid: 1, mib_avg: 60, mib_max: 60, cpu_avg_pm: 0 }];
            m.procs = vec![proc_(ts, 1, 1, 1, "node", args, 60, 0), proc_(ts, 1, 2, 2, "node", "b", 55, 0)];
            m
        };
        let start = T0 - 9 * DAY;
        s.flush(start + 60, &g, &[mk(start, "a"), mk(start + 60, "a")], &[]).unwrap();
        assert_eq!(count(&s, "strings"), 3);
        s.flush(T0 + 60, &g, &[mk(T0, "c")], &[]).unwrap();
        assert_eq!(count(&s, "strings"), 4); // node, a, b, c
        let rep = s.maintenance(T0 + 120).unwrap();
        // "a" survives through process_hour (90 d); nothing to purge yet
        assert_eq!(rep.strings_purged, 0);
        // now drop process_hour rows by hand to simulate their retention
        s.conn.execute("DELETE FROM process_hour", []).unwrap();
        s.conn.execute("UPDATE meta SET value = 0 WHERE key = 'strings_purged_ts'", []).unwrap();
        let rep = s.maintenance(T0 + 180).unwrap();
        assert_eq!(rep.strings_purged, 1);
        assert_eq!(s.string_count(), 3);
        // "a" comes back as a new row, not a dangling id
        s.flush(T0 + 240, &g, &[mk(T0 + 180, "a")], &[]).unwrap();
        let a_ok: i64 = s
            .conn
            .query_row("SELECT count(*) FROM process_minute p JOIN strings s ON s.id = p.args_id WHERE p.ts = ?1 AND s.text = 'a'", [T0 + 180], |r| r.get(0))
            .unwrap();
        assert_eq!(a_ok, 1);
        assert_eq!(count(&s, "strings"), 4);
        // purge only once a day
        s.conn.execute("DELETE FROM process_minute", []).unwrap();
        assert_eq!(s.maintenance(T0 + 3600).unwrap().strings_purged, 0);
    }

    #[test]
    fn failed_flush_forgets_its_strings() {
        let d = TmpDir::new("rollback");
        let mut s = Store::open(&d.0).unwrap();
        let mut m = minute(T0, 1);
        m.procs = vec![proc_(T0, 1, 1, 1, "fresh", "x", 1, 0)];
        s.conn.execute_batch("CREATE TEMP TRIGGER boom BEFORE INSERT ON process_minute BEGIN SELECT RAISE(ABORT, 'boom'); END;").unwrap();
        assert!(s.flush(T0 + 60, &[], &[m.clone()], &[]).is_err());
        assert_eq!(s.string_count(), 0);
        s.conn.execute_batch("DROP TRIGGER boom;").unwrap();
        s.flush(T0 + 60, &[], &[m], &[]).unwrap();
        assert_eq!(s.string_count(), 2);
        assert_eq!(count(&s, "strings"), 2);
    }

    #[test]
    fn reopen_keeps_ids_and_strings() {
        let d = TmpDir::new("reopen");
        let groups = [group(1, GroupKind::Worktree, "wt", None), group(7, GroupKind::Docker, "pg", Some(1))];
        {
            let mut s = Store::open(&d.0).unwrap();
            let mut m = minute(T0, 1);
            m.procs = vec![proc_(T0, 1, 1, 1, "node", "x", 1, 0)];
            s.flush(T0 + 60, &groups, &[m], &[]).unwrap();
        }
        let mut s = Store::open(&d.0).unwrap();
        assert_eq!(s.groups().unwrap(), groups.to_vec());
        assert_eq!(s.string_count(), 2);
        assert_eq!(s.meta("last_flush_ts").unwrap(), Some(T0 + 60));
        let mut m = minute(T0 + 60, 1);
        m.procs = vec![proc_(T0 + 60, 1, 1, 1, "node", "y", 1, 0)];
        s.flush(T0 + 120, &groups, &[m], &[]).unwrap();
        assert_eq!(count(&s, "strings"), 3);
        let ids: Vec<i64> = s.conn.prepare("SELECT id FROM strings ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        let q = |sql: &str| -> i64 { s.conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(q("PRAGMA auto_vacuum"), 2);
        assert_eq!(q("PRAGMA wal_autocheckpoint"), 0);
    }

    #[test]
    fn unchanged_groups_are_not_rewritten_but_last_ts_follows_hourly() {
        let d = TmpDir::new("lastts");
        let mut s = Store::open(&d.0).unwrap();
        let g = [group(3, GroupKind::App, "a", None)];
        let row = |ts: i64| {
            let mut m = minute(ts, 1);
            m.groups = vec![GroupMinute { ts, gid: 3, mib_avg: 60, mib_max: 60, cpu_avg_pm: 0 }];
            m
        };
        s.flush(T0 + 60, &g, &[row(T0)], &[]).unwrap();
        let changes = |s: &Store| -> u64 { s.conn.total_changes() };
        let before = changes(&s);
        s.flush(T0 + 120, &g, &[row(T0 + 60)], &[]).unwrap();
        // machine row + group row + meta only: the group itself was not rewritten
        assert_eq!(changes(&s) - before, 3);
        let last = |s: &Store| -> i64 { s.conn.query_row("SELECT last_ts FROM groups WHERE id = 3", [], |r| r.get(0)).unwrap() };
        assert_eq!(last(&s), T0);
        s.flush(T0 + 3660, &g, &[row(T0 + 1800), row(T0 + 3600)], &[]).unwrap();
        s.maintenance(T0 + 3660).unwrap();
        assert_eq!(last(&s), T0 + 1800);
        // a rename is written at once
        let renamed = [Group { name: "b".into(), ..g[0].clone() }];
        s.flush(T0 + 3720, &renamed, &[], &[]).unwrap();
        assert_eq!(s.groups().unwrap()[0].name, "b");
        drop(s);
        // reopen: the RAM copy is reloaded, nothing rewritten
        let mut s = Store::open(&d.0).unwrap();
        let before = changes(&s);
        s.flush(T0 + 3780, &renamed, &[], &[]).unwrap();
        assert_eq!(changes(&s) - before, 1);
    }

    #[test]
    fn purge_after_reopen_keeps_strings_of_the_previous_run() {
        let d = TmpDir::new("purge-reopen");
        {
            let mut s = Store::open(&d.0).unwrap();
            let mut m = minute(T0, 1);
            m.procs = vec![proc_(T0, 1, 1, 1, "node", "old run", 1, 0)];
            s.flush(T0 + 60, &[], &[m], &[]).unwrap();
            // a string that nothing references any more
            s.conn.execute("INSERT INTO strings(id, text) VALUES (99, 'orphan')", []).unwrap();
        }
        let mut s = Store::open(&d.0).unwrap();
        let rep = s.maintenance(T0 + 120).unwrap();
        assert_eq!(rep.strings_purged, 1);
        let texts: Vec<String> = s.conn.prepare("SELECT text FROM strings ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(texts, vec!["node", "old run"]);
        assert_eq!(s.string_count(), 2);
    }
}
