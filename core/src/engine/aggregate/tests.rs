use super::*;
use crate::collect::agents::RuntimeKind;
use crate::collect::classify::{Facts, ProcIn, Runtime};
use crate::collect::repo::tests::FakeFs;
use crate::collect::{Sample, SessionInfo};
use crate::model::MIB;

/// A hand-built scan, one process at a time.
struct B {
    scan: Scan,
}

impl B {
    fn new() -> Self {
        B { scan: Scan::default() }
    }

    fn proc(mut self, pid: i32, name: &str, argv: &[&str], owner: Owner, mib: u64, cpu_pm: u32) -> Self {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let label = crate::collect::label::label(name, &argv);
        let facts = Facts { name: name.into(), argv, label: label.into(), ..Facts::default() };
        self.scan.procs.push(ProcIn { pid, ppid: 1, start_abs: pid as u64, readable: true, facts: Arc::new(facts) });
        self.scan.samples.push(Sample { footprint: mib * MIB, cpu_pm, start_sec: 0 });
        self.scan.classes.push(Class { owner, runtime: None });
        self
    }

    /// Marks the last process as an agent runtime with these sessions.
    fn runtime(mut self, agent: Agent, sessions: &[bool]) -> Self {
        let idx = self.scan.procs.len() - 1;
        let p = &mut self.scan.procs[idx];
        let mut f = (*p.facts).clone();
        f.runtime = Some((agent, RuntimeKind::Session));
        p.facts = Arc::new(f);
        let owner = self.scan.classes[idx].owner.clone();
        self.scan.classes[idx].runtime = Some(self.scan.runtimes.len());
        self.scan.runtimes.push(Runtime { idx, agent, kind: RuntimeKind::Session, owner, sessions: vec![], reported_idle: None });
        self.scan.sessions.push(sessions.iter().map(|&idle| SessionInfo { agent, idle }).collect());
        self
    }
}

impl B {
    /// Sets the last process's executable, responsible process and parent.
    fn launched(mut self, exe: &str, responsible: Option<i32>, ppid: i32) -> Self {
        let p = self.scan.procs.last_mut().unwrap();
        let mut f = (*p.facts).clone();
        f.exe = Some(exe.into());
        f.responsible = responsible;
        p.facts = Arc::new(f);
        p.ppid = ppid;
        self
    }
}

fn wt(p: &str) -> Owner {
    Owner::Worktree(p.into())
}
fn app(p: &str) -> Owner {
    Owner::App(p.into())
}

fn setup() -> (Registry, RepoIndex) {
    let fs = FakeFs::default().repo("/w/a", "ref: refs/heads/feat/x").repo("/w/b", "ref: refs/heads/main");
    (Registry::new("/Users/me", vec![]), RepoIndex::with_fs("/Users/me".into(), Box::new(fs)))
}

fn stack(key: &str, name: &str, dir: Option<&str>, services: &[&str], mib: u64, cpu_pm: u32) -> Stack {
    Stack {
        key: key.into(),
        name: name.into(),
        working_dir: dir.map(Into::into),
        services: services.iter().map(|s| s.to_string()).collect(),
        mem: mib * MIB,
        cpu_pm,
        containers: if services.is_empty() { vec![name.into()] } else { services.iter().map(|s| format!("{name}-{s}")).collect() },
    }
}

const DOCKER: &str = "/Applications/Docker.app";
const CHROME: &str = "/Applications/Google Chrome.app";

