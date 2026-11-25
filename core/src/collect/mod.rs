//! Collection: machine metrics, processes, and their classification.
//!
//! One `Collector::tick` every 5 s. A process already seen costs one
//! `proc_pid_rusage`; its facts (parent, exe, cwd, argv, filtered env) are
//! read once, at first sight. Never a subprocess.

pub mod agents;
pub mod classify;
pub mod label;
pub mod machine;
pub mod repo;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agents::{Agent, ClaudeRegistry, ENV_KEYS, RuntimeKind};
use classify::{Class, Classifier, Facts, ProcIn, Runtime, SessionSource};
use machine::{CpuMeter, PressureState};
use repo::RepoIndex;

use crate::model::{MachineTick, Pressure};
use crate::sys::{self, Timebase};

/// Codex lock files are re-read this often.
const CODEX_LOCK_EVERY: Duration = Duration::from_secs(30);
/// A session is idle below this CPU (per-mille of one core) over a minute...
const IDLE_CPU_PM: u32 = 10;
/// ...and when none of its processes started less than this long ago.
const IDLE_YOUNG_SECS: u64 = 60;
const IDLE_WINDOW: usize = 12;

pub struct Config {
    pub home: String,
    pub claude_home: PathBuf,
    pub codex_lock_dir: String,
}

impl Config {
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let claude_home = std::env::var("CLAUDE_CONFIG_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&home).join(".claude"));
        let codex_lock_dir = agents::codex_lock_dir(std::env::var("CODEX_HOME").ok().as_deref(), &home);
        Config { home, claude_home, codex_lock_dir }
    }
}

/// A process as the collector remembers it between ticks.
struct Known {
    start_abs: u64,
    start_sec: u64,
    ppid: i32,
    readable: bool,
    facts: Arc<Facts>,
    prev_cpu_mach: u64,
    /// `ps -o lstart` form of the start time, for claude runtimes only.
    lstart: Option<String>,
}

/// Measured values of one live process at this tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    pub footprint: u64,
    pub cpu_pm: u32,
    pub start_sec: u64,
}

/// A session of an agent, for the row detail ("2 Claude sessions (1 idle)").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub agent: Agent,
    pub idle: bool,
}

/// Result of one tick.
#[derive(Default)]
pub struct Scan {
    pub machine: MachineTick,
    /// Since when the displayed pressure colour holds.
    pub pressure_since: i64,
    pub ncpu: u32,
    pub procs: Vec<ProcIn>,
    pub samples: Vec<Sample>,
    pub classes: Vec<Class>,
    pub runtimes: Vec<Runtime>,
    /// Per runtime: its sessions with their idle state.
    pub sessions: Vec<Vec<SessionInfo>>,
    pub unreadable: u32,
    /// CPU time this tick cost the collector, nanoseconds.
    pub cost_ns: u64,
}

pub struct Collector {
    cfg: Config,
    tb: Timebase,
    ncpu: u32,
    pids: Vec<i32>,
    args_buf: Vec<u8>,
    path_buf: Box<[u8; sys::PATH_MAX]>,
    vnode: Box<libc::proc_vnodepathinfo>,
    known: HashMap<i32, Known>,
    repos: RepoIndex,
    claude: ClaudeRegistry,
    codex_locks: HashMap<i32, Vec<String>>,
    codex_scanned: Option<Instant>,
    /// CPU history per runtime identity (pid, start), for idle detection.
    runtime_cpu: HashMap<(i32, u64), VecDeque<u32>>,
    pressure: PressureState,
    cpu: CpuMeter,
    last_tick: Option<Instant>,
}

