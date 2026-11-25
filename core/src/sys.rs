//! Thin, safe wrappers over the macOS calls devload needs: libproc, sysctl and
//! mach. libc only, no subprocess. Every function returns `None`/`Err` instead
//! of panicking: a process can die between two calls.
#![allow(deprecated)] // libc marks mach_host_self & co. as deprecated in favour of the mach2 crate.

use std::ffi::CStr;
use std::mem::{MaybeUninit, size_of, zeroed};

pub const PATH_MAX: usize = libc::PROC_PIDPATHINFO_MAXSIZE as usize;

pub fn errno() -> i32 {
    unsafe { *libc::__error() }
}

// ---------------------------------------------------------------- clocks

/// Mach absolute time → nanoseconds. On Apple Silicon numer/denom = 125/3:
/// reading `ri_user_time` as nanoseconds would be 41.7 times too small.
#[derive(Clone, Copy, Debug)]
pub struct Timebase {
    numer: u32,
    denom: u32,
}

impl Timebase {
    pub fn host() -> Self {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        unsafe { libc::mach_timebase_info(&mut tb) };
        if tb.numer == 0 || tb.denom == 0 {
            return Timebase { numer: 1, denom: 1 };
        }
        Timebase { numer: tb.numer, denom: tb.denom }
    }

    #[cfg(test)]
    pub fn new(numer: u32, denom: u32) -> Self {
        Timebase { numer, denom }
    }

    pub fn to_ns(self, mach: u64) -> u64 {
        (mach as u128 * self.numer as u128 / self.denom as u128) as u64
    }
}

