//! Who owns each process: a dev project (worktree or plain folder), an app,
//! or System. Pure over one scan, so it is tested on hand-made scans.
//!
//! Rules, first match wins:
//! 1. An agent runtime (Claude Code, Codex) belongs to the project of its cwd;
//!    Codex's app-server belongs to the app that launched it.
//! 2. A command carrying an agent session marker (CLAUDE_CODE_SESSION_ID,
//!    CODEX_SESSION_ID) belongs to that session's project, even when its
//!    parent is gone (daemonized dev servers).
//! 3. A child of a live process that belongs to a project, started after it
//!    (pid reuse guard), belongs to the same project and session.
//! 4. A process whose own cwd is inside a git worktree belongs to it.
//! 5. A process with an app (the process macOS holds responsible for it, else
//!    itself) belongs to that app's outermost .app bundle.
//! 6. Everything else, and every process we cannot read, is System.

use std::collections::HashMap;
use std::sync::Arc;

use super::agents::{Agent, RuntimeKind};
use super::repo::RepoIndex;

/// What we learn about a process the first time we see it. Never re-read.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Short name from the kernel (`pbi_name`).
    pub name: Arc<str>,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// `responsibility_get_pid_responsible_for_pid`, when not itself.
    pub responsible: Option<i32>,
    pub runtime: Option<(Agent, RuntimeKind)>,
    pub marker: Option<(Agent, String)>,
    /// Short program label for the UI ("terminal", "next", "postgres").
    pub label: Arc<str>,
}

/// One live process in this tick's scan.
#[derive(Clone, Debug)]
pub struct ProcIn {
    pub pid: i32,
    pub ppid: i32,
    /// `ri_proc_start_abstime` (mach units): orders parent/child starts.
    pub start_abs: u64,
    /// Unreadable (EPERM): no facts beyond the name, always System.
    pub readable: bool,
    pub facts: Arc<Facts>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Owner {
    /// A git worktree root.
    Worktree(Arc<str>),
    /// A folder without git that an agent runs in.
    Folder(Arc<str>),
    /// An app bundle path ("/Applications/Google Chrome.app").
    App(Arc<str>),
    System,
}

impl Owner {
    pub fn is_project(&self) -> bool {
        matches!(self, Owner::Worktree(_) | Owner::Folder(_))
    }
}

/// An agent runtime found in this scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    /// Index of the runtime process in the scan.
    pub idx: usize,
    pub agent: Agent,
    pub kind: RuntimeKind,
    pub owner: Owner,
    /// Session ids it runs: Claude's registry id, Codex's lock ids.
    pub sessions: Vec<String>,
    /// Claude's own idle/busy status, when it reports one.
    pub reported_idle: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Class {
    pub owner: Owner,
    /// Index into the runtimes of the scan.
    pub runtime: Option<usize>,
}

/// Session information the classifier needs, from the Claude registry and the
/// Codex lock files.
pub trait SessionSource {
    /// (session id, reported idle) of a claude runtime.
    fn claude(&self, pid: i32) -> Option<(String, Option<bool>)>;
    /// Codex session ids held open by this codex runtime.
    fn codex(&self, pid: i32) -> Vec<String>;
}

/// The outermost `.app` bundle in a path.
pub fn outer_app(path: &str) -> Option<&str> {
    let i = path.find(".app/").or_else(|| path.ends_with(".app").then(|| path.len() - 4))?;
    Some(&path[..i + 4])
}

pub struct Classifier<'a> {
    pub repos: &'a mut RepoIndex,
    pub sessions: &'a dyn SessionSource,
    /// Executable of a pid that is not in the scan (rare: a responsible
    /// process that just died, or isn't listed).
    pub exe_of: &'a dyn Fn(i32) -> Option<String>,
}