fn machine() -> Scan {
    B::new()
        .proc(10, "claude", &["claude"], wt("/w/a"), 500, 5)
        .runtime(Agent::Claude, &[true])
        .proc(11, "node", &["node", "/w/a/node_modules/.bin/vite", "--token", "s3cr3t-value-xyz"], wt("/w/a"), 300, 40)
        .proc(12, "zsh", &["-zsh"], wt("/w/a"), 10, 0)
        .proc(20, "Google Chrome", &["Google Chrome"], app(CHROME), 200, 30)
        .proc(21, "Google Chrome He", &[], app(CHROME), 100, 10)
        .proc(22, "Google Chrome He", &[], app(CHROME), 50, 0)
        .proc(23, "Google Chrome He", &[], app(CHROME), 40, 0)
        .proc(30, "com.apple.Virtualization.Virtua", &[], app(DOCKER), 2000, 100)
        .proc(31, "com.docker.backend", &[], app(DOCKER), 50, 5)
        .proc(0, "kernel_task", &[], Owner::System, 1000, 50)
        .scan
}

fn row<'a>(a: &'a Agg, name: &str) -> &'a Row {
    a.rows.iter().find(|r| r.name == name).unwrap_or_else(|| panic!("no row {name}: {:#?}", a.rows))
}

#[test]
fn groups_rows_and_details_without_docker() {
    let (mut reg, mut repos) = setup();
    let a = aggregate(&machine(), &mut reg, &mut repos, Provider::Unknown, None, &mut ArgsCache::default(), &mut Members::default());

    let x = row(&a, "feat/x");
    assert_eq!((x.kind, x.footprint, x.cpu_pm), (RowKind::Dev, 810 * MIB, 45));
    assert_eq!(x.detail, "idle Claude session, vite, terminal");

    let c = row(&a, "Google Chrome");
    assert_eq!((c.kind, c.footprint, c.detail.as_str()), (RowKind::App, 390 * MIB, "4 processes"));
    let d = row(&a, "Docker");
    assert_eq!((d.footprint, d.detail.as_str()), (2050 * MIB, "app + engine"));

    // Dev rows first, then apps, each biggest first. No System row.
    let names: Vec<&str> = a.rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["feat/x", "Docker", "Google Chrome"]);

    // System still has a group tick (it's in the graph's "Other").
    assert_eq!(a.groups.len(), 4);
    assert_eq!(a.groups.iter().map(|g| g.footprint).sum::<u64>(), 4250 * MIB);

    // Top 3 per group, with scrubbed args.
    let chrome_gid = c.gid;
    let top: Vec<i32> = a.procs.iter().filter(|p| p.gid == chrome_gid).map(|p| p.pid).collect();
    assert_eq!(top, [20, 21, 22]);
    let vite = a.procs.iter().find(|p| p.pid == 11).unwrap();
    assert_eq!(&*vite.name, "vite");
    assert!(!vite.args.contains("s3cr3t"), "{}", vite.args);
}

#[test]
fn docker_stacks_move_from_docker_desktop_to_their_worktree() {
    let (mut reg, mut repos) = setup();
    let stacks = [
        stack("shop@/w/a", "shop", Some("/w/a"), &["redis", "postgres"], 300, 20),
        stack("container:scratch", "scratch", None, &[], 100, 0),
    ];
    let a = aggregate(&machine(), &mut reg, &mut repos, Provider::Unknown, Some(&stacks), &mut ArgsCache::default(), &mut Members::default());

    let x = row(&a, "feat/x");
    assert_eq!((x.footprint, x.cpu_pm), (1110 * MIB, 65));
    // Services come before labels; three parts at most.
    assert_eq!(x.detail, "idle Claude session, postgres, redis");

    let lone = row(&a, "scratch");
    assert_eq!((lone.kind, lone.footprint, lone.detail.as_str()), (RowKind::Dev, 100 * MIB, "container"));

    let d = row(&a, "Docker");
    assert_eq!((d.footprint, d.cpu_pm), (1650 * MIB, 85));

    // Machine total is unchanged: memory moved, not added.
    assert_eq!(a.groups.iter().map(|g| g.footprint).sum::<u64>(), 4250 * MIB);
    // The stack is its own group, under the worktree.
    let s = a.groups.iter().find(|g| reg.get(g.gid).unwrap().kind == GroupKind::Docker && reg.get(g.gid).unwrap().name == "shop").unwrap();
    assert_eq!(reg.parent_of(s.gid), Some(x.gid));
}