/// CPU time used by the calling thread, in nanoseconds.
pub fn thread_cpu_ns() -> u64 {
    let mut ts: libc::timespec = unsafe { zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

// ---------------------------------------------------------------- sysctl

pub fn sysctl_i32(name: &CStr) -> Option<i32> {
    let mut v: i32 = 0;
    let mut sz = size_of::<i32>();
    let r = unsafe {
        libc::sysctlbyname(name.as_ptr(), &mut v as *mut _ as *mut _, &mut sz, std::ptr::null_mut(), 0)
    };
    (r == 0 && sz == size_of::<i32>()).then_some(v)
}

pub fn sysctl_u64(name: &CStr) -> Option<u64> {
    let mut v: u64 = 0;
    let mut sz = size_of::<u64>();
    let r = unsafe {
        libc::sysctlbyname(name.as_ptr(), &mut v as *mut _ as *mut _, &mut sz, std::ptr::null_mut(), 0)
    };
    (r == 0 && sz == size_of::<u64>()).then_some(v)
}

/// `hw.memsize`, bytes.
pub fn total_memory() -> u64 {
    sysctl_u64(c"hw.memsize").unwrap_or(0)
}

/// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical.
pub fn kernel_pressure_level() -> Option<i32> {
    sysctl_i32(c"kern.memorystatus_vm_pressure_level")
}

/// `kern.memorystatus_level`: percentage of memory available (0–100).
pub fn memory_available_pct() -> Option<i32> {
    sysctl_i32(c"kern.memorystatus_level")
}

// ---------------------------------------------------------------- machine

#[derive(Clone, Copy, Debug, Default)]
pub struct VmStats {
    pub internal: u64,
    pub purgeable: u64,
    pub wired: u64,
    pub compressor: u64,
    pub page_size: u64,
}

impl VmStats {
    /// Memory used, as Activity Monitor computes it:
    /// (internal − purgeable + wired + compressor) × page size.
    pub fn used_bytes(&self) -> u64 {
        (self.internal.saturating_sub(self.purgeable) + self.wired + self.compressor) * self.page_size
    }
}

pub fn vm_stats() -> Option<VmStats> {
    let mut s: libc::vm_statistics64 = unsafe { zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let host = unsafe { libc::mach_host_self() };
    let r = unsafe { libc::host_statistics64(host, libc::HOST_VM_INFO64, &mut s as *mut _ as *mut _, &mut count) };
    if r != 0 {
        return None;
    }
    let page = unsafe { libc::vm_page_size } as u64;
    Some(VmStats {
        internal: s.internal_page_count as u64,
        purgeable: s.purgeable_count as u64,
        wired: s.wire_count as u64,
        compressor: s.compressor_page_count as u64,
        page_size: page,
    })
}

/// Cumulative CPU ticks of the whole machine: user, system, idle, nice.
pub fn cpu_ticks() -> Option<[u64; 4]> {
    let mut info: libc::host_cpu_load_info = unsafe { zeroed() };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    let host = unsafe { libc::mach_host_self() };
    let r = unsafe { libc::host_statistics(host, libc::HOST_CPU_LOAD_INFO, &mut info as *mut _ as *mut _, &mut count) };
    if r != 0 {
        return None;
    }
    let t = info.cpu_ticks;
    Some([t[0] as u64, t[1] as u64, t[2] as u64, t[3] as u64])
}

// ---------------------------------------------------------------- processes

/// Fills `buf` with every pid and returns how many. Grows `buf` when full.
pub fn list_pids(buf: &mut Vec<i32>) -> usize {
    if buf.is_empty() {
        buf.resize(4096, 0);
    }
    loop {
        let bytes = (buf.len() * size_of::<i32>()) as i32;
        let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr() as *mut _, bytes) };
        if n < 0 {
            return 0;
        }
        let n = n as usize;
        if n < buf.len() {
            return n;
        }
        let len = buf.len() * 2;
        buf.resize(len, 0);
    }
}

/// `rusage_info_v6` is 464 bytes; read as u64 words.
/// [0..2] uuid, 2 user_time, 3 system_time, 9 phys_footprint, 10 proc_start_abstime.
#[repr(C)]
struct RusageV6 {
    w: [u64; 58],
}
const RUSAGE_INFO_V6: libc::c_int = 6;
const RI_USER: usize = 2;
const RI_SYS: usize = 3;
const RI_FOOTPRINT: usize = 9;
const RI_START: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rusage {
    /// `ri_phys_footprint`, bytes: Activity Monitor's "Memory" column.
    pub footprint: u64,
    /// user + system time, mach units (convert with `Timebase`).
    pub cpu_mach: u64,
    /// `ri_proc_start_abstime`: with the pid, identifies a process across pid reuse.
    pub start_abs: u64,
}

/// `proc_pid_rusage(RUSAGE_INFO_V6)`. `Err(errno)`: EPERM for root and other
/// users' processes, ESRCH when the process is gone.
pub fn rusage(pid: i32) -> Result<Rusage, i32> {
    let mut r = MaybeUninit::<RusageV6>::zeroed();
    let rc = unsafe { libc::proc_pid_rusage(pid, RUSAGE_INFO_V6, r.as_mut_ptr() as *mut _) };
    if rc != 0 {
        return Err(errno());
    }
    let w = unsafe { r.assume_init() }.w;
    Ok(Rusage {
        footprint: w[RI_FOOTPRINT],
        cpu_mach: w[RI_USER].wrapping_add(w[RI_SYS]),
        start_abs: w[RI_START],
    })
}

#[derive(Clone, Debug)]
pub struct BsdInfo {
    pub ppid: i32,
    pub uid: u32,
    pub zombie: bool,
    pub name: String,
    /// Has a controlling terminal.
    pub tty: bool,
    /// Start time, Unix seconds.
    pub start_sec: u64,
}

const SZOMB: u32 = 5;

pub fn bsd_info(pid: i32) -> Option<BsdInfo> {
    let mut b = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let sz = size_of::<libc::proc_bsdinfo>() as i32;
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, b.as_mut_ptr() as *mut _, sz) };
    if r != sz {
        return None;
    }
    let b = unsafe { b.assume_init() };
    // pbi_name is the long name (2*MAXCOMLEN); empty for some processes, then pbi_comm.
    let mut name = c_chars(&b.pbi_name);
    if name.is_empty() {
        name = c_chars(&b.pbi_comm);
    }
    Some(BsdInfo {
        ppid: b.pbi_ppid as i32,
        uid: b.pbi_uid,
        zombie: b.pbi_status == SZOMB,
        name,
        tty: b.e_tdev != u32::MAX && b.e_tdev != 0,
        start_sec: b.pbi_start_tvsec,
    })
}

/// libc declares `vip_path` as `[[c_char; 32]; 32]` (MAXPATHLEN split in rows).
fn vip_path(p: &[[libc::c_char; 32]; 32]) -> &[libc::c_char] {
    p.as_flattened()
}

