//! Quitting a row: SIGTERM its processes and stop its docker containers,
//! then SIGKILL the processes still alive after a grace period.
//!
//! Runs on its own thread, never under the engine lock: it sleeps. Takes its
//! targets as plain data (from the last tick) so it is tested on processes
//! the tests spawn themselves.

use std::time::{Duration, Instant};

use crate::sys;

/// A process seen at the last tick. Its start time tells it apart from a
/// later process that reused its pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    /// `ri_proc_start_abstime`.
    pub start_abs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Targets {
    pub procs: Vec<Proc>,
    /// Docker container ids.
    pub containers: Vec<String>,
}

/// What a quit did. Recycled and refused pids count nowhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Processes and containers now gone.
    pub quit: u32,
    /// Processes we may not signal (EPERM).
    pub denied: u32,
    /// Processes and containers still alive at the end.
    pub left: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// After SIGTERM, before SIGKILL.
    pub grace: Duration,
    /// After SIGKILL, before giving up.
    pub kill_wait: Duration,
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { grace: Duration::from_secs(3), kill_wait: Duration::from_secs(1), poll: Duration::from_millis(200) }
    }
}

/// Never signalled: launchd and the kernel, ourselves and every process
/// above us (a terminal running the app from the build folder), and the
/// shells of terminal tabs. A tab's shell only sits in a worktree because
/// its cwd was there once; killing it closes the tab, and hangs up whatever
/// else runs in it, rows of other worktrees included.
pub fn refused(pid: i32) -> bool {
    pid <= 1 || above_us(pid) || sys::bsd_info(pid).is_some_and(|b| interactive_shell(&b.name, b.tty))
}

fn above_us(pid: i32) -> bool {
    let mut p = std::process::id() as i32;
    for _ in 0..64 {
        if p == pid {
            return true;
        }
        match sys::bsd_info(p) {
            Some(b) if p > 1 => p = b.ppid,
            _ => return false,
        }
    }
    false
}