#[test]
fn stacks_bigger_than_the_vm_are_capped() {
    let (mut reg, mut repos) = setup();
    let stacks = [stack("big@/w/b", "big", Some("/w/b"), &["db"], 3000, 0), stack("small@/w/b", "small", Some("/w/b"), &["web"], 1000, 0)];
    let a = aggregate(&machine(), &mut reg, &mut repos, Provider::Unknown, Some(&stacks), &mut ArgsCache::default(), &mut Members::default());
    let b = row(&a, "b"); // main → folder name
    assert_eq!(b.footprint, 2000 * MIB);
    assert_eq!(b.detail, "db, web");
    assert_eq!(row(&a, "Docker").footprint, 50 * MIB);
}

#[test]
fn session_counts_in_details() {
    let (mut reg, mut repos) = setup();
    let scan = B::new()
        .proc(1, "claude", &["claude"], wt("/w/a"), 100, 0)
        .runtime(Agent::Claude, &[true])
        .proc(2, "claude", &["claude"], wt("/w/a"), 100, 0)
        .runtime(Agent::Claude, &[false])
        .proc(3, "claude", &["claude"], wt("/w/a"), 100, 0)
        .runtime(Agent::Claude, &[true])
        .proc(4, "codex", &["codex"], wt("/w/b"), 100, 0)
        .runtime(Agent::Codex, &[false, false])
        .scan;
    let a = aggregate(&scan, &mut reg, &mut repos, Provider::Unknown, None, &mut ArgsCache::default(), &mut Members::default());
    assert_eq!(row(&a, "feat/x").detail, "3 Claude sessions (2 idle)");
    assert_eq!(row(&a, "b").detail, "2 Codex sessions");
}

#[test]
fn dev_detail_wording() {
    let mut a = Acc::default();
    assert_eq!(dev_detail(&a, &[]), "");
    a.claude = (3, 3);
    a.codex = (1, 0);
    assert_eq!(dev_detail(&a, &[]), "3 idle Claude sessions, Codex session");
    a.claude = (2, 0);
    a.codex = (0, 0);
    a.labels.insert("next".into(), 5);
    a.labels.insert("terminal".into(), 9);
    assert_eq!(dev_detail(&a, &[]), "2 Claude sessions, terminal, next");
}

// ------------------------------------------------------------------ parts

use crate::engine::More;
use crate::engine::parts::{PARTS_ROWS, Part, PartKind};

fn run(scan: &Scan, stacks: Option<&[Stack]>) -> (Agg, Members, Registry) {
    let (mut reg, mut repos) = setup();
    let mut m = Members::default();
    let a = aggregate(scan, &mut reg, &mut repos, Provider::Unknown, stacks, &mut ArgsCache::default(), &mut m);
    (a, m, reg)
}

fn part(kind: PartKind, label: &str, detail: &str, count: u32, idle: u32, mib: u64) -> Part {
    Part { label: label.into(), detail: detail.into(), count, idle, footprint: mib * MIB, kind }
}

/// Every row's parts, plus "+N more", add up to the row.
fn assert_parts_add_up(a: &Agg, m: &Members) {
    for r in &a.rows {
        let p = m.parts(r.gid).unwrap_or_else(|| panic!("no parts for {}", r.name));
        let sum: u64 = p.parts.iter().map(|p| p.footprint).sum::<u64>() + p.more.footprint;
        assert_eq!(sum, r.footprint, "{}: {p:#?}", r.name);
    }
}