fn c_chars(chars: &[libc::c_char]) -> String {
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(chars.as_ptr() as *const u8, chars.len()) };
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Executable path. Works for processes of other users too.
pub fn pid_path(pid: i32, buf: &mut [u8; PATH_MAX]) -> Option<String> {
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut _, PATH_MAX as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// Current directory.
pub fn cwd(pid: i32, info: &mut libc::proc_vnodepathinfo) -> Option<String> {
    let sz = size_of::<libc::proc_vnodepathinfo>() as i32;
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, info as *mut _ as *mut _, sz) };
    if r != sz {
        return None;
    }
    let s = c_chars(vip_path(&info.pvi_cdir.vip_path));
    (!s.is_empty()).then_some(s)
}

/// What `KERN_PROCARGS2` gives: exec path, argv, and the whitelisted env vars.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcArgs {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub const MAX_ARGS: usize = 64;
pub const MAX_ARG_LEN: usize = 1024;

/// Reads argv and the environment variables named in `env_keys` (nothing else
/// is kept). `buf` is reused between calls; a process whose arguments exceed it
/// is read truncated, which only loses the end of its environment.
pub fn proc_args(pid: i32, buf: &mut [u8], env_keys: &[&str]) -> Option<ProcArgs> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut sz = buf.len();
    let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut _, &mut sz, std::ptr::null_mut(), 0) };
    if r != 0 {
        return None;
    }
    Some(parse_procargs2(&buf[..sz.min(buf.len())], env_keys))
}

/// Layout: argc (i32), exec path, NUL padding, argv[0..argc], env entries, each NUL-terminated.
pub fn parse_procargs2(data: &[u8], env_keys: &[&str]) -> ProcArgs {
    let mut out = ProcArgs::default();
    if data.len() < 4 {
        return out;
    }
    let argc = i32::from_ne_bytes([data[0], data[1], data[2], data[3]]).max(0) as usize;
    let mut i = 4;
    // exec path
    while i < data.len() && data[i] != 0 {
        i += 1;
    }
    // padding
    while i < data.len() && data[i] == 0 {
        i += 1;
    }
    let mut strings = data[i..].split(|&b| b == 0);
    for _ in 0..argc {
        let Some(s) = strings.next() else { return out };
        if out.argv.len() < MAX_ARGS {
            let s = &s[..s.len().min(MAX_ARG_LEN)];
            out.argv.push(String::from_utf8_lossy(s).into_owned());
        }
    }
    if env_keys.is_empty() {
        return out;
    }
    for s in strings {
        if s.is_empty() {
            break; // end of the environment
        }
        let Some(eq) = s.iter().position(|&b| b == b'=') else { continue };
        let key = &s[..eq];
        if let Some(k) = env_keys.iter().find(|k| k.as_bytes() == key) {
            let v = &s[eq + 1..];
            out.env.push((k.to_string(), String::from_utf8_lossy(&v[..v.len().min(256)]).into_owned()));
        }
    }
    out
}

unsafe extern "C" {
    /// libquarantine (in libSystem): the process macOS holds responsible for
    /// `pid` (Chrome for a Chrome Helper, Terminal for a shell). Returns `pid`
    /// itself, or -1, when there is none.
    fn responsibility_get_pid_responsible_for_pid(pid: libc::pid_t) -> libc::pid_t;
}

pub fn responsible_pid(pid: i32) -> Option<i32> {
    let r = unsafe { responsibility_get_pid_responsible_for_pid(pid) };
    (r > 0).then_some(r)
}

// ---------------------------------------------------------------- open files

const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

#[repr(C)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: libc::off_t,
    fi_type: i32,
    fi_guardflags: u32,
}

#[repr(C)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