impl Collector {
    pub fn new(cfg: Config) -> Self {
        let repos = RepoIndex::new(cfg.home.clone());
        let claude = ClaudeRegistry::new(&cfg.claude_home);
        Collector {
            cfg,
            tb: Timebase::host(),
            ncpu: machine::logical_cpus(),
            pids: Vec::new(),
            args_buf: vec![0u8; 64 * 1024],
            path_buf: Box::new([0u8; sys::PATH_MAX]),
            vnode: Box::new(unsafe { std::mem::zeroed() }),
            known: HashMap::new(),
            repos,
            claude,
            codex_locks: HashMap::new(),
            codex_scanned: None,
            runtime_cpu: HashMap::new(),
            pressure: PressureState::default(),
            cpu: CpuMeter::default(),
            last_tick: None,
        }
    }

    /// Tests: a collector whose repo lookups hit a fake file system.
    #[cfg(test)]
    pub(crate) fn with_repos(cfg: Config, repos: RepoIndex) -> Self {
        Collector { repos, ..Collector::new(cfg) }
    }

    pub fn repos(&mut self) -> &mut RepoIndex {
        &mut self.repos
    }

    pub fn ncpu(&self) -> u32 {
        self.ncpu
    }

    /// One tick. `now` is wall-clock Unix seconds.
    pub fn tick(&mut self, now: i64) -> Scan {
        let cpu_start = sys::thread_cpu_ns();
        let mono = Instant::now();
        let elapsed_ns = self.last_tick.map(|t| mono.duration_since(t).as_nanos() as u64).unwrap_or(0);
        self.last_tick = Some(mono);

        let machine = self.machine(now);
        self.claude.refresh();

        let n = sys::list_pids(&mut self.pids);
        let mut procs = Vec::with_capacity(n);
        let mut samples = Vec::with_capacity(n);
        let mut unreadable = 0u32;
        let mut alive: Vec<i32> = Vec::with_capacity(n);
        let pids = std::mem::take(&mut self.pids);
        let tb = self.tb;
        for &pid in &pids[..n] {
            if pid <= 0 {
                continue;
            }
            match sys::rusage(pid) {
                Ok(r) => {
                    let Some(k) = self.known_for(pid, r.start_abs, true) else { continue };
                    if r.footprint == 0 && sys::bsd_info(pid).is_none_or(|b| b.zombie) {
                        continue; // zombie
                    }
                    let d_ns = tb.to_ns(r.cpu_mach.saturating_sub(k.prev_cpu_mach));
                    let cpu_pm = if elapsed_ns > 0 && k.prev_cpu_mach > 0 {
                        (d_ns as u128 * 1000 / elapsed_ns as u128).min(u32::MAX as u128) as u32
                    } else {
                        0
                    };
                    k.prev_cpu_mach = r.cpu_mach;
                    alive.push(pid);
                    procs.push(ProcIn { pid, ppid: k.ppid, start_abs: k.start_abs, readable: true, facts: k.facts.clone() });
                    samples.push(Sample { footprint: r.footprint, cpu_pm, start_sec: k.start_sec });
                }
                Err(libc::EPERM) => {
                    unreadable += 1;
                    alive.push(pid);
                    // Identity unknown (no rusage): keyed by pid only.
                    if let Some(k) = self.known_for(pid, 0, false) {
                        procs.push(ProcIn { pid, ppid: k.ppid, start_abs: 0, readable: false, facts: k.facts.clone() });
                        samples.push(Sample::default());
                    }
                }
                Err(_) => {} // gone
            }
        }
        self.pids = pids;
        alive.sort_unstable();
        self.known.retain(|pid, _| alive.binary_search(pid).is_ok());

        self.refresh_codex_locks(&procs);
        let sessions = LiveSessions {
            claude: &self.claude,
            known: &self.known,
            codex: &self.codex_locks,
        };
        let exe_of = |pid: i32| {
            let mut buf = [0u8; sys::PATH_MAX];
            sys::pid_path(pid, &mut buf)
        };
        let (classes, runtimes) =
            Classifier { repos: &mut self.repos, sessions: &sessions, exe_of: &exe_of }.run(&procs);
        let sessions = self.session_states(now, &procs, &samples, &classes, &runtimes);

        Scan {
            pressure_since: self.pressure.since(),
            machine,
            ncpu: self.ncpu,
            procs,
            samples,
            classes,
            runtimes,
            sessions,
            unreadable,
            cost_ns: sys::thread_cpu_ns().saturating_sub(cpu_start),
        }
    }