#[test]
fn dev_row_parts_group_agents_labels_and_stacks() {
    let mut scan = B::new()
        .proc(101, "claude", &["claude"], wt("/w/a"), 400, 0)
        .runtime(Agent::Claude, &[true])
        .proc(102, "claude", &["claude"], wt("/w/a"), 300, 0)
        .runtime(Agent::Claude, &[false])
        .proc(103, "node", &["node"], wt("/w/a"), 200, 0)
        .proc(104, "zsh", &["-zsh"], wt("/w/a"), 10, 0)
        .proc(105, "node", &["node"], wt("/w/a"), 50, 0)
        .proc(106, "Google Chrome", &["Google Chrome"], app(CHROME), 100, 0)
        .scan;
    // A tool the first session runs: same runtime, counted by its label.
    scan.classes[2].runtime = Some(0);
    let stacks = [stack("shop@/w/a", "shop", Some("/w/a"), &["api", "db", "redis"], 600, 0)];
    let (a, m, _) = run(&scan, Some(&stacks));
    let x = row(&a, "feat/x");
    assert_eq!(x.path, "/w/a");
    let p = m.parts(x.gid).unwrap();
    assert_eq!(
        p.parts,
        [
            part(PartKind::Agent, "Claude sessions", "", 2, 1, 700),
            part(PartKind::Docker, "shop", "api, db, redis", 3, 0, 600),
            part(PartKind::Process, "node", "", 2, 0, 250),
            part(PartKind::Process, "terminal", "", 1, 0, 10),
        ]
    );
    assert_eq!((p.more, p.processes, p.containers), (More::default(), 5, 3));
    assert_eq!(p.parts.iter().map(|p| p.footprint).sum::<u64>(), x.footprint);

    let t = m.targets(x.gid).unwrap();
    let pids: Vec<i32> = t.procs.iter().map(|p| p.pid).collect();
    assert_eq!(pids, [101, 102, 103, 104, 105]);
    assert_eq!(t.procs[2].start_abs, 103, "start time kept for the recycled-pid check");
    assert_eq!(t.containers, ["shop-api", "shop-db", "shop-redis"]);
}

#[test]
fn one_session_reads_singular_and_ties_sort_by_name() {
    let scan = B::new()
        .proc(1, "codex", &["codex"], wt("/w/a"), 100, 0)
        .runtime(Agent::Codex, &[true])
        .proc(2, "zsh", &["-zsh"], wt("/w/a"), 100, 0)
        .proc(3, "node", &["node"], wt("/w/a"), 100, 0)
        .scan;
    let (a, m, _) = run(&scan, None);
    let p = m.parts(row(&a, "feat/x").gid).unwrap().parts;
    let labels: Vec<&str> = p.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, ["Codex session", "node", "terminal"]);
    assert_eq!((p[0].count, p[0].idle), (1, 1));
}

#[test]
fn app_row_parts_by_label_with_more_beyond_eight() {
    let helper = |kind: &str| {
        format!(
            "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Helpers/Google Chrome Helper ({kind}).app/Contents/MacOS/Google Chrome Helper ({kind})"
        )
    };
    let (renderer, gpu) = (helper("Renderer"), helper("GPU"));
    let mut b = B::new()
        .proc(101, "Google Chrome", &["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"], app(CHROME), 300, 0)
        .proc(102, "Google Chrome He", &[&renderer, "--type=renderer"], app(CHROME), 200, 0)
        .proc(103, "Google Chrome He", &[&renderer, "--type=renderer"], app(CHROME), 150, 0)
        .proc(104, "Google Chrome He", &[&gpu, "--type=gpu-process"], app(CHROME), 100, 0);
    // Ten more small, distinct programs: 13 labels in all.
    for i in 0..10 {
        let name = format!("tool{i}");
        b = b.proc(10 + i, &name, &[&name], app(CHROME), 10 - i as u64, 0);
    }
    let (a, m, _) = run(&b.scan, None);
    let c = row(&a, "Google Chrome");
    assert_eq!(c.path, CHROME);
    let p = m.parts(c.gid).unwrap();
    assert_eq!(p.parts.len(), PARTS_ROWS);
    assert_eq!(p.parts[0], part(PartKind::Process, "Google Chrome Helper (Renderer)", "", 2, 0, 350));
    assert_eq!(p.parts[1], part(PartKind::Process, "Google Chrome", "", 1, 0, 300));
    assert_eq!(p.parts[2].label, "Google Chrome Helper (GPU)");
    // tool0..tool4 shown (10..6 MiB), tool5..tool9 summed (5+4+3+2+1).
    assert_eq!(p.parts[7].label, "tool4");
    assert_eq!(p.more, More { count: 5, footprint: 15 * MIB });
    assert_eq!((p.processes, p.containers), (14, 0));
    assert_parts_add_up(&a, &m);
}