/// Paths of the regular files `pid` has open (vnodes only). Used for a handful
/// of agent processes, every 30 s.
pub fn open_file_paths(pid: i32) -> Vec<String> {
    let mut out = Vec::new();
    let need = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return out;
    }
    let n = need as usize / size_of::<libc::proc_fdinfo>() + 16;
    let mut fds: Vec<libc::proc_fdinfo> = Vec::with_capacity(n);
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr() as *mut _,
            (n * size_of::<libc::proc_fdinfo>()) as i32,
        )
    };
    if got <= 0 {
        return out;
    }
    unsafe { fds.set_len(got as usize / size_of::<libc::proc_fdinfo>()) };
    let mut info = MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
    let sz = size_of::<VnodeFdInfoWithPath>() as i32;
    for fd in fds.iter().filter(|f| f.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32) {
        let r = unsafe { libc::proc_pidfdinfo(pid, fd.proc_fd, PROC_PIDFDVNODEPATHINFO, info.as_mut_ptr() as *mut _, sz) };
        if r == sz {
            let i = unsafe { info.assume_init_ref() };
            let p = c_chars(vip_path(&i.pvip.vip_path));
            if !p.is_empty() {
                out.push(p);
            }
        }
    }
    out
}

/// Footprint of this process, bytes.
pub fn self_footprint() -> u64 {
    rusage(std::process::id() as i32).map(|r| r.footprint).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn procargs_blob(exec: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
        let mut b = (argv.len() as i32).to_ne_bytes().to_vec();
        b.extend_from_slice(exec.as_bytes());
        b.extend_from_slice(&[0, 0, 0, 0]);
        for a in argv {
            b.extend_from_slice(a.as_bytes());
            b.push(0);
        }
        for e in env {
            b.extend_from_slice(e.as_bytes());
            b.push(0);
        }
        b.push(0);
        b
    }

    #[test]
    fn parses_procargs_and_keeps_only_whitelisted_env() {
        let blob = procargs_blob(
            "/usr/bin/node",
            &["node", "server.js", "--port", "3000"],
            &["PATH=/usr/bin", "CLAUDE_CODE_SESSION_ID=abc", "SECRET=x"],
        );
        let p = parse_procargs2(&blob, &["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"]);
        assert_eq!(p.argv, vec!["node", "server.js", "--port", "3000"]);
        assert_eq!(p.env, vec![("CLAUDE_CODE_SESSION_ID".to_string(), "abc".to_string())]);
    }

    #[test]
    fn truncated_procargs_never_panics() {
        let blob = procargs_blob("/bin/zsh", &["zsh", "-c", "echo"], &["A=b"]);
        for cut in 0..blob.len() {
            let _ = parse_procargs2(&blob[..cut], &["A"]);
        }
    }

    #[test]
    fn mach_units_convert_like_apple_silicon() {
        assert_eq!(Timebase::new(125, 3).to_ns(24_000_000), 1_000_000_000);
    }

    #[test]
    fn reads_this_process() {
        let me = std::process::id() as i32;
        let r = rusage(me).expect("rusage of self");
        assert!(r.footprint > 0 && r.start_abs > 0);
        let b = bsd_info(me).expect("bsd info of self");
        assert!(!b.zombie);
        let mut path = [0u8; PATH_MAX];
        assert!(pid_path(me, &mut path).is_some());
        let mut v: libc::proc_vnodepathinfo = unsafe { zeroed() };
        assert!(cwd(me, &mut v).is_some());
        let mut buf = vec![0u8; 64 * 1024];
        let a = proc_args(me, &mut buf, &[]).expect("own args");
        assert!(!a.argv.is_empty());
        assert!(vm_stats().unwrap().used_bytes() > 0);
        assert!(total_memory() > 0);
        assert!(cpu_ticks().is_some());
        assert!(kernel_pressure_level().is_some());
        assert!(memory_available_pct().is_some());
    }

    #[test]
    fn lists_own_open_files_and_responsible_pid() {
        let dir = std::env::temp_dir().join(format!("devload-sys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("thread.lock");
        let _f = std::fs::File::create(&path).unwrap();
        let me = std::process::id() as i32;
        let canon = std::fs::canonicalize(&path).unwrap();
        let open = open_file_paths(me);
        assert!(open.iter().any(|p| std::path::Path::new(p) == canon), "{open:?}");
        assert!(responsible_pid(me).is_some());
        let mut pids = Vec::new();
        let n = list_pids(&mut pids);
        assert!(n > 10 && pids[..n].contains(&me));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn launchd_is_not_readable() {
        assert_eq!(rusage(1), Err(libc::EPERM));
    }
}
