//! Docker stacks: the real memory and CPU of each compose project (or lone
//! container), read from the Docker Engine API over its Unix socket.
//!
//! The engine calls [`Docker::scan`] every 30 s on its own thread. A scan is
//! one `GET /containers/json` and one one-shot `GET /containers/{id}/stats` per
//! running container, all on one kept-alive connection. When Docker is not
//! running a scan costs one failed `connect`, and is not retried for
//! [`RETRY_AFTER`].

mod http;
mod socket;
mod vm;

pub use socket::resolve_socket;
pub use vm::{Provider, engine_vms, is_vm_process};

use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const API: &str = "/v1.41";
/// After a failed scan, the next attempts return None without touching the
/// socket until this much time has passed. A bit under the 30 s cadence, so
/// the engine's next regular call does try again.
pub const RETRY_AFTER: Duration = Duration::from_secs(25);

const LABEL_PROJECT: &str = "com.docker.compose.project";
const LABEL_WORKING_DIR: &str = "com.docker.compose.project.working_dir";
const LABEL_SERVICE: &str = "com.docker.compose.service";

/// A compose project, or a container started without compose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    /// `"<project>@<working_dir>"` (working_dir empty if the label is
    /// missing) or `"container:<name>"`.
    pub key: String,
    /// Compose project name, or container name.
    pub name: String,
    pub working_dir: Option<String>,
    /// Compose services with at least one running container, sorted, unique.
    /// Empty for a lone container.
    pub services: Vec<String>,
    /// Bytes: sum over containers of `usage − inactive_file`.
    pub mem: u64,
    /// Per-mille of one core, summed over containers.
    pub cpu_pm: u32,
    /// Ids of its running containers, sorted: what quitting the stack stops.
    pub containers: Vec<String>,
}

/// Previous cumulative CPU counter of a container.
#[derive(Clone, Copy, Debug)]
struct CpuMark {
    total_ns: u64,
    at: Instant,
}

/// One running container, as measured by a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sample {
    id: String,
    name: String,
    project: Option<String>,
    working_dir: Option<String>,
    service: Option<String>,
    mem: u64,
    cpu_total_ns: u64,
    at: Instant,
}

pub struct Docker {
    client: Option<http::Client>,
    prev_cpu: HashMap<String, CpuMark>,
    next_try: Option<Instant>,
}

impl Docker {
    /// `socket`: from [`resolve_socket`]. None (no supported endpoint) makes
    /// every scan return None.
    pub fn new(socket: Option<PathBuf>) -> Self {
        Docker { client: socket.map(|p| http::Client::new(&p)), prev_cpu: HashMap::new(), next_try: None }
    }

    /// None = Docker not reachable (stopped, or not answering in time).
    /// Some(vec![]) = running, no containers. Stacks are sorted by memory,
    /// biggest first.
    pub fn scan(&mut self) -> Option<Vec<Stack>> {
        let started = Instant::now();
        if self.next_try.is_some_and(|t| started < t) {
            return None;
        }
        let client = self.client.as_mut()?;
        match collect(client) {
            Ok(samples) => {
                self.next_try = None;
                Some(self.account(samples))
            }
            Err(_) => {
                client.close();
                self.next_try = Some(started + RETRY_AFTER);
                None
            }
        }
    }

    /// Turns samples into stacks, diffing CPU counters against the previous
    /// scan. Containers no longer running are forgotten.
    fn account(&mut self, samples: Vec<Sample>) -> Vec<Stack> {
        let mut seen = HashMap::with_capacity(samples.len());
        let mut stacks: BTreeMap<String, Stack> = BTreeMap::new();
        for s in samples {
            let cpu_pm = cpu_per_mille(self.prev_cpu.get(&s.id).copied(), s.cpu_total_ns, s.at);
            seen.insert(s.id.clone(), CpuMark { total_ns: s.cpu_total_ns, at: s.at });

            let (key, name, working_dir, service) = match s.project {
                Some(project) => {
                    let wd = s.working_dir.unwrap_or_default();
                    let key = format!("{project}@{wd}");
                    (key, project, (!wd.is_empty()).then_some(wd), s.service)
                }
                None => (format!("container:{}", s.name), s.name, None, None),
            };
            let st = stacks.entry(key.clone()).or_insert_with(|| Stack {
                key,
                name,
                working_dir,
                services: Vec::new(),
                mem: 0,
                cpu_pm: 0,
                containers: Vec::new(),
            });
            st.mem = st.mem.saturating_add(s.mem);
            st.cpu_pm = st.cpu_pm.saturating_add(cpu_pm);
            st.containers.push(s.id);
            if let Some(svc) = service {
                st.services.push(svc);
            }
        }
        self.prev_cpu = seen;

        let mut out: Vec<Stack> = stacks.into_values().collect();
        for st in &mut out {
            st.services.sort();
            st.services.dedup();
            st.containers.sort();
        }
        out.sort_by(|a, b| b.mem.cmp(&a.mem).then_with(|| a.key.cmp(&b.key)));
        out
    }
}

/// Stacks' memory can't exceed the Docker VM's footprint: scale all stacks
/// down proportionally if their sum is above it. Returns the remainder
/// (vm − sum), which stays with the VM process (in its app's row).
pub fn cap_to_vm(stacks: &mut [Stack], vm_footprint: u64) -> u64 {
    let sum: u128 = stacks.iter().map(|s| s.mem as u128).sum();
    if sum > vm_footprint as u128 {
        for s in stacks.iter_mut() {
            s.mem = (s.mem as u128 * vm_footprint as u128 / sum) as u64;
        }
    }
    let after: u64 = stacks.iter().map(|s| s.mem).sum();
    vm_footprint.saturating_sub(after)
}

