//! Coding agents: recognising their runtime processes, and their sessions.
//!
//! One independent rule per agent. Adding another agent later (Gemini CLI sets
//! GEMINI_CLI=1, opencode OPENCODE_SESSION_ID) means adding a rule here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::repo::basename;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }
}

/// How a runtime process is attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeKind {
    /// A terminal or headless session: belongs to the project of its cwd.
    Session,
    /// Codex's app-server (Codex desktop app, IDE extension): belongs to the
    /// app that launched it; may hold several sessions.
    AppServer,
}

/// Environment variables read from every process (nothing else is kept).
pub const ENV_KEYS: [&str; 3] = ["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID", "CODEX_THREAD_ID"];

/// Is this process an agent runtime (the agent itself, not a command it runs)?
pub fn runtime_of(exe: Option<&str>, argv: &[String]) -> Option<(Agent, RuntimeKind)> {
    let arg0 = argv.first().map(|a| basename(a.trim_start_matches('-'))).unwrap_or("");
    // Claude Code: the native binary (~/.local/share/claude/versions/2.1.332,
    // run through a `claude` symlink), or the npm package under node.
    let exe_is_claude = exe.is_some_and(|e| e.contains("/claude/versions/") || basename(e) == "claude");
    if arg0 == "claude" || exe_is_claude {
        return Some((Agent::Claude, RuntimeKind::Session));
    }
    if matches!(arg0, "node" | "bun")
        && argv.get(1).is_some_and(|a| a.contains("@anthropic-ai/claude-code"))
    {
        return Some((Agent::Claude, RuntimeKind::Session));
    }
    // Codex: the native binary (Homebrew, npm's vendored binary, or
    // Codex.app/Contents/Resources/codex; the Homebrew cask binary is
    // `codex-aarch64-apple-darwin` behind a `codex` symlink). The `node …/bin/codex.js` wrapper
    // is ignored: its child is the real runtime.
    let exe_name = exe.map(basename).unwrap_or("");
    let codex_exe = exe_name == "codex" || (exe_name.starts_with("codex-") && exe_name.ends_with("-apple-darwin"));
    if codex_exe {
        return match argv.get(1).map(String::as_str) {
            Some("app-server") => Some((Agent::Codex, RuntimeKind::AppServer)),
            // Subcommands that are not a session (login, mcp, completion…)
            // still count as the agent: they're short-lived and harmless.
            _ => Some((Agent::Codex, RuntimeKind::Session)),
        };
    }
    None
}

/// The agent session a command was started by, from its environment.
pub fn session_marker(env: &[(String, String)]) -> Option<(Agent, &str)> {
    let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    if let Some(id) = get("CLAUDE_CODE_SESSION_ID") {
        return Some((Agent::Claude, id));
    }
    get("CODEX_SESSION_ID").or_else(|| get("CODEX_THREAD_ID")).map(|id| (Agent::Codex, id))
}

// ------------------------------------------------------------ Claude registry

/// One `~/.claude/sessions/<pid>.json`, written by Claude Code for each live
/// session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeSession {
    pub pid: i32,
    pub session_id: String,
    pub cwd: Option<String>,
    /// "idle" / "busy" when Claude Code reports it.
    pub status: Option<String>,
    /// Process start, as `ps -o lstart` prints it ("Tue Nov 24 18:41:52 2026").
    pub proc_start: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionFile {
    pid: i32,
    session_id: String,
    cwd: Option<String>,
    status: Option<String>,
    proc_start: Option<String>,
}

pub fn parse_claude_session(json: &[u8]) -> Option<ClaudeSession> {
    let f: SessionFile = serde_json::from_slice(json).ok()?;
    Some(ClaudeSession { pid: f.pid, session_id: f.session_id, cwd: f.cwd, status: f.status, proc_start: f.proc_start })
}

/// The registry, re-read when the directory changes (and at most every tick).
pub struct ClaudeRegistry {
    dir: PathBuf,
    dir_mtime: Option<SystemTime>,
    /// File mtime per pid, to re-read only changed files (status changes).
    files: HashMap<i32, (SystemTime, ClaudeSession)>,
}

impl ClaudeRegistry {
    pub fn new(claude_home: &Path) -> Self {
        ClaudeRegistry { dir: claude_home.join("sessions"), dir_mtime: None, files: HashMap::new() }
    }