#[test]
fn docker_desktop_vm_is_a_linux_vm_without_the_moved_stacks() {
    let stacks = [
        stack("shop@/w/a", "shop", Some("/w/a"), &["redis", "postgres"], 300, 20),
        stack("container:scratch", "scratch", None, &[], 100, 0),
    ];
    let (a, m, _) = run(&machine(), Some(&stacks));
    let d = row(&a, "Docker");
    let p = m.parts(d.gid).unwrap();
    assert_eq!(
        p.parts,
        [part(PartKind::Process, "Linux VM", "", 1, 0, 1600), part(PartKind::Process, "com.docker.backend", "", 1, 0, 50)]
    );
    // The orphan stack is its own row, made of itself.
    let lone = row(&a, "scratch");
    assert_eq!(lone.path, "");
    let p = m.parts(lone.gid).unwrap();
    assert_eq!(p.parts, [part(PartKind::Docker, "scratch", "", 1, 0, 100)]);
    assert_eq!((p.processes, p.containers), (0, 1));
    assert_eq!(m.targets(lone.gid).unwrap().containers, ["scratch"]);
    assert_parts_add_up(&a, &m);
}

#[test]
fn parts_add_up_to_rows_with_and_without_docker() {
    let (a, m, _) = run(&machine(), None);
    assert_parts_add_up(&a, &m);
    // Stacks bigger than the VM: scaled down, still consistent.
    let stacks = [stack("big@/w/b", "big", Some("/w/b"), &["db"], 3000, 0), stack("small@/w/b", "small", Some("/w/b"), &["web"], 1000, 0)];
    let (a, m, _) = run(&machine(), Some(&stacks));
    assert_parts_add_up(&a, &m);
    let d = m.parts(row(&a, "Docker").gid).unwrap();
    assert_eq!(d.parts[0], part(PartKind::Process, "com.docker.backend", "", 1, 0, 50));
    assert_eq!(d.parts[1], part(PartKind::Process, "Linux VM", "", 1, 0, 0));
}

#[test]
fn no_parts_for_unknown_rows_or_system() {
    let (a, m, reg) = run(&machine(), None);
    let system = a.groups.iter().map(|g| g.gid).find(|&g| reg.get(g).unwrap().kind == GroupKind::System).unwrap();
    assert_eq!(m.parts(system), None);
    assert_eq!(m.targets(system), None);
    assert_eq!(m.parts(9999), None);
    assert_eq!(m.targets(9999), None);
}

#[test]
fn members_reuse_their_buffers() {
    let (mut reg, mut repos) = setup();
    let mut m = Members::default();
    let scan = machine();
    aggregate(&scan, &mut reg, &mut repos, Provider::Unknown, None, &mut ArgsCache::default(), &mut m);
    assert_eq!(m.procs.len(), 9, "System is left out");
    let cap = m.procs.capacity();
    aggregate(&scan, &mut reg, &mut repos, Provider::Unknown, None, &mut ArgsCache::default(), &mut m);
    assert_eq!((m.procs.len(), m.procs.capacity()), (9, cap));
}

#[test]
fn args_cache_scrubs_once_and_forgets_dead_processes() {
    let mut c = ArgsCache::default();
    let argv = vec!["x".to_string()];
    let first = c.get(1, 1, &argv);
    assert!(Arc::ptr_eq(&first, &c.get(1, 1, &argv)));
    c.prune();
    for pid in 100..300 {
        c.get(pid, 0, &argv);
        c.prune();
    }
    assert!(c.map.len() < 70, "{}", c.map.len());
}

