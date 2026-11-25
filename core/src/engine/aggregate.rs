//! One tick → per-group totals, the top processes of each group, and the rows
//! of the popover lists.

use std::collections::HashMap;
use std::sync::Arc;

use super::groups::Registry;
use super::parts::{Member, Members, StackMember, VM_LABEL};
use crate::collect::Scan;
use crate::collect::agents::Agent;
use crate::collect::classify::{Class, Owner};
use crate::collect::repo::RepoIndex;
use crate::docker::{self, Provider, Stack};
use crate::model::{Gid, GroupKind, GroupTick, ProcTick};
use crate::scrub;

/// Top processes kept per group, per tick and per minute.
pub const TOP_PROCS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Dev,
    App,
}

/// One line of a popover list. `footprint` includes child groups (a
/// worktree's docker stacks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub gid: Gid,
    pub kind: RowKind,
    pub name: String,
    /// Worktree root or .app bundle path; "" for an orphan docker stack.
    pub path: String,
    pub detail: String,
    pub footprint: u64,
    pub cpu_pm: u32,
}

#[derive(Debug, Default)]
pub struct Agg {
    /// Each group's own processes (a worktree without its docker stacks).
    pub groups: Vec<GroupTick>,
    pub procs: Vec<ProcTick>,
    /// Dev rows then app rows, each sorted by footprint, biggest first.
    pub rows: Vec<Row>,
}

/// Per-group accumulation during one tick.
#[derive(Default)]
struct Acc {
    footprint: u64,
    cpu_pm: u32,
    procs: u32,
    /// (footprint, index into scan.procs), the biggest TOP_PROCS.
    top: Vec<(u64, usize)>,
    /// label → footprint, for the row detail.
    labels: HashMap<Arc<str>, u64>,
    claude: (u32, u32), // (sessions, idle)
    codex: (u32, u32),
    services: Vec<String>,
    is_docker_app: bool,
}

impl Acc {
    fn push_top(&mut self, fp: u64, i: usize) {
        self.top.push((fp, i));
        self.top.sort_unstable_by_key(|t| std::cmp::Reverse(t.0));
        self.top.truncate(TOP_PROCS);
    }
}

/// Scrubbed args of each process, computed once per process (by pid and
/// start time) because scrubbing runs regexes.
#[derive(Default)]
pub struct ArgsCache {
    map: HashMap<(i32, u64), Arc<str>>,
    seen: Vec<(i32, u64)>,
}

impl ArgsCache {
    fn get(&mut self, pid: i32, start: u64, argv: &[String]) -> Arc<str> {
        self.seen.push((pid, start));
        self.map.entry((pid, start)).or_insert_with(|| scrub::scrub_args(argv).into()).clone()
    }

    /// Drop entries not asked for since the last prune.
    fn prune(&mut self) {
        if self.map.len() > 2 * self.seen.len() + 64 {
            let keep: std::collections::HashSet<_> = self.seen.iter().copied().collect();
            self.map.retain(|k, _| keep.contains(k));
        }
        self.seen.clear();
    }
}