/// CPU used between two readings of a cumulative counter, in per-mille of one
/// core. First sight, a counter that went backwards (container restarted) or
/// no elapsed time → 0.
fn cpu_per_mille(prev: Option<CpuMark>, total_ns: u64, at: Instant) -> u32 {
    let Some(prev) = prev else { return 0 };
    let wall_ns = at.saturating_duration_since(prev.at).as_nanos();
    if wall_ns == 0 || total_ns < prev.total_ns {
        return 0;
    }
    let pm = (total_ns - prev.total_ns) as u128 * 1000 / wall_ns;
    pm.min(u32::MAX as u128) as u32
}

// ------------------------------------------------------------------ API

/// Seconds Docker waits after its SIGTERM before it kills a container.
pub const STOP_GRACE_SECS: u64 = 5;

/// Stops these containers, all at once (one connection each), the way
/// `docker stop -t 5` does. Returns how many are down: stopped now, already
/// stopped, or gone.
pub fn stop_containers(socket: &Path, ids: &[String]) -> u32 {
    std::thread::scope(|s| {
        let running: Vec<_> = ids.iter().map(|id| s.spawn(move || stop_container(&mut http::Client::new(socket), id))).collect();
        running.into_iter().map(|t| t.join().unwrap_or(false) as u32).sum()
    })
}

fn stop_container(client: &mut http::Client, id: &str) -> bool {
    if !is_safe_id(id) {
        return false;
    }
    // Docker answers once the container is down: up to the grace period, plus
    // the kill.
    let timeout = Duration::from_secs(STOP_GRACE_SECS) + http::IO_TIMEOUT * 2;
    client.post(&format!("{API}/containers/{id}/stop?t={STOP_GRACE_SECS}"), timeout).is_ok_and(|r| is_down(r.status))
}

/// 204 stopped, 304 already stopped, 404 no such container.
fn is_down(status: u16) -> bool {
    matches!(status, 204 | 304 | 404)
}

/// Any error here means "Docker not reachable" for this scan, except a non-200
/// status or unparsable stats on one container, which only skip it (it may
/// have stopped between the list and its stats).
fn collect(client: &mut http::Client) -> Result<Vec<Sample>, http::HttpError> {
    let list = client.get(&format!("{API}/containers/json"))?;
    let containers =
        parse_list(&list).map_err(|_| http::HttpError::Malformed("containers/json is not the expected JSON"))?;
    let mut samples = Vec::with_capacity(containers.len());
    for c in containers {
        if !is_safe_id(&c.id) {
            continue;
        }
        let body = match client.get(&format!("{API}/containers/{}/stats?stream=false&one-shot=true", c.id)) {
            Ok(b) => b,
            Err(http::HttpError::Status(_)) => continue,
            Err(e) => return Err(e),
        };
        let at = Instant::now();
        let Some((mem, cpu_total_ns)) = parse_stats(&body) else { continue };
        samples.push(Sample {
            id: c.id,
            name: c.name,
            project: c.project,
            working_dir: c.working_dir,
            service: c.service,
            mem,
            cpu_total_ns,
            at,
        });
    }
    Ok(samples)
}

/// Container ids are hex; anything else must not reach a request path.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[derive(Debug, PartialEq, Eq)]
struct ContainerInfo {
    id: String,
    name: String,
    project: Option<String>,
    working_dir: Option<String>,
    service: Option<String>,
}

fn parse_list(body: &[u8]) -> Result<Vec<ContainerInfo>, serde_json::Error> {
    #[derive(Deserialize)]
    struct Summary {
        #[serde(rename = "Id")]
        id: String,
        #[serde(rename = "Names", default)]
        names: Option<Vec<String>>,
        #[serde(rename = "Labels", default)]
        labels: Option<HashMap<String, String>>,
    }
    let list: Vec<Summary> = serde_json::from_slice(body)?;
    Ok(list
        .into_iter()
        .map(|s| {
            let mut labels = s.labels.unwrap_or_default();
            let mut take = |k: &str| labels.remove(k).filter(|v| !v.is_empty());
            let project = take(LABEL_PROJECT);
            let working_dir = take(LABEL_WORKING_DIR);
            let service = take(LABEL_SERVICE);
            let name = s
                .names
                .unwrap_or_default()
                .into_iter()
                .next()
                .map(|n| n.trim_start_matches('/').to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| s.id.chars().take(12).collect());
            ContainerInfo { id: s.id, name, project, working_dir, service }
        })
        .collect())
}

/// One-shot stats → (memory bytes, cumulative CPU ns).
///
/// Memory is what `docker stats` shows: usage minus the inactive page cache,
/// `total_inactive_file` on cgroup v1 (it covers the whole hierarchy), else
/// `inactive_file` (cgroup v2), floored at 0. A container whose stats are
/// empty (it just stopped) reads as 0.
fn parse_stats(body: &[u8]) -> Option<(u64, u64)> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Stats {
        memory_stats: MemoryStats,
        cpu_stats: CpuStats,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct MemoryStats {
        usage: u64,
        stats: MemInner,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct MemInner {
        inactive_file: Option<u64>,
        total_inactive_file: Option<u64>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct CpuStats {
        cpu_usage: CpuUsage,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct CpuUsage {
        total_usage: u64,
    }
    let s: Stats = serde_json::from_slice(body).ok()?;
    let m = &s.memory_stats;
    let inactive = m.stats.total_inactive_file.or(m.stats.inactive_file).unwrap_or(0);
    Some((m.usage.saturating_sub(inactive), s.cpu_stats.cpu_usage.total_usage))
}

#[cfg(test)]
mod tests;