// ------------------------------------------------------------------ docker VMs

const ORBSTACK: &str = "/Applications/OrbStack.app";
const TERMINAL: &str = "/System/Applications/Utilities/Terminal.app";
const UTM: &str = "/Applications/UTM.app";
const VZ: &str = "com.apple.Virtualization.Virtua";
const VZ_EXE: &str = "/System/Library/Frameworks/Virtualization.framework/Versions/A/XPCServices/com.apple.Virtualization.VirtualMachine.xpc/Contents/MacOS/com.apple.Virtualization.VirtualMachine";

fn total(a: &Agg) -> u64 {
    a.groups.iter().map(|g| g.footprint).sum()
}

fn agg(scan: &Scan, provider: Provider, stacks: &[Stack]) -> (Agg, Members, Registry) {
    let (mut reg, mut repos) = setup();
    let mut m = Members::default();
    let a = aggregate(scan, &mut reg, &mut repos, provider, Some(stacks), &mut ArgsCache::default(), &mut m);
    (a, m, reg)
}

fn shop() -> [Stack; 1] {
    [stack("shop@/w/a", "shop", Some("/w/a"), &["postgres", "redis"], 600, 30)]
}

#[test]
fn orbstack_vm_in_its_helper_gives_the_stacks_their_memory_once() {
    // OrbStack's VM memory can show in "OrbStack Helper" and in the
    // Virtualization XPC it is responsible for: both are its VM.
    let scan = B::new()
        .proc(1, "node", &["node"], wt("/w/a"), 100, 0)
        .proc(40, "OrbStack", &[], app(ORBSTACK), 80, 0)
        .proc(41, "OrbStack Helper", &[], app(ORBSTACK), 1500, 50)
        .launched("/Applications/OrbStack.app/Contents/Frameworks/OrbStack Helper.app/Contents/MacOS/OrbStack Helper", None, 40)
        .proc(42, VZ, &[], app(ORBSTACK), 400, 10)
        .launched(VZ_EXE, Some(40), 1)
        .scan;
    let (a, m, _) = agg(&scan, Provider::OrbStack, &shop());
    assert_eq!(row(&a, "feat/x").footprint, 700 * MIB);
    let o = row(&a, "OrbStack");
    assert_eq!((o.footprint, o.cpu_pm, o.detail.as_str()), (1380 * MIB, 30, "app + engine"));
    assert_eq!(total(&a), 2080 * MIB, "memory moved, not added");
    assert_parts_add_up(&a, &m);
}

#[test]
fn colima_vm_held_by_no_app_comes_out_of_system() {
    // Colima with vz: the VM is an XPC macOS holds limactl responsible for.
    // limactl is no app, so the VM is System: no row, but it still holds the
    // containers' memory.
    let scan = B::new()
        .proc(1, "node", &["node"], wt("/w/a"), 100, 0)
        .proc(50, "limactl", &["limactl", "hostagent", "--pidfile", "/Users/me/.colima/_lima/colima/ha.pid", "colima"], Owner::System, 40, 0)
        .launched("/opt/homebrew/Cellar/lima/1.0.0/bin/limactl", None, 1)
        .proc(51, VZ, &[], Owner::System, 2000, 80)
        .launched(VZ_EXE, Some(50), 1)
        .proc(0, "kernel_task", &[], Owner::System, 1000, 0)
        .scan;
    let (a, m, reg) = agg(&scan, Provider::Colima, &shop());
    assert_eq!(row(&a, "feat/x").footprint, 700 * MIB);
    assert_eq!(total(&a), 3140 * MIB, "memory moved, not added");
    let sys = a.groups.iter().find(|g| reg.get(g.gid).unwrap().kind == GroupKind::System).unwrap();
    assert_eq!((sys.footprint, sys.cpu_pm), (2440 * MIB, 50));
    assert_parts_add_up(&a, &m);
}