    fn machine(&mut self, now: i64) -> MachineTick {
        let total = sys::total_memory();
        let used = sys::vm_stats().map(|v| v.used_bytes()).unwrap_or(0).min(total);
        let avail = sys::memory_available_pct();
        let pressure = self.pressure.update(now, sys::kernel_pressure_level(), avail);
        let busy = sys::cpu_ticks().map(|t| self.cpu.sample(t)).unwrap_or(0.0);
        MachineTick {
            ts: now,
            used,
            total,
            pressure,
            avail_pct: avail.unwrap_or(100).clamp(0, 100) as u8,
            cpu_pm: (busy * self.ncpu as f64 * 1000.0).round() as u32,
        }
    }

    /// The remembered process for (pid, start), reading its facts at first
    /// sight. `None` if it vanished meanwhile.
    fn known_for(&mut self, pid: i32, start_abs: u64, readable: bool) -> Option<&mut Known> {
        let fresh = match self.known.get(&pid) {
            Some(k) => k.start_abs != start_abs || k.readable != readable,
            None => true,
        };
        if fresh {
            let k = self.read_facts(pid, start_abs, readable)?;
            self.known.insert(pid, k);
        }
        self.known.get_mut(&pid)
    }

    fn read_facts(&mut self, pid: i32, start_abs: u64, readable: bool) -> Option<Known> {
        let b = sys::bsd_info(pid)?;
        if b.zombie {
            return None;
        }
        let exe = sys::pid_path(pid, &mut self.path_buf);
        if !readable {
            let name: Arc<str> = exe.as_deref().map(repo::basename).unwrap_or(&b.name).into();
            let facts = Facts { name: name.clone(), exe, label: name, ..Facts::default() };
            return Some(Known {
                start_abs,
                start_sec: b.start_sec,
                ppid: b.ppid,
                readable,
                facts: Arc::new(facts),
                prev_cpu_mach: 0,
                lstart: None,
            });
        }
        let cwd = sys::cwd(pid, &mut self.vnode);
        let args = sys::proc_args(pid, &mut self.args_buf, &ENV_KEYS).unwrap_or_default();
        let runtime = agents::runtime_of(exe.as_deref(), &args.argv);
        let marker = agents::session_marker(&args.env).map(|(a, id)| (a, id.to_string()));
        let responsible = sys::responsible_pid(pid).filter(|&r| r != pid);
        let label = label::label(&b.name, &args.argv);
        let lstart = matches!(runtime, Some((Agent::Claude, _))).then(|| agents::lstart(b.start_sec));
        let facts = Facts {
            name: b.name.as_str().into(),
            exe,
            cwd,
            argv: args.argv,
            env: args.env,
            responsible,
            runtime,
            marker,
            label: label.into(),
        };
        Some(Known { start_abs, start_sec: b.start_sec, ppid: b.ppid, readable, facts: Arc::new(facts), prev_cpu_mach: 0, lstart })
    }

    fn refresh_codex_locks(&mut self, procs: &[ProcIn]) {
        let codex: Vec<i32> = procs
            .iter()
            .filter(|p| matches!(p.facts.runtime, Some((Agent::Codex, _))))
            .map(|p| p.pid)
            .collect();
        let due = self.codex_scanned.is_none_or(|t| t.elapsed() >= CODEX_LOCK_EVERY);
        let new_runtime = codex.iter().any(|pid| !self.codex_locks.contains_key(pid));
        if !due && !new_runtime {
            return;
        }
        if due {
            self.codex_scanned = Some(Instant::now());
        }
        self.codex_locks.retain(|pid, _| codex.contains(pid));
        for pid in codex {
            if due || !self.codex_locks.contains_key(&pid) {
                let open = sys::open_file_paths(pid);
                let ids = agents::codex_sessions_from_open_files(&open, &self.cfg.codex_lock_dir);
                self.codex_locks.insert(pid, ids);
            }
        }
    }