/// `members` is refilled with what the rows are made of. `provider` runs the
/// Docker engine the stacks come from: its VM processes hold the stacks'
/// memory.
#[allow(clippy::too_many_arguments)]
pub fn aggregate(
    scan: &Scan,
    reg: &mut Registry,
    repos: &mut RepoIndex,
    provider: Provider,
    stacks: Option<&[Stack]>,
    args: &mut ArgsCache,
    members: &mut Members,
) -> Agg {
    members.clear();
    let mut acc: HashMap<Gid, Acc> = HashMap::new();
    // Owner → gid, so each distinct owner hits the registry once per tick.
    let mut gid_of: HashMap<&Owner, Gid> = HashMap::new();
    let mut runtime_gid: Vec<Gid> = Vec::with_capacity(scan.runtimes.len());
    for rt in &scan.runtimes {
        let g = *gid_of.entry(&rt.owner).or_insert_with(|| reg.of_owner(&rt.owner, repos));
        runtime_gid.push(g);
    }

    // The Docker engine's VM processes, as indices into scan.procs.
    let docker_vms = docker_vms(scan, provider);
    let vm_footprint: u64 = docker_vms.iter().map(|&i| scan.samples[i].footprint).sum();
    let vm_cpu: u32 = docker_vms.iter().map(|&i| scan.samples[i].cpu_pm).sum();

    for (i, (p, Class { owner, runtime })) in scan.procs.iter().zip(&scan.classes).enumerate() {
        let s = &scan.samples[i];
        let gid = *gid_of.entry(owner).or_insert_with(|| reg.of_owner(owner, repos));
        let a = acc.entry(gid).or_default();
        a.footprint += s.footprint;
        a.cpu_pm += s.cpu_pm;
        a.procs += 1;
        a.push_top(s.footprint, i);
        let is_vm = docker_vms.contains(&i);
        if is_vm && matches!(owner, Owner::App(_)) {
            a.is_docker_app = true;
        }
        if *owner != Owner::System {
            // The runtime process itself carries its sessions; its children
            // share its `runtime` but are counted by label.
            let rt = runtime.filter(|&r| scan.runtimes[r].idx == i);
            let sessions = rt.map_or(&[][..], |r| &scan.sessions[r]);
            members.procs.push(Member {
                gid,
                pid: p.pid,
                start_abs: p.start_abs,
                label: if is_vm { VM_LABEL.into() } else { p.facts.label.clone() },
                agent: rt.map(|r| scan.runtimes[r].agent),
                sessions: sessions.len() as u32,
                idle: sessions.iter().filter(|s| s.idle).count() as u32,
                footprint: s.footprint,
            });
        }
        // Agent runtimes are counted as sessions, not as a label.
        if p.facts.runtime.is_none() {
            *a.labels.entry(p.facts.label.clone()).or_default() += s.footprint;
        }
    }

    // `scan.sessions` is parallel to `scan.runtimes`.
    for (infos, &gid) in scan.sessions.iter().zip(&runtime_gid) {
        let a = acc.entry(gid).or_default();
        for s in infos {
            bump(a, s.agent, s.idle);
        }
    }

    // Docker stacks: their own groups, under the worktree of their compose
    // folder. Their memory comes out of the VM process's group (Docker
    // Desktop's or OrbStack's app row, System for a Colima VM…).
    if let Some(stacks) = stacks {
        let mut stacks = stacks.to_vec();
        let cap = if docker_vms.is_empty() { u64::MAX } else { vm_footprint };
        crate::docker::cap_to_vm(&mut stacks, cap);
        let mut moved_mem = 0u64;
        let mut moved_cpu = 0u32;
        for st in &stacks {
            let parent = st.working_dir.as_deref().and_then(|d| repos.root_of(d)).map(|root| reg.of_owner(&Owner::Worktree(root.into()), repos));
            let gid = reg.ensure(GroupKind::Docker, &st.key, &st.name, parent);
            let a = acc.entry(gid).or_default();
            a.footprint += st.mem;
            a.cpu_pm += st.cpu_pm;
            a.services = if st.services.is_empty() && parent.is_none() { vec!["container".into()] } else { st.services.clone() };
            moved_mem += st.mem;
            moved_cpu += st.cpu_pm;
            members.stacks.push(StackMember {
                row: parent.unwrap_or(gid),
                name: st.name.clone(),
                services: st.services.clone(),
                containers: st.containers.clone(),
                mem: st.mem,
            });
        }
        // The stacks' memory comes out of the VM processes, biggest first,
        // so each group (and its parts) still adds up.
        let mut left_mem = moved_mem;
        let mut left_cpu = moved_cpu.min(vm_cpu);
        for &i in &docker_vms {
            let gid = gid_of[&scan.classes[i].owner];
            let s = &scan.samples[i];
            let (mem, cpu) = (left_mem.min(s.footprint), left_cpu.min(s.cpu_pm));
            left_mem -= mem;
            left_cpu -= cpu;
            let a = acc.get_mut(&gid).expect("VM process was counted");
            a.footprint = a.footprint.saturating_sub(mem);
            a.cpu_pm = a.cpu_pm.saturating_sub(cpu);
            let pid = scan.procs[i].pid;
            if let Some(m) = members.procs.iter_mut().find(|m| m.pid == pid && m.gid == gid) {
                m.footprint -= mem.min(m.footprint);
            }
        }
    }

    // Per-group ticks and top processes.
    let mut out = Agg::default();
    let mut gids: Vec<Gid> = acc.keys().copied().collect();
    gids.sort_unstable();
    for &gid in &gids {
        let a = &acc[&gid];
        out.groups.push(GroupTick { gid, footprint: a.footprint, cpu_pm: a.cpu_pm });
        for &(fp, i) in &a.top {
            let p = &scan.procs[i];
            out.procs.push(ProcTick {
                gid,
                pid: p.pid,
                name: p.facts.label.clone(),
                args: args.get(p.pid, p.start_abs, &p.facts.argv),
                footprint: fp,
                cpu_pm: scan.samples[i].cpu_pm,
            });
        }
    }
    args.prune();

    out.rows = rows(&acc, reg);
    out
}

/// The VM processes that run `provider`'s Docker engine, biggest first.
fn docker_vms(scan: &Scan, provider: Provider) -> Vec<usize> {
    let vms: Vec<usize> =
        (0..scan.procs.len()).filter(|&i| docker::is_vm_process(&scan.procs[i].facts.name, scan.procs[i].facts.exe.as_deref())).collect();
    if vms.is_empty() {
        return Vec::new();
    }
    let providers: Vec<Provider> = vms.iter().map(|&i| vm_provider(scan, i)).collect();
    let mut out: Vec<usize> = docker::engine_vms(provider, &providers).into_iter().map(|k| vms[k]).collect();
    out.sort_by_key(|&i| std::cmp::Reverse(scan.samples[i].footprint));
    out
}