impl Classifier<'_> {
    pub fn run(&mut self, procs: &[ProcIn]) -> (Vec<Class>, Vec<Runtime>) {
        let by_pid: HashMap<i32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();

        // Rule 1 first: runtimes and their owners.
        let mut runtimes = Vec::new();
        let mut runtime_of_idx: HashMap<usize, usize> = HashMap::new();
        for (i, p) in procs.iter().enumerate() {
            let Some((agent, kind)) = p.facts.runtime.filter(|_| p.readable) else { continue };
            let owner = match kind {
                RuntimeKind::Session => self.project_of_cwd(&p.facts, true),
                RuntimeKind::AppServer => self.app_of(p, procs, &by_pid),
            };
            let (sessions, reported_idle) = match agent {
                Agent::Claude => match self.sessions.claude(p.pid) {
                    Some((id, idle)) => (vec![id], idle),
                    None => (Vec::new(), None),
                },
                Agent::Codex => (self.sessions.codex(p.pid), None),
            };
            runtime_of_idx.insert(i, runtimes.len());
            runtimes.push(Runtime { idx: i, agent, kind, owner, sessions, reported_idle });
        }
        let mut by_session: HashMap<(Agent, &str), usize> = HashMap::new();
        for (r, rt) in runtimes.iter().enumerate() {
            for s in &rt.sessions {
                by_session.insert((rt.agent, s.as_str()), r);
            }
        }

        let mut out: Vec<Option<Class>> = vec![None; procs.len()];
        let mut stack = Vec::new();
        for start in 0..procs.len() {
            // Resolve parents first, iteratively (no recursion, cycle-safe).
            let mut i = start;
            while out[i].is_none() {
                if stack.len() > 64 || stack.contains(&i) {
                    break;
                }
                stack.push(i);
                match self.parent_of(i, procs, &by_pid) {
                    Some(pi) if out[pi].is_none() => i = pi,
                    _ => break,
                }
            }
            while let Some(i) = stack.pop() {
                if out[i].is_some() {
                    continue;
                }
                let c = self.classify_one(i, procs, &by_pid, &out, &runtimes, &runtime_of_idx, &by_session);
                out[i] = Some(c);
            }
        }
        (out.into_iter().map(|c| c.unwrap_or(Class { owner: Owner::System, runtime: None })).collect(), runtimes)
    }

    fn parent_of(&self, i: usize, procs: &[ProcIn], by_pid: &HashMap<i32, usize>) -> Option<usize> {
        let p = &procs[i];
        if p.ppid <= 1 || p.ppid == p.pid {
            return None;
        }
        let pi = *by_pid.get(&p.ppid)?;
        // A parent that started after its child is a reused pid.
        (procs[pi].start_abs <= p.start_abs).then_some(pi)
    }

    #[allow(clippy::too_many_arguments)]
    fn classify_one(
        &mut self,
        i: usize,
        procs: &[ProcIn],
        by_pid: &HashMap<i32, usize>,
        done: &[Option<Class>],
        runtimes: &[Runtime],
        runtime_of_idx: &HashMap<usize, usize>,
        by_session: &HashMap<(Agent, &str), usize>,
    ) -> Class {
        let p = &procs[i];
        if !p.readable {
            return Class { owner: Owner::System, runtime: None };
        }
        // 1. runtime
        if let Some(&r) = runtime_of_idx.get(&i) {
            return Class { owner: runtimes[r].owner.clone(), runtime: Some(r) };
        }
        // 2. session marker
        if let Some((agent, id)) = &p.facts.marker
            && let Some(&r) = by_session.get(&(*agent, id.as_str()))
            && runtimes[r].owner.is_project()
        {
            return Class { owner: runtimes[r].owner.clone(), runtime: Some(r) };
        }
        // 3. parent
        if let Some(pi) = self.parent_of(i, procs, by_pid)
            && let Some(pc) = &done[pi]
            && pc.owner.is_project()
        {
            return pc.clone();
        }
        // 4. own cwd
        let owner = self.project_of_cwd(&p.facts, false);
        if owner != Owner::System {
            return Class { owner, runtime: None };
        }
        // 5. app
        Class { owner: self.app_of(p, procs, by_pid), runtime: None }
    }

    /// Worktree of the cwd; for agent runtimes, the plain folder otherwise.
    fn project_of_cwd(&mut self, f: &Facts, folder_ok: bool) -> Owner {
        let Some(cwd) = f.cwd.as_deref() else { return Owner::System };
        if let Some(root) = self.repos.root_of(cwd) {
            return Owner::Worktree(root.into());
        }
        if folder_ok && cwd != "/" {
            return Owner::Folder(cwd.trim_end_matches('/').into());
        }
        Owner::System
    }

    fn app_of(&self, p: &ProcIn, procs: &[ProcIn], by_pid: &HashMap<i32, usize>) -> Owner {
        let resp_exe = p.facts.responsible.filter(|&r| r != p.pid).and_then(|r| match by_pid.get(&r) {
            Some(&ri) => procs[ri].facts.exe.clone(),
            None => (self.exe_of)(r),
        });
        for exe in [resp_exe.as_deref(), p.facts.exe.as_deref()].into_iter().flatten() {
            if let Some(app) = outer_app(exe) {
                return Owner::App(app.into());
            }
        }
        Owner::System
    }
}