#[test]
fn colima_qemu_vm_in_a_worktree_is_not_counted_twice() {
    // `colima start` from a worktree with vmType qemu: QEMU's cwd is the
    // worktree, so its memory is the worktree's, and so is the stack's.
    let scan = B::new()
        .proc(1, "node", &["node"], wt("/w/a"), 100, 0)
        .proc(60, "qemu-system-aarc", &["qemu-system-aarch64", "-drive", "file=/Users/me/.colima/_lima/colima/diffdisk"], wt("/w/a"), 1800, 40)
        .launched("/opt/homebrew/bin/qemu-system-aarch64", None, 1)
        .scan;
    let (a, m, _) = agg(&scan, Provider::Colima, &shop());
    assert_eq!(row(&a, "feat/x").footprint, 1900 * MIB, "the VM once, its stack inside it");
    assert_eq!(total(&a), 1900 * MIB);
    assert_parts_add_up(&a, &m);
}

#[test]
fn colima_vm_held_by_the_terminal_still_holds_the_stacks() {
    // Started from Terminal, the VM's responsible process is Terminal: it
    // can't be tied to Colima, but no other VM is Colima's.
    let scan = B::new()
        .proc(1, "node", &["node"], wt("/w/a"), 100, 0)
        .proc(70, "Terminal", &[], app(TERMINAL), 100, 0)
        .proc(71, VZ, &[], app(TERMINAL), 2000, 0)
        .launched(VZ_EXE, Some(70), 1)
        .proc(30, VZ, &[], app(DOCKER), 900, 0)
        .scan;
    let (a, m, _) = agg(&scan, Provider::Colima, &shop());
    assert_eq!(row(&a, "Terminal").footprint, 1500 * MIB);
    assert_eq!(row(&a, "Docker").footprint, 900 * MIB, "Docker Desktop's VM is not Colima's");
    assert_eq!(total(&a), 3100 * MIB);
    assert_parts_add_up(&a, &m);
}

#[test]
fn another_vm_next_to_docker_desktop_keeps_its_memory() {
    let scan = B::new()
        .proc(1, "node", &["node"], wt("/w/a"), 100, 0)
        .proc(80, VZ, &[], app(UTM), 4000, 0)
        .proc(30, VZ, &[], app(DOCKER), 2000, 100)
        .proc(31, "com.docker.backend", &[], app(DOCKER), 50, 5)
        .scan;
    for provider in [Provider::DockerDesktop, Provider::Unknown] {
        let (a, m, _) = agg(&scan, provider, &shop());
        assert_eq!(row(&a, "UTM").footprint, 4000 * MIB, "{provider:?}");
        assert_eq!(row(&a, "UTM").detail, "1 process");
        assert_eq!(row(&a, "Docker").footprint, 1450 * MIB);
        assert_eq!(total(&a), 6150 * MIB);
        assert_parts_add_up(&a, &m);
    }
}

#[test]
fn docker_desktop_and_orbstack_both_running_use_the_socket_one() {
    let scan = B::new()
        .proc(1, "node", &["node"], wt("/w/a"), 100, 0)
        .proc(30, VZ, &[], app(DOCKER), 2000, 0)
        .proc(41, "OrbStack Helper", &[], app(ORBSTACK), 1000, 0)
        .scan;
    let (a, _, _) = agg(&scan, Provider::OrbStack, &shop());
    assert_eq!(row(&a, "Docker").footprint, 2000 * MIB);
    assert_eq!(row(&a, "OrbStack").footprint, 400 * MIB);
    // Socket unknown and two engines: not taken from either app (it could
    // be the wrong one); the stacks are added, as before.
    let (a, _, _) = agg(&scan, Provider::Unknown, &shop());
    assert_eq!((row(&a, "Docker").footprint, row(&a, "OrbStack").footprint), (2000 * MIB, 1000 * MIB));
}