/// Who runs this VM process: its app, else the paths in its own command
/// line, its launcher's (the process macOS holds responsible) or its
/// parent's.
fn vm_provider(scan: &Scan, i: usize) -> Provider {
    if let Owner::App(app) = &scan.classes[i].owner {
        let p = Provider::of_text(app);
        if p != Provider::Unknown {
            return p;
        }
    }
    let p = &scan.procs[i];
    let related = [Some(p.pid), p.facts.responsible, Some(p.ppid).filter(|&pp| pp > 1)];
    for pid in related.into_iter().flatten() {
        let Some(q) = scan.procs.iter().find(|q| q.pid == pid) else { continue };
        let text = format!("{} {}", q.facts.exe.as_deref().unwrap_or(""), q.facts.argv.join(" "));
        let found = Provider::of_text(&text);
        if found != Provider::Unknown {
            return found;
        }
    }
    Provider::Unknown
}

fn bump(a: &mut Acc, agent: Agent, idle: bool) {
    let c = match agent {
        Agent::Claude => &mut a.claude,
        Agent::Codex => &mut a.codex,
    };
    c.0 += 1;
    c.1 += idle as u32;
}

/// Popover rows: dev projects (worktrees and folders, with their docker
/// stacks folded in; orphan stacks as their own rows) and apps. System has no
/// row.
fn rows(acc: &HashMap<Gid, Acc>, reg: &Registry) -> Vec<Row> {
    let mut dev: HashMap<Gid, Row> = HashMap::new();
    let mut services: HashMap<Gid, Vec<String>> = HashMap::new();
    let mut apps = Vec::new();
    for (&gid, a) in acc {
        let Some(g) = reg.get(gid) else { continue };
        match g.kind {
            GroupKind::System => {}
            GroupKind::App => apps.push(Row {
                gid,
                kind: RowKind::App,
                name: g.name.clone(),
                path: g.path.clone(),
                detail: if a.is_docker_app { "app + engine".into() } else { plural(a.procs, "process", "processes") },
                footprint: a.footprint,
                cpu_pm: a.cpu_pm,
            }),
            GroupKind::Worktree | GroupKind::Docker => {
                let host = if g.kind == GroupKind::Docker { g.parent.unwrap_or(gid) } else { gid };
                let h = reg.get(host);
                let name = h.map(|h| h.name.clone()).unwrap_or_default();
                let path = h.filter(|h| h.kind == GroupKind::Worktree).map(|h| h.path.clone()).unwrap_or_default();
                let r = dev.entry(host).or_insert_with(|| Row {
                    gid: host,
                    kind: RowKind::Dev,
                    name,
                    path,
                    detail: String::new(),
                    footprint: 0,
                    cpu_pm: 0,
                });
                r.footprint += a.footprint;
                r.cpu_pm += a.cpu_pm;
                services.entry(host).or_default().extend(a.services.iter().cloned());
            }
        }
    }
    let mut dev: Vec<Row> = dev.into_values().collect();
    for r in &mut dev {
        let empty = Acc::default();
        let a = acc.get(&r.gid).unwrap_or(&empty);
        let mut svc = services.remove(&r.gid).unwrap_or_default();
        svc.sort();
        svc.dedup();
        r.detail = dev_detail(a, &svc);
    }
    let by_size = |a: &Row, b: &Row| b.footprint.cmp(&a.footprint).then_with(|| a.name.cmp(&b.name));
    dev.sort_by(by_size);
    apps.sort_by(by_size);
    dev.extend(apps);
    dev
}

/// "3 idle Claude sessions, terminal", "postgres, redis", "Codex session, vite".
fn dev_detail(a: &Acc, services: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (agent, (n, idle)) in [("Claude", a.claude), ("Codex", a.codex)] {
        if n == 0 {
            continue;
        }
        let noun = if n == 1 { "session" } else { "sessions" };
        parts.push(match (n, idle) {
            (1, 1) => format!("idle {agent} session"),
            (1, _) => format!("{agent} session"),
            (n, i) if i == n => format!("{n} idle {agent} {noun}"),
            (n, 0) => format!("{n} {agent} {noun}"),
            (n, i) => format!("{n} {agent} {noun} ({i} idle)"),
        });
    }
    parts.extend(services.iter().cloned());
    let mut labels: Vec<(&Arc<str>, &u64)> = a.labels.iter().collect();
    labels.sort_by(|x, y| y.1.cmp(x.1).then_with(|| x.0.cmp(y.0)));
    for (l, _) in labels {
        if parts.len() >= 3 {
            break;
        }
        if !parts.iter().any(|p| **p == **l) {
            parts.push(l.to_string());
        }
    }
    parts.join(", ")
}

fn plural(n: u32, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

#[cfg(test)]
mod tests;