    /// Refreshes from disk. Cheap when nothing changed: one stat of the
    /// directory plus one stat per session file (status changes rewrite the
    /// file in place, which doesn't touch the directory mtime).
    pub fn refresh(&mut self) {
        let mtime = std::fs::metadata(&self.dir).and_then(|m| m.modified()).ok();
        if mtime.is_none() {
            self.files.clear();
            self.dir_mtime = None;
            return;
        }
        if mtime != self.dir_mtime {
            self.dir_mtime = mtime;
            let mut seen = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&self.dir) {
                for e in rd.flatten() {
                    let name = e.file_name();
                    let Some(pid) = name.to_str().and_then(|n| n.strip_suffix(".json")).and_then(|p| p.parse().ok())
                    else {
                        continue;
                    };
                    seen.push(pid);
                    if !self.files.contains_key(&pid) {
                        self.load(pid);
                    }
                }
            }
            self.files.retain(|pid, _| seen.contains(pid));
        }
        let stale: Vec<i32> = self
            .files
            .iter()
            .filter(|(pid, (m, _))| self.file_mtime(**pid).is_some_and(|now| now != *m))
            .map(|(pid, _)| *pid)
            .collect();
        for pid in stale {
            self.load(pid);
        }
    }

    fn path(&self, pid: i32) -> PathBuf {
        self.dir.join(format!("{pid}.json"))
    }

    fn file_mtime(&self, pid: i32) -> Option<SystemTime> {
        std::fs::metadata(self.path(pid)).and_then(|m| m.modified()).ok()
    }

    fn load(&mut self, pid: i32) {
        let p = self.path(pid);
        let (Ok(meta), Ok(bytes)) = (std::fs::metadata(&p), std::fs::read(&p)) else {
            self.files.remove(&pid);
            return;
        };
        match (meta.modified(), parse_claude_session(&bytes)) {
            (Ok(m), Some(s)) if s.pid == pid => {
                self.files.insert(pid, (m, s));
            }
            _ => {
                self.files.remove(&pid);
            }
        }
    }

    /// The session of a live claude process. `lstart` is the process start
    /// formatted like `ps -o lstart`, to reject a file left by a dead process
    /// whose pid was reused.
    pub fn by_pid(&self, pid: i32, lstart: Option<&str>) -> Option<&ClaudeSession> {
        let s = &self.files.get(&pid)?.1;
        match (&s.proc_start, lstart) {
            (Some(a), Some(b)) if !same_words(a, b) => None,
            _ => Some(s),
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn same_words(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

/// Formats a Unix time like `ps -o lstart` / ctime: "Tue Nov 24 18:41:52 2026"
/// (local time).
pub fn lstart(unix: u64) -> String {
    let t = unix as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return String::new();
    }
    let mut buf = [0u8; 64];
    let n = unsafe { libc::strftime(buf.as_mut_ptr() as *mut _, buf.len(), c"%a %b %e %H:%M:%S %Y".as_ptr(), &tm) };
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

// ------------------------------------------------------------ Codex locks

/// `$CODEX_HOME/thread-writer-locks`: a Codex runtime keeps `<thread id>.lock`
/// open for each session it is running.
pub fn codex_lock_dir(env_codex_home: Option<&str>, home: &str) -> String {
    match env_codex_home {
        Some(h) if !h.is_empty() => format!("{}/thread-writer-locks/", h.trim_end_matches('/')),
        _ => format!("{home}/.codex/thread-writer-locks/"),
    }
}

/// Session ids from a Codex runtime's open files.
pub fn codex_sessions_from_open_files(paths: &[String], lock_dir: &str) -> Vec<String> {
    let mut ids: Vec<String> = paths
        .iter()
        .filter_map(|p| p.strip_prefix(lock_dir))
        .filter_map(|f| f.strip_suffix(".lock"))
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recognises_claude_runtimes() {
        let native = Some("/Users/me/.local/share/claude/versions/2.1.332");
        assert_eq!(runtime_of(native, &v(&["claude"])), Some((Agent::Claude, RuntimeKind::Session)));
        assert_eq!(
            runtime_of(native, &v(&["/Users/me/.local/bin/claude", "--resume=x"])),
            Some((Agent::Claude, RuntimeKind::Session))
        );
        assert_eq!(
            runtime_of(
                Some("/opt/homebrew/bin/node"),
                &v(&["node", "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js"])
            ),
            Some((Agent::Claude, RuntimeKind::Session))
        );
        // Commands run by Claude are not runtimes.
        assert_eq!(runtime_of(Some("/bin/zsh"), &v(&["/bin/zsh", "-c", "claude --help"])), None);
        assert_eq!(runtime_of(Some("/opt/homebrew/bin/node"), &v(&["node", "server.js"])), None);
        // The Claude desktop app is an app, not a runtime.
        assert_eq!(runtime_of(Some("/Applications/Claude.app/Contents/MacOS/Claude"), &v(&["Claude"])), None);
    }

    #[test]
    fn recognises_codex_runtimes() {
        let cask = Some("/opt/homebrew/Caskroom/codex/0.80.0/codex-aarch64-apple-darwin");
        assert_eq!(runtime_of(Some("/opt/homebrew/bin/codex"), &v(&["codex"])), Some((Agent::Codex, RuntimeKind::Session)));
        assert_eq!(runtime_of(cask, &v(&["codex"])), Some((Agent::Codex, RuntimeKind::Session)));
        assert_eq!(runtime_of(Some("/usr/bin/vim"), &v(&["vim", "codex.md"])), None);
        assert_eq!(runtime_of(Some("/Users/me/git/codex-notes/run.sh"), &v(&["run.sh"])), None);
        assert_eq!(runtime_of(Some("/bin/sh"), &v(&["codex", "x"])), None, "a script named codex");
        assert_eq!(
            runtime_of(Some("/usr/local/lib/node_modules/@openai/codex/vendor/aarch64-apple-darwin/codex/codex"), &v(&["codex", "exec", "fix it"])),
            Some((Agent::Codex, RuntimeKind::Session))
        );
        assert_eq!(
            runtime_of(Some("/Applications/Codex.app/Contents/Resources/codex"), &v(&["codex", "app-server"])),
            Some((Agent::Codex, RuntimeKind::AppServer))
        );
        // The node wrapper is ignored.
        assert_eq!(runtime_of(Some("/opt/homebrew/bin/node"), &v(&["node", "/opt/homebrew/bin/codex.js"])), None);
    }

    #[test]
    fn session_markers() {
        let env = vec![("CODEX_THREAD_ID".to_string(), "t1".to_string())];
        assert_eq!(session_marker(&env), Some((Agent::Codex, "t1")));
        let env = vec![
            ("CODEX_THREAD_ID".to_string(), "t1".to_string()),
            ("CODEX_SESSION_ID".to_string(), "s1".to_string()),
        ];
        assert_eq!(session_marker(&env), Some((Agent::Codex, "s1")));
        let env = vec![("CLAUDE_CODE_SESSION_ID".to_string(), "c1".to_string())];
        assert_eq!(session_marker(&env), Some((Agent::Claude, "c1")));
        assert_eq!(session_marker(&[]), None);
    }

    #[test]
    fn parses_a_real_shaped_session_file() {
        let json = br#"{"pid":25587,"sessionId":"bc71d8a6","cwd":"/Users/me/w/app","startedAt":1795602854693,
            "procStart":"Wed Nov 25 10:34:14 2026","version":"2.1.332","kind":"interactive","status":"busy"}"#;
        let s = parse_claude_session(json).unwrap();
        assert_eq!(s.pid, 25587);
        assert_eq!(s.session_id, "bc71d8a6");
        assert_eq!(s.status.as_deref(), Some("busy"));
        assert!(parse_claude_session(b"{\"peerToken\":\"x\"}").is_none());
    }

    #[test]
    fn registry_reads_dir_and_rejects_reused_pids() {
        let dir = std::env::temp_dir().join(format!("devload-claude-{}", std::process::id()));
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("42.json"),
            r#"{"pid":42,"sessionId":"s42","cwd":"/w/a","procStart":"Tue Nov  4 08:01:02 2026","status":"idle"}"#,
        )
        .unwrap();
        std::fs::write(sessions.join("42.abc.key"), "{}").unwrap();
        let mut r = ClaudeRegistry::new(&dir);
        r.refresh();
        assert_eq!(r.len(), 1);
        assert_eq!(r.by_pid(42, Some("Tue Nov 4 08:01:02 2026")).unwrap().session_id, "s42");
        assert!(r.by_pid(42, Some("Wed Nov 5 08:01:02 2026")).is_none(), "pid reused");
        assert!(r.by_pid(42, None).is_some());
        std::fs::remove_file(sessions.join("42.json")).unwrap();
        // Directory mtime granularity: force a re-read.
        r.dir_mtime = None;
        r.refresh();
        assert!(r.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lstart_matches_ps_format() {
        let _tz = crate::TZ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let s = lstart(0);
        assert_eq!(s.split_whitespace().count(), 5, "{s}");
        // Real check against ps for this very process.
        let me = std::process::id() as i32;
        let b = crate::sys::bsd_info(me).unwrap();
        let ps = std::process::Command::new("ps").args(["-o", "lstart=", "-p", &me.to_string()]).output().unwrap();
        assert!(same_words(&lstart(b.start_sec), &String::from_utf8_lossy(&ps.stdout)));
    }

    #[test]
    fn codex_lock_files() {
        let dir = codex_lock_dir(None, "/Users/me");
        assert_eq!(dir, "/Users/me/.codex/thread-writer-locks/");
        assert_eq!(codex_lock_dir(Some("/x/codex/"), "/Users/me"), "/x/codex/thread-writer-locks/");
        let open = v(&[
            "/Users/me/.codex/thread-writer-locks/0199a-b.lock",
            "/Users/me/.codex/thread-writer-locks/0199a-c.lock",
            "/Users/me/.codex/thread-writer-locks/0199a-b.lock",
            "/Users/me/.codex/log/codex-tui.log",
            "/Users/me/.codex/thread-writer-locks/sub/x.lock",
        ]);
        assert_eq!(codex_sessions_from_open_files(&open, &dir), v(&["0199a-b", "0199a-c"]));
    }
}