#[cfg(test)]
mod tests {
    use super::super::agents::{runtime_of, session_marker};
    use super::super::repo::tests::FakeFs;
    use super::*;

    /// Builds a scan by hand. Pids double as start order unless overridden.
    #[derive(Default)]
    struct Scan {
        procs: Vec<ProcIn>,
        claude: HashMap<i32, (String, Option<bool>)>,
        codex: HashMap<i32, Vec<String>>,
    }

    impl SessionSource for Scan {
        fn claude(&self, pid: i32) -> Option<(String, Option<bool>)> {
            self.claude.get(&pid).cloned()
        }
        fn codex(&self, pid: i32) -> Vec<String> {
            self.codex.get(&pid).cloned().unwrap_or_default()
        }
    }

    struct P<'a> {
        pid: i32,
        ppid: i32,
        exe: &'a str,
        argv: &'a [&'a str],
        cwd: &'a str,
        env: &'a [(&'a str, &'a str)],
        resp: Option<i32>,
    }

    impl Scan {
        fn add(&mut self, p: P) -> &mut Self {
            let argv: Vec<String> = p.argv.iter().map(|s| s.to_string()).collect();
            let env: Vec<(String, String)> = p.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let exe = (!p.exe.is_empty()).then(|| p.exe.to_string());
            let facts = Facts {
                name: super::super::repo::basename(p.exe).into(),
                runtime: runtime_of(exe.as_deref(), &argv),
                marker: session_marker(&env).map(|(a, id)| (a, id.to_string())),
                exe,
                cwd: (!p.cwd.is_empty()).then(|| p.cwd.to_string()),
                argv,
                env,
                responsible: p.resp,
                label: "".into(),
            };
            self.procs.push(ProcIn {
                pid: p.pid,
                ppid: p.ppid,
                start_abs: p.pid as u64 * 1000,
                readable: true,
                facts: Arc::new(facts),
            });
            self
        }

        fn unreadable(&mut self, pid: i32, ppid: i32) -> &mut Self {
            self.procs.push(ProcIn { pid, ppid, start_abs: pid as u64 * 1000, readable: false, facts: Arc::default() });
            self
        }

        fn run(&self, fs: FakeFs) -> (HashMap<i32, Class>, Vec<Runtime>) {
            let mut repos = RepoIndex::with_fs("/Users/me".into(), Box::new(fs));
            let exe_of = |_pid: i32| None;
            let mut c = Classifier { repos: &mut repos, sessions: self, exe_of: &exe_of };
            let (classes, rts) = c.run(&self.procs);
            (self.procs.iter().zip(classes).map(|(p, c)| (p.pid, c)).collect(), rts)
        }
    }

    fn wt(p: &str) -> Owner {
        Owner::Worktree(p.into())
    }
    fn app(p: &str) -> Owner {
        Owner::App(p.into())
    }

    const TERMINAL: &str = "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal";
    const ORCA: &str = "/Applications/Orca.app/Contents/MacOS/Orca";
    const CLAUDE: &str = "/Users/me/.local/share/claude/versions/2.1.332";

    fn repos() -> FakeFs {
        FakeFs::default()
            .repo("/Users/me/git/shop", "ref: refs/heads/main")
            .repo("/Users/me/w/shop-auth", "ref: refs/heads/feat/auth")
            .repo("/Users/me/git/blog", "ref: refs/heads/main")
    }

    fn p<'a>(pid: i32, ppid: i32, exe: &'a str, argv: &'a [&'a str], cwd: &'a str) -> P<'a> {
        P { pid, ppid, exe, argv, cwd, env: &[], resp: None }
    }

    #[test]
    fn claude_session_and_everything_it_runs_go_to_its_worktree() {
        let mut s = Scan::default();
        s.add(p(100, 1, ORCA, &["Orca"], "/"))
            .add(P { resp: Some(100), ..p(200, 100, "/Applications/Orca.app/Contents/MacOS/orca-provider-exec", &["orca-provider-exec"], "/Users/me/w/shop-auth") })
            .add(P { resp: Some(100), ..p(201, 200, CLAUDE, &["/Users/me/.local/bin/claude", "--resume=x"], "/Users/me/w/shop-auth") })
            // a shell Claude runs, cd'd elsewhere: parent wins over own cwd
            .add(P { resp: Some(100), ..p(202, 201, "/bin/zsh", &["/bin/zsh", "-c", "npm test"], "/Users/me/git/blog") })
            .add(P { resp: Some(100), ..p(203, 202, "/opt/homebrew/bin/node", &["node", "jest"], "/Users/me/git/blog") });
        s.claude.insert(201, ("sess-1".into(), Some(true)));
        let (c, rts) = s.run(repos());
        assert_eq!(c[&100].owner, app("/Applications/Orca.app"));
        assert_eq!(c[&200].owner, wt("/Users/me/w/shop-auth"), "own cwd: rule 4");
        for pid in [201, 202, 203] {
            assert_eq!(c[&pid].owner, wt("/Users/me/w/shop-auth"), "pid {pid}");
            assert_eq!(c[&pid].runtime, Some(0));
        }
        assert_eq!(rts.len(), 1);
        assert_eq!(rts[0].sessions, vec!["sess-1".to_string()]);
        assert_eq!(rts[0].reported_idle, Some(true));
    }

    #[test]
    fn a_daemonized_dev_server_follows_its_session_marker() {
        let mut s = Scan::default();
        s.add(p(300, 1, CLAUDE, &["claude"], "/Users/me/git/shop/web"))
            // parent died: reparented to launchd, cwd /, but carries the marker
            .add(P { env: &[("CLAUDE_CODE_SESSION_ID", "sess-9")], ..p(301, 1, "/opt/homebrew/bin/node", &["node", "server.js"], "/") });
        s.claude.insert(300, ("sess-9".into(), None));
        let (c, _) = s.run(repos());
        assert_eq!(c[&301].owner, wt("/Users/me/git/shop"));
        assert_eq!(c[&301].runtime, Some(0));
    }

    #[test]
    fn a_marker_of_an_unknown_session_falls_through() {
        let mut s = Scan::default();
        s.add(P { env: &[("CLAUDE_CODE_SESSION_ID", "gone")], ..p(301, 1, "/opt/homebrew/bin/node", &["node", "x.js"], "/Users/me/git/blog/src") });
        let (c, rts) = s.run(repos());
        assert!(rts.is_empty());
        assert_eq!(c[&301], Class { owner: wt("/Users/me/git/blog"), runtime: None });
    }

    #[test]
    fn claude_outside_git_gets_a_folder_group() {
        let mut s = Scan::default();
        s.add(p(10, 1, CLAUDE, &["claude"], "/Users/me/notes/")).add(p(11, 10, "/bin/zsh", &["zsh"], "/Users/me/notes"));
        let (c, _) = s.run(repos());
        assert_eq!(c[&10].owner, Owner::Folder("/Users/me/notes".into()));
        assert_eq!(c[&11].owner, Owner::Folder("/Users/me/notes".into()));
    }

    #[test]
    fn terminal_shell_in_home_is_the_terminal_app_but_its_dev_server_is_the_project() {
        let mut s = Scan::default();
        s.add(p(50, 1, TERMINAL, &["Terminal"], "/"))
            .add(P { resp: Some(50), ..p(51, 50, "/usr/bin/login", &["login", "-pf", "me"], "/Users/me") })
            .add(P { resp: Some(50), ..p(52, 51, "/bin/zsh", &["-zsh"], "/Users/me") })
            .add(P { resp: Some(50), ..p(53, 52, "/opt/homebrew/bin/node", &["node", "next", "dev"], "/Users/me/git/shop") })
            .add(P { resp: Some(50), ..p(54, 53, "/opt/homebrew/bin/node", &["node", "worker.js"], "/Users/me/git/shop") });
        let (c, _) = s.run(repos());
        let term = app("/System/Applications/Utilities/Terminal.app");
        assert_eq!(c[&51].owner, term);
        assert_eq!(c[&52].owner, term);
        assert_eq!(c[&53].owner, wt("/Users/me/git/shop"));
        assert_eq!(c[&54].owner, wt("/Users/me/git/shop"));
    }

    #[test]
    fn helpers_go_to_the_outermost_app() {
        let mut s = Scan::default();
        let chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        let helper = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/1/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)";
        s.add(p(60, 1, chrome, &["Google Chrome"], "/"))
            .add(P { resp: Some(60), ..p(61, 60, helper, &["helper"], "/") })
            // the Docker VM is launched by launchd; responsible is Docker Desktop
            .add(p(70, 1, "/Applications/Docker.app/Contents/MacOS/Docker Desktop.app/Contents/MacOS/Docker Desktop", &["Docker Desktop"], "/"))
            .add(P { resp: Some(70), ..p(71, 1, "/Applications/Docker.app/Contents/MacOS/com.docker.virtualization", &["com.docker.virtualization"], "/") });
        let (c, _) = s.run(repos());
        assert_eq!(c[&61].owner, app("/Applications/Google Chrome.app"));
        assert_eq!(c[&71].owner, app("/Applications/Docker.app"));
    }

    #[test]
    fn unreadable_and_unknown_processes_are_system() {
        let mut s = Scan::default();
        s.unreadable(400, 1).add(p(401, 400, "/usr/libexec/foo", &["foo"], "/")).add(p(402, 1, "", &[], ""));
        let (c, _) = s.run(repos());
        assert_eq!(c[&400].owner, Owner::System);
        assert_eq!(c[&401].owner, Owner::System);
        assert_eq!(c[&402].owner, Owner::System);
    }

    #[test]
    fn a_reused_parent_pid_is_not_a_parent() {
        let mut s = Scan::default();
        // 900 started after 20: it can't be 20's parent.
        s.add(p(900, 1, CLAUDE, &["claude"], "/Users/me/git/shop")).add(p(20, 900, "/usr/bin/caffeinate", &["caffeinate"], "/"));
        let (c, _) = s.run(repos());
        assert_eq!(c[&20].owner, Owner::System);
    }

    #[test]
    fn codex_terminal_session_and_its_sandboxed_commands() {
        let mut s = Scan::default();
        s.add(p(80, 1, TERMINAL, &["Terminal"], "/"))
            .add(P { resp: Some(80), ..p(81, 80, "/bin/zsh", &["-zsh"], "/Users/me") })
            .add(P { resp: Some(80), ..p(82, 81, "/opt/homebrew/bin/node", &["node", "/opt/homebrew/bin/codex"], "/Users/me/git/shop") })
            .add(P { resp: Some(80), ..p(83, 82, "/opt/homebrew/lib/node_modules/@openai/codex/vendor/aarch64-apple-darwin/codex/codex", &["codex"], "/Users/me/git/shop") })
            .add(P { resp: Some(80), env: &[("CODEX_SESSION_ID", "th-1")], ..p(84, 83, "/usr/bin/sandbox-exec", &["sandbox-exec", "-p", "…", "--", "zsh", "-lc", "cargo build"], "/Users/me/git/shop") })
            .add(P { resp: Some(80), env: &[("CODEX_SESSION_ID", "th-1")], ..p(85, 84, "/bin/zsh", &["zsh", "-lc", "cargo build"], "/Users/me/git/shop") });
        s.codex.insert(83, vec!["th-1".into()]);
        let (c, rts) = s.run(repos());
        assert_eq!(rts.len(), 1);
        assert_eq!(rts[0].agent, Agent::Codex);
        assert_eq!(rts[0].sessions, vec!["th-1".to_string()]);
        // The node wrapper is in the project by its cwd, not as a runtime.
        assert_eq!(c[&82], Class { owner: wt("/Users/me/git/shop"), runtime: None });
        for pid in [83, 84, 85] {
            assert_eq!(c[&pid], Class { owner: wt("/Users/me/git/shop"), runtime: Some(0) }, "pid {pid}");
        }
    }

    #[test]
    fn codex_app_server_belongs_to_the_app_and_its_commands_to_their_project() {
        let mut s = Scan::default();
        let app_exe = "/Applications/Codex.app/Contents/MacOS/Codex";
        s.add(p(90, 1, app_exe, &["Codex"], "/"))
            .add(P { resp: Some(90), ..p(91, 90, "/Applications/Codex.app/Contents/Resources/codex", &["codex", "app-server"], "/Users/me") })
            .add(P { resp: Some(90), env: &[("CODEX_SESSION_ID", "a")], ..p(92, 91, "/usr/bin/sandbox-exec", &["sandbox-exec"], "/Users/me/git/blog") })
            .add(P { resp: Some(90), env: &[("CODEX_SESSION_ID", "b")], ..p(93, 91, "/usr/bin/sandbox-exec", &["sandbox-exec"], "/Users/me/git/shop/api") });
        s.codex.insert(91, vec!["a".into(), "b".into()]);
        let (c, rts) = s.run(repos());
        assert_eq!(rts[0].kind, RuntimeKind::AppServer);
        assert_eq!(rts[0].owner, app("/Applications/Codex.app"));
        assert_eq!(c[&91].owner, app("/Applications/Codex.app"));
        assert_eq!(c[&92].owner, wt("/Users/me/git/blog"));
        assert_eq!(c[&93].owner, wt("/Users/me/git/shop"));
    }

    #[test]
    fn deep_chains_and_cycles_terminate() {
        let mut s = Scan::default();
        s.add(p(1000, 1, CLAUDE, &["claude"], "/Users/me/git/shop"));
        for pid in 1001..1200 {
            s.add(p(pid, pid - 1, "/bin/sh", &["sh"], "/"));
        }
        // A cycle (impossible in practice) must not hang.
        s.add(p(5000, 5001, "/bin/sh", &["sh"], "/")).add(p(5001, 5000, "/bin/sh", &["sh"], "/"));
        let (c, _) = s.run(repos());
        assert_eq!(c[&1199].owner, wt("/Users/me/git/shop"));
        assert_eq!(c[&5000].owner, Owner::System);
    }

    #[test]
    fn outer_app_paths() {
        assert_eq!(outer_app("/Applications/A.app/Contents/B.app/Contents/MacOS/b"), Some("/Applications/A.app"));
        assert_eq!(outer_app("/Applications/A.app"), Some("/Applications/A.app"));
        assert_eq!(outer_app("/usr/bin/zsh"), None);
        assert_eq!(outer_app("/Users/me/x.application/bin"), None);
    }
}