    /// Idle state of every session. Claude reports its own status; otherwise
    /// a session is idle when its processes used < 1 % of a core over the
    /// last minute and none of them started less than a minute ago.
    fn session_states(
        &mut self,
        now: i64,
        procs: &[ProcIn],
        samples: &[Sample],
        classes: &[Class],
        runtimes: &[Runtime],
    ) -> Vec<Vec<SessionInfo>> {
        let mut cpu = vec![0u32; runtimes.len()];
        let mut young = vec![false; runtimes.len()];
        for (i, c) in classes.iter().enumerate() {
            if let Some(r) = c.runtime {
                cpu[r] = cpu[r].saturating_add(samples[i].cpu_pm);
                if runtimes[r].idx != i && (now as u64).saturating_sub(samples[i].start_sec) < IDLE_YOUNG_SECS {
                    young[r] = true;
                }
            }
        }
        let mut live = Vec::with_capacity(runtimes.len());
        let out = runtimes
            .iter()
            .enumerate()
            .map(|(r, rt)| {
                let p = &procs[rt.idx];
                let key = (p.pid, p.start_abs);
                live.push(key);
                let hist = self.runtime_cpu.entry(key).or_default();
                hist.push_back(cpu[r]);
                if hist.len() > IDLE_WINDOW {
                    hist.pop_front();
                }
                let avg = hist.iter().sum::<u32>() / hist.len() as u32;
                let measured_idle = hist.len() >= 2 && avg < IDLE_CPU_PM && !young[r];
                let idle = rt.reported_idle.unwrap_or(measured_idle);
                let n = match (rt.agent, rt.kind) {
                    (Agent::Codex, _) => rt.sessions.len().max(usize::from(rt.kind == RuntimeKind::Session)),
                    (Agent::Claude, _) => 1,
                };
                vec![SessionInfo { agent: rt.agent, idle }; n]
            })
            .collect();
        self.runtime_cpu.retain(|k, _| live.contains(k));
        out
    }

    pub fn pressure(&self) -> Pressure {
        self.pressure.shown()
    }
}

struct LiveSessions<'a> {
    claude: &'a ClaudeRegistry,
    known: &'a HashMap<i32, Known>,
    codex: &'a HashMap<i32, Vec<String>>,
}

impl SessionSource for LiveSessions<'_> {
    fn claude(&self, pid: i32) -> Option<(String, Option<bool>)> {
        let lstart = self.known.get(&pid).and_then(|k| k.lstart.as_deref());
        let s = self.claude.by_pid(pid, lstart)?;
        let idle = s.status.as_deref().map(|st| st == "idle");
        Some((s.session_id.clone(), idle))
    }

    fn codex(&self, pid: i32) -> Vec<String> {
        self.codex.get(&pid).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_tick_on_this_machine() {
        let mut c = Collector::new(Config::from_env());
        let first = c.tick(1_800_000_000);
        std::thread::sleep(Duration::from_millis(300));
        let s = c.tick(1_800_000_005);
        assert!(s.procs.len() > 50, "{}", s.procs.len());
        assert!(s.unreadable > 0);
        assert!(s.machine.used > 0 && s.machine.total > s.machine.used);
        // This test process runs from a git checkout: it is in a dev project
        // (this repo by its cwd, or the project of the agent that ran cargo).
        let me = std::process::id() as i32;
        let i = s.procs.iter().position(|p| p.pid == me).unwrap();
        assert!(s.classes[i].owner.is_project(), "{:?}", s.classes[i]);
        eprintln!(
            "procs {} unreadable {} runtimes {} first tick {:.1} ms, second {:.1} ms",
            s.procs.len(),
            s.unreadable,
            s.runtimes.len(),
            first.cost_ns as f64 / 1e6,
            s.cost_ns as f64 / 1e6
        );
    }
}
