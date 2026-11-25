//! What a row is made of at the last tick: its processes grouped by label
//! (agent runtimes as sessions), its docker stacks, and what quitting it
//! would signal or stop.

use std::collections::HashMap;
use std::sync::Arc;

use super::More;
use crate::collect::agents::Agent;
use crate::model::Gid;
use crate::quit::{self, Targets};

/// Parts sent per row; the rest is summed as "+N more".
pub const PARTS_ROWS: usize = 8;
/// Label of the Apple Virtualization process Docker Desktop runs its VM in.
pub const VM_LABEL: &str = "Linux VM";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PartKind {
    Process = 0,
    Agent = 1,
    Docker = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// "node", "Claude sessions", a docker stack name, "Linux VM".
    pub label: String,
    /// Docker: its services, "api, db". Else "".
    pub detail: String,
    /// Processes; agent parts: sessions; docker parts: running containers.
    pub count: u32,
    /// Agent parts: idle sessions.
    pub idle: u32,
    pub footprint: u64,
    pub kind: PartKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    /// Biggest first, at most PARTS_ROWS.
    pub parts: Vec<Part>,
    pub more: More,
    /// Processes a quit would signal.
    pub processes: u32,
    /// Containers a quit would stop.
    pub containers: u32,
}

/// One process of a row at the last tick.
#[derive(Clone, Debug)]
pub(crate) struct Member {
    pub gid: Gid,
    pub pid: i32,
    pub start_abs: u64,
    pub label: Arc<str>,
    /// Set on agent runtimes, with their sessions.
    pub agent: Option<Agent>,
    pub sessions: u32,
    pub idle: u32,
    pub footprint: u64,
}

/// One docker stack at the last tick.
#[derive(Clone, Debug)]
pub(crate) struct StackMember {
    /// The row it shows under: its worktree, or itself when orphan.
    pub row: Gid,
    pub name: String,
    pub services: Vec<String>,
    pub containers: Vec<String>,
    pub mem: u64,
}

/// The last tick's processes (System excluded) and stacks, refilled each
/// tick into the same buffers.
#[derive(Default)]
pub struct Members {
    pub(crate) procs: Vec<Member>,
    pub(crate) stacks: Vec<StackMember>,
}

impl Members {
    pub(crate) fn clear(&mut self) {
        self.procs.clear();
        self.stacks.clear();
    }

    /// None when no row had this id at the last tick.
    pub fn parts(&self, row: Gid) -> Option<Parts> {
        let mut out = Parts::default();
        let mut parts: Vec<Part> = Vec::new();
        let mut index: HashMap<(PartKind, &str), usize> = HashMap::new();
        for m in self.procs.iter().filter(|m| m.gid == row) {
            let key = match m.agent {
                Some(a) => (PartKind::Agent, a.label()),
                None => (PartKind::Process, &*m.label),
            };
            let i = *index.entry(key).or_insert_with(|| {
                parts.push(Part { label: key.1.to_string(), detail: String::new(), count: 0, idle: 0, footprint: 0, kind: key.0 });
                parts.len() - 1
            });
            let p = &mut parts[i];
            p.count += if m.agent.is_some() { m.sessions } else { 1 };
            p.idle += m.idle;
            p.footprint += m.footprint;
            out.processes += !quit::refused(m.pid) as u32;
        }
        for p in parts.iter_mut().filter(|p| p.kind == PartKind::Agent) {
            p.label = format!("{} {}", p.label, if p.count == 1 { "session" } else { "sessions" });
        }
        for s in self.stacks.iter().filter(|s| s.row == row) {
            parts.push(Part {
                label: s.name.clone(),
                detail: s.services.join(", "),
                count: s.containers.len() as u32,
                idle: 0,
                footprint: s.mem,
                kind: PartKind::Docker,
            });
            out.containers += s.containers.len() as u32;
        }
        if parts.is_empty() {
            return None;
        }
        parts.sort_by(|a, b| b.footprint.cmp(&a.footprint).then_with(|| a.label.cmp(&b.label)));
        let rest = parts.split_off(parts.len().min(PARTS_ROWS));
        out.more = More { count: rest.len() as u32, footprint: rest.iter().map(|p| p.footprint).sum() };
        out.parts = parts;
        Some(out)
    }

    /// What quitting the row signals and stops. None when no such row.
    pub fn targets(&self, row: Gid) -> Option<Targets> {
        let procs: Vec<quit::Proc> =
            self.procs.iter().filter(|m| m.gid == row).map(|m| quit::Proc { pid: m.pid, start_abs: m.start_abs }).collect();
        let stacks: Vec<&StackMember> = self.stacks.iter().filter(|s| s.row == row).collect();
        if procs.is_empty() && stacks.is_empty() {
            return None;
        }
        Some(Targets { procs, containers: stacks.iter().flat_map(|s| s.containers.iter().cloned()).collect() })
    }
}