const SHELLS: [&str; 10] = ["zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "elvish"];

/// A shell with a terminal: what a tab runs. Login shells are "-zsh".
fn interactive_shell(name: &str, tty: bool) -> bool {
    tty && SHELLS.contains(&name.trim_start_matches('-'))
}

/// Quits `t`. `stop_containers` stops docker containers and returns how many
/// are down; it runs alongside the signalling.
pub fn run(t: &Targets, timing: Timing, stop_containers: impl FnOnce(&[String]) -> u32 + Send) -> Outcome {
    std::thread::scope(|s| {
        let docker = (!t.containers.is_empty()).then(|| s.spawn(|| stop_containers(&t.containers)));
        let mut out = processes(&t.procs, timing);
        if let Some(d) = docker {
            let down = d.join().unwrap_or(0).min(t.containers.len() as u32);
            out.quit += down;
            out.left += t.containers.len() as u32 - down;
        }
        out
    })
}

fn processes(procs: &[Proc], timing: Timing) -> Outcome {
    let mut out = Outcome::default();
    let mut live = Vec::with_capacity(procs.len());
    for &p in procs.iter().filter(|p| !refused(p.pid)) {
        signal(p, libc::SIGTERM, &mut live, &mut out);
    }
    wait(&mut live, timing.grace, timing.poll, &mut out);
    for p in std::mem::take(&mut live) {
        signal(p, libc::SIGKILL, &mut live, &mut out);
    }
    wait(&mut live, timing.kill_wait, timing.poll, &mut out);
    out.left += live.len() as u32;
    out
}

/// Signals `p` if it is still the process we saw; keeps it in `live` when
/// the signal was delivered.
fn signal(p: Proc, sig: i32, live: &mut Vec<Proc>, out: &mut Outcome) {
    match state(p) {
        State::Gone => out.quit += 1,
        State::Recycled => {}
        State::Denied => out.denied += 1,
        State::Alive => {
            if unsafe { libc::kill(p.pid, sig) } == 0 {
                live.push(p);
            } else if sys::errno() == libc::EPERM {
                out.denied += 1;
            } else {
                out.quit += 1; // ESRCH: died in between
            }
        }
    }
}

/// Polls `live` every `poll` for up to `d`, moving the processes that ended
/// to `out.quit`.
fn wait(live: &mut Vec<Proc>, d: Duration, poll: Duration, out: &mut Outcome) {
    let end = Instant::now() + d;
    loop {
        live.retain(|&p| match state(p) {
            // A recycled pid means ours is gone, and so does one we may no
            // longer read: we could signal it a moment ago, so it is another
            // user's process now.
            State::Gone | State::Recycled | State::Denied => {
                out.quit += 1;
                false
            }
            State::Alive => true,
        });
        let now = Instant::now();
        if live.is_empty() || now >= end {
            return;
        }
        std::thread::sleep(poll.min(end - now));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Alive,
    /// Exited (a zombie counts: it only waits for its parent to reap it).
    Gone,
    /// The pid now belongs to another process.
    Recycled,
    /// Alive, but not ours to signal.
    Denied,
}

fn state(p: Proc) -> State {
    match sys::rusage(p.pid) {
        Ok(r) if r.start_abs != p.start_abs => State::Recycled,
        // rusage still answers for a zombie; the BSD info call does not.
        Ok(_) if sys::bsd_info(p.pid).is_none_or(|b| b.zombie) => State::Gone,
        Ok(_) => State::Alive,
        Err(libc::ESRCH) => State::Gone,
        // Another user's process now (a recycled pid, or a setuid exec): we
        // can't check who it is, and couldn't signal it anyway.
        Err(_) => State::Denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    const FAST: Timing = Timing { grace: Duration::from_millis(300), kill_wait: Duration::from_millis(500), poll: Duration::from_millis(20) };

    fn proc_of(pid: i32) -> Proc {
        let r = sys::rusage(pid).expect("rusage");
        Proc { pid, start_abs: r.start_abs }
    }

    fn sleeper() -> Child {
        Command::new("sleep").arg("60").spawn().unwrap()
    }

    /// A `sleep 60` that ignores SIGTERM (the ignored disposition survives exec).
    fn stubborn() -> Child {
        let mut c = Command::new("sleep");
        c.arg("60");
        unsafe {
            c.pre_exec(|| {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
                Ok(())
            });
        }
        c.spawn().unwrap()
    }

    fn targets(children: &[&Child]) -> Targets {
        Targets { procs: children.iter().map(|c| proc_of(c.id() as i32)).collect(), containers: vec![] }
    }

    fn no_docker(_: &[String]) -> u32 {
        panic!("no containers to stop")
    }

    #[test]
    fn sigterm_quits_processes() {
        let (mut a, mut b) = (sleeper(), sleeper());
        let t0 = Instant::now();
        let out = run(&targets(&[&a, &b]), FAST, no_docker);
        assert_eq!(out, Outcome { quit: 2, denied: 0, left: 0 });
        assert!(t0.elapsed() < FAST.grace, "no wait for the grace period: {:?}", t0.elapsed());
        for c in [&mut a, &mut b] {
            let st = c.wait().unwrap();
            assert_eq!(std::os::unix::process::ExitStatusExt::signal(&st), Some(libc::SIGTERM));
        }
    }

    #[test]
    fn a_process_ignoring_sigterm_is_killed_after_the_grace_period() {
        let mut c = stubborn();
        let t0 = Instant::now();
        let out = run(&targets(&[&c]), FAST, no_docker);
        assert_eq!(out, Outcome { quit: 1, denied: 0, left: 0 });
        assert!(t0.elapsed() >= FAST.grace);
        let st = c.wait().unwrap();
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&st), Some(libc::SIGKILL));
    }

    #[test]
    fn a_recycled_pid_is_left_alone() {
        let mut c = sleeper();
        let mut t = targets(&[&c]);
        t.procs[0].start_abs += 1;
        assert_eq!(run(&t, FAST, no_docker), Outcome::default());
        assert_eq!(c.try_wait().unwrap(), None, "still running");
        c.kill().unwrap();
        c.wait().unwrap();
    }

    #[test]
    fn launchd_ourselves_and_our_parent_are_refused() {
        let me = std::process::id() as i32;
        assert!(refused(1) && refused(0) && refused(-1) && refused(me) && refused(unsafe { libc::getppid() }));
        // Our own pid, correctly identified: nothing is sent, we are still here.
        let t = Targets { procs: vec![proc_of(me), Proc { pid: 1, start_abs: 0 }], containers: vec![] };
        assert_eq!(run(&t, FAST, no_docker), Outcome::default());
    }

    #[test]
    fn every_process_above_us_is_refused() {
        let mut p = unsafe { libc::getppid() };
        while p > 1 {
            assert!(refused(p), "{p} is above us");
            p = sys::bsd_info(p).unwrap().ppid;
        }
    }

    #[test]
    fn only_shells_with_a_terminal_are_spared() {
        assert!(interactive_shell("zsh", true) && interactive_shell("-zsh", true) && interactive_shell("bash", true));
        assert!(!interactive_shell("zsh", false), "a script's shell is part of the row");
        assert!(!interactive_shell("node", true) && !interactive_shell("claude", true));
    }

    /// A shell in a session of its own with a pseudo-terminal: a tab.
    fn tab_shell() -> (Child, i32) {
        let (mut master, mut slave) = (0, 0);
        let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(r, 0);
        let mut c = Command::new("sh");
        // The trailing `true` keeps sh alive rather than exec'ing sleep.
        c.args(["-c", "sleep 60; true"]);
        unsafe {
            c.pre_exec(move || {
                libc::setsid();
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = c.spawn().unwrap();
        unsafe { libc::close(slave) };
        (child, master)
    }

    #[test]
    fn a_tab_shell_is_never_signalled() {
        let (mut sh, master) = tab_shell();
        let pid = sh.id() as i32;
        let t0 = Instant::now();
        while !sys::bsd_info(pid).is_some_and(|b| b.name == "sh" && b.tty) {
            assert!(t0.elapsed() < Duration::from_secs(5), "shell never got its terminal");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(refused(pid));
        assert_eq!(run(&targets(&[&sh]), FAST, no_docker), Outcome::default());
        assert_eq!(sh.try_wait().unwrap(), None, "still running");
        sh.kill().unwrap();
        sh.wait().unwrap();
        unsafe { libc::close(master) };
    }

    #[test]
    fn an_exited_pid_counts_as_quit() {
        let mut c = Command::new("true").spawn().unwrap();
        let t = targets(&[&c]);
        c.wait().unwrap();
        assert_eq!(run(&t, FAST, no_docker), Outcome { quit: 1, denied: 0, left: 0 });
    }

    #[test]
    fn a_zombie_counts_as_gone() {
        let mut c = Command::new("true").spawn().unwrap();
        let p = proc_of(c.id() as i32);
        let t0 = Instant::now();
        while state(p) != State::Gone {
            assert!(t0.elapsed() < Duration::from_secs(5), "child never exited");
            std::thread::sleep(Duration::from_millis(10));
        }
        c.wait().unwrap();
    }

    #[test]
    fn another_users_process_is_denied_without_a_signal() {
        // state() only reads: it never signals launchd.
        assert_eq!(state(Proc { pid: 1, start_abs: 0 }), State::Denied);
    }

    #[test]
    fn containers_are_stopped_alongside_and_counted() {
        let mut c = sleeper();
        let mut t = targets(&[&c]);
        t.containers = vec!["aa".into(), "bb".into(), "cc".into()];
        let out = run(&t, FAST, |ids| {
            assert_eq!(ids.len(), 3);
            2
        });
        assert_eq!(out, Outcome { quit: 3, denied: 0, left: 1 });
        c.wait().unwrap();
    }
}
