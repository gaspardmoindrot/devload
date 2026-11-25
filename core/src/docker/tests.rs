use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/docker/", $name))
    };
}

const LIST: &[u8] = fixture!("containers.json");
const STATS_V2: &[u8] = fixture!("stats_v2.json");
const STATS_V1: &[u8] = fixture!("stats_v1.json");
const STATS_STOPPED: &[u8] = fixture!("stats_stopped.json");

const MIB: u64 = 1 << 20;
const V2_MEM: u64 = 152_043_520 - 104_857_600; // usage − inactive_file
const V1_MEM: u64 = 6_537_216 - 1_048_576; // usage − total_inactive_file

// ------------------------------------------------------------------ parsing

#[test]
fn list_reads_names_and_compose_labels() {
    let l = parse_list(LIST).unwrap();
    assert_eq!(l.len(), 6);
    assert_eq!(
        l[0],
        ContainerInfo {
            id: "a1".repeat(32),
            name: "shop-postgres-1".into(),
            project: Some("shop".into()),
            working_dir: Some("/Users/dev/code/shop".into()),
            service: Some("postgres".into()),
        }
    );
    assert_eq!(l[4].name, "scratch-nginx");
    assert_eq!((l[4].project.as_deref(), l[4].service.as_deref()), (None, None));
}

#[test]
fn list_tolerates_missing_fields() {
    let l = parse_list(br#"[{"Id":"abcdef0123456789ff","Names":null,"Labels":null},{"Id":"12","Names":[]}]"#).unwrap();
    assert_eq!(l[0].name, "abcdef012345");
    assert_eq!(l[1].name, "12");
    assert_eq!(parse_list(b"[]").unwrap(), vec![]);
    assert!(parse_list(b"{\"message\":\"x\"}").is_err());
}

#[test]
fn memory_cgroup_v2_subtracts_inactive_file() {
    assert_eq!(parse_stats(STATS_V2), Some((V2_MEM, 432_597_212_000)));
}

#[test]
fn memory_cgroup_v1_subtracts_total_inactive_file() {
    assert_eq!(parse_stats(STATS_V1), Some((V1_MEM, 100_094_376)));
}

#[test]
fn memory_of_a_stopped_container_is_zero() {
    assert_eq!(parse_stats(STATS_STOPPED), Some((0, 0)));
    assert_eq!(parse_stats(b"{}"), Some((0, 0)));
}

#[test]
fn memory_is_floored_at_zero() {
    let s = br#"{"memory_stats":{"usage":100,"stats":{"inactive_file":500}},"cpu_stats":{"cpu_usage":{"total_usage":7}}}"#;
    assert_eq!(parse_stats(s), Some((0, 7)));
    assert_eq!(parse_stats(b"not json"), None);
}

#[test]
fn unsafe_ids_are_rejected() {
    assert!(is_safe_id(&"a1".repeat(32)));
    for bad in ["", "../images/json", "a b", "id?x=1", "id/stats"] {
        assert!(!is_safe_id(bad), "{bad}");
    }
}

// ------------------------------------------------------------------ CPU and stacks

fn sample(id: &str, project: Option<(&str, &str, &str)>, mem: u64, cpu_ns: u64, at: Instant) -> Sample {
    Sample {
        id: id.into(),
        name: format!("{id}-name"),
        project: project.map(|p| p.0.into()),
        working_dir: project.map(|p| p.1.into()),
        service: project.map(|p| p.2.into()),
        mem,
        cpu_total_ns: cpu_ns,
        at,
    }
}

#[test]
fn cpu_is_the_counter_diff_over_wall_time() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    assert_eq!(cpu_per_mille(None, 5_000_000_000, t0), 0);
    let prev = Some(CpuMark { total_ns: 1_000_000_000, at: t0 });
    // 15 s of CPU over 30 s = half a core.
    assert_eq!(cpu_per_mille(prev, 16_000_000_000, at(30)), 500);
    // Three cores busy.
    assert_eq!(cpu_per_mille(prev, 91_000_000_000, at(30)), 3000);
    // Counter went backwards (restart) or no time elapsed.
    assert_eq!(cpu_per_mille(prev, 10, at(30)), 0);
    assert_eq!(cpu_per_mille(prev, 2_000_000_000, t0), 0);
}

#[test]
fn account_diffs_cpu_per_container_and_forgets_gone_ones() {
    let mut d = Docker::new(None);
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let p = Some(("shop", "/w/shop", "db"));

    let first = d.account(vec![sample("a", p, MIB, 1_000_000_000, at(0)), sample("b", None, MIB, 0, at(0))]);
    assert!(first.iter().all(|s| s.cpu_pm == 0), "first sight is 0");

    let second = d.account(vec![sample("a", p, MIB, 7_000_000_000, at(30))]);
    assert_eq!(second[0].cpu_pm, 200);
    assert!(!d.prev_cpu.contains_key("b"), "b stopped: forgotten");

    // b comes back: first sight again.
    let third = d.account(vec![sample("a", p, MIB, 7_000_000_000, at(60)), sample("b", None, MIB, 9_000_000_000, at(60))]);
    assert!(third.iter().all(|s| s.cpu_pm == 0));
}

#[test]
fn stacks_group_by_project_and_working_dir() {
    let mut d = Docker::new(None);
    let t = Instant::now();
    let stacks = d.account(vec![
        sample("1", Some(("shop", "/w/shop", "redis")), 10 * MIB, 0, t),
        sample("2", Some(("shop", "/w/shop", "postgres")), 100 * MIB, 0, t),
        sample("3", Some(("shop", "/w/shop", "postgres")), 5 * MIB, 0, t),
        // Same project name in another worktree: another stack.
        sample("4", Some(("shop", "/w/shop-wt2", "postgres")), 50 * MIB, 0, t),
        sample("5", None, 200 * MIB, 0, t),
    ]);
    let keys: Vec<&str> = stacks.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(keys, ["container:5-name", "shop@/w/shop", "shop@/w/shop-wt2"]);
    assert_eq!(
        stacks[1],
        Stack {
            key: "shop@/w/shop".into(),
            name: "shop".into(),
            working_dir: Some("/w/shop".into()),
            services: vec!["postgres".into(), "redis".into()],
            mem: 115 * MIB,
            cpu_pm: 0,
            containers: vec!["1".into(), "2".into(), "3".into()],
        }
    );
    assert_eq!(stacks[0].name, "5-name");
    assert_eq!(stacks[0].working_dir, None);
    assert!(stacks[0].services.is_empty());
}

#[test]
fn compose_project_without_working_dir() {
    let mut d = Docker::new(None);
    let mut s = sample("1", Some(("p", "", "svc")), MIB, 0, Instant::now());
    s.working_dir = None;
    let stacks = d.account(vec![s]);
    assert_eq!((stacks[0].key.as_str(), stacks[0].working_dir.as_deref()), ("p@", None));
}

fn stack(key: &str, mem: u64) -> Stack {
    Stack { key: key.into(), name: key.into(), working_dir: None, services: vec![], mem, cpu_pm: 0, containers: vec![] }
}

#[test]
fn cap_to_vm_leaves_stacks_alone_when_they_fit() {
    let mut s = vec![stack("a", 100), stack("b", 300)];
    assert_eq!(cap_to_vm(&mut s, 1000), 600);
    assert_eq!((s[0].mem, s[1].mem), (100, 300));
    assert_eq!(cap_to_vm(&mut [], 1000), 1000);
}

#[test]
fn cap_to_vm_scales_down_proportionally() {
    let mut s = vec![stack("a", 3000), stack("b", 1000)];
    assert_eq!(cap_to_vm(&mut s, 2000), 0);
    assert_eq!((s[0].mem, s[1].mem), (1500, 500));
    // Rounding down leaves a tiny remainder, never a negative one.
    let mut s = vec![stack("a", 1), stack("b", 1), stack("c", 1)];
    let rest = cap_to_vm(&mut s, 2);
    assert_eq!(s.iter().map(|s| s.mem).sum::<u64>() + rest, 2);
    // Huge values do not overflow.
    let mut s = vec![stack("a", u64::MAX), stack("b", u64::MAX)];
    cap_to_vm(&mut s, 4 << 30);
    assert_eq!(s[0].mem, 2 << 30);
    assert_eq!(cap_to_vm(&mut s, 0), 0);
}

// ------------------------------------------------------------------ fake daemon

/// What the fake daemon does with one request path.
enum Reply {
    Ok(&'static [u8]),
    Status(u16),
    /// A status without a body (204, 304).
    Empty(u16),
    /// Answer, then close the connection without saying so.
    OkThenHangUp(&'static [u8]),
    /// Never answer.
    Hang,
}

/// (method, path) → reply.
type Handler = Arc<dyn Fn(&str, &str) -> Reply + Send + Sync>;

fn sock_path(tag: &str) -> PathBuf {
    // Short: sun_path is 104 bytes.
    let p = std::env::temp_dir().join(format!("dl-{tag}-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn serve(path: &Path, handler: Handler) {
    let listener = UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { return };
            let h = handler.clone();
            std::thread::spawn(move || serve_conn(conn, h));
        }
    });
}

fn serve_conn(conn: std::os::unix::net::UnixStream, h: Handler) {
    let mut w = conn.try_clone().unwrap();
    let mut r = BufReader::new(conn);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut words = line.split(' ');
        let method = words.next().unwrap_or("").to_string();
        let path = words.next().unwrap_or("").to_string();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                break;
            }
        }
        let (status, body, hang_up) = match h(&method, &path) {
            Reply::Empty(status) => {
                if w.write_all(format!("HTTP/1.1 {status} X\r\nApi-Version: 1.54\r\n\r\n").as_bytes()).is_err() {
                    return;
                }
                continue;
            }
            Reply::Ok(b) => (200, b, false),
            Reply::OkThenHangUp(b) => (200, b, true),
            Reply::Status(s) => (s, &b"{\"message\":\"no such container\"}"[..], false),
            Reply::Hang => {
                std::thread::sleep(Duration::from_secs(10));
                return;
            }
        };
        // Two chunks, like dockerd's streaming encoder.
        let (a, b) = body.split_at(body.len() / 2);
        let mut out = format!("HTTP/1.1 {status} X\r\nApi-Version: 1.54\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n").into_bytes();
        for part in [a, b] {
            if !part.is_empty() {
                out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
                out.extend_from_slice(part);
                out.extend_from_slice(b"\r\n");
            }
        }
        out.extend_from_slice(b"0\r\n\r\n");
        if w.write_all(&out).is_err() || hang_up {
            return;
        }
    }
}

fn fixture_daemon(method: &str, path: &str) -> Reply {
    assert_eq!(method, "GET");
    if path == "/v1.41/containers/json" {
        return Reply::Ok(LIST);
    }
    let id = path.strip_prefix("/v1.41/containers/").and_then(|p| p.strip_suffix("/stats?stream=false&one-shot=true"));
    match id.map(|id| &id[..2]) {
        Some("a1") | Some("d1") => Reply::Ok(STATS_V2),
        Some("a2") => Reply::Status(404), // stopped between list and stats
        Some("b1") | Some("b2") => Reply::Ok(STATS_V1),
        Some("c1") => Reply::Ok(STATS_STOPPED),
        _ => Reply::Status(400),
    }
}

#[test]
fn scan_against_a_fake_daemon() {
    let p = sock_path("scan");
    serve(&p, Arc::new(fixture_daemon));
    let mut d = Docker::new(Some(p.clone()));
    let stacks = d.scan().unwrap();
    let got: Vec<(&str, u64, Vec<String>)> = stacks.iter().map(|s| (s.key.as_str(), s.mem, s.services.clone())).collect();
    assert_eq!(
        got,
        [
            ("shop@/Users/dev/code/shop", V2_MEM, vec!["postgres".to_string()]),
            ("shop@/Users/dev/code/shop-wt2", V2_MEM, vec!["postgres".to_string()]),
            ("blog@/Users/dev/code/blog/docker", 2 * V1_MEM, vec!["web".to_string()]),
            ("container:scratch-nginx", 0, vec![]),
        ]
    );
    assert_eq!(d.client.as_ref().unwrap().connects, 1, "one connection for the whole scan");
    // Second scan: same connection, counters unchanged → 0 CPU.
    let again = d.scan().unwrap();
    assert!(again.iter().all(|s| s.cpu_pm == 0));
    assert_eq!(d.client.as_ref().unwrap().connects, 1, "kept alive across scans");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn reconnects_when_the_daemon_drops_the_connection() {
    let p = sock_path("drop");
    serve(
        &p,
        Arc::new(|method: &str, path: &str| match fixture_daemon(method, path) {
            Reply::Ok(b) => Reply::OkThenHangUp(b),
            other => other,
        }),
    );
    let mut d = Docker::new(Some(p.clone()));
    assert_eq!(d.scan().unwrap().len(), 4);
    assert!(d.client.as_ref().unwrap().connects > 1);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn empty_daemon_is_some_empty() {
    let p = sock_path("empty");
    serve(&p, Arc::new(|_: &str, _: &str| Reply::Ok(b"[]")));
    assert_eq!(Docker::new(Some(p.clone())).scan(), Some(vec![]));
    let _ = std::fs::remove_file(&p);
}

#[test]
fn stopped_docker_is_none_and_backs_off() {
    assert_eq!(Docker::new(None).scan(), None);

    let p = sock_path("down");
    let mut d = Docker::new(Some(p.clone()));
    assert_eq!(d.scan(), None);
    assert!(d.next_try.is_some());
    // Docker starts, but we are still backing off: no connect at all.
    serve(&p, Arc::new(|_: &str, _: &str| Reply::Ok(b"[]")));
    assert_eq!(d.scan(), None);
    assert_eq!(d.client.as_ref().unwrap().connects, 0);
    // Back-off elapsed.
    d.next_try = Some(Instant::now());
    assert_eq!(d.scan(), Some(vec![]));
    assert_eq!(d.next_try, None);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn hung_daemon_times_out() {
    let p = sock_path("hang");
    serve(&p, Arc::new(|_: &str, _: &str| Reply::Hang));
    let t = Instant::now();
    assert_eq!(Docker::new(Some(p.clone())).scan(), None);
    let took = t.elapsed();
    assert!(took >= http::IO_TIMEOUT && took < http::IO_TIMEOUT * 2, "{took:?}");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn error_body_on_the_list_is_none() {
    let p = sock_path("err");
    serve(&p, Arc::new(|_: &str, _: &str| Reply::Status(500)));
    assert_eq!(Docker::new(Some(p.clone())).scan(), None);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn a_listed_id_that_could_escape_the_path_is_never_requested() {
    let p = sock_path("unsafe");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    serve(
        &p,
        Arc::new(move |method: &str, path: &str| {
            log.lock().unwrap().push(path.to_string());
            if path == "/v1.41/containers/json" {
                return Reply::Ok(br#"[{"Id":"../../images/x","Names":["/evil"]},{"Id":"a1b2","Names":["/web"]}]"#);
            }
            fixture_daemon(method, path)
        }),
    );
    let stacks = Docker::new(Some(p.clone())).scan().unwrap();
    assert_eq!(stacks.len(), 1);
    assert_eq!(*seen.lock().unwrap(), ["/v1.41/containers/json", "/v1.41/containers/a1b2/stats?stream=false&one-shot=true"]);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn a_daemon_that_stops_answering_mid_scan_is_none() {
    let p = sock_path("midhang");
    serve(&p, Arc::new(|_: &str, path: &str| if path == "/v1.41/containers/json" { Reply::Ok(LIST) } else { Reply::Hang }));
    // A partial scan would show stacks as smaller than they are.
    assert_eq!(Docker::new(Some(p.clone())).scan(), None);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn stop_counts_stopped_already_stopped_and_gone_as_down() {
    assert!(is_down(204) && is_down(304) && is_down(404));
    assert!(!is_down(200) && !is_down(409) && !is_down(500));
}

#[test]
fn stop_posts_to_each_container_against_a_fake_daemon() {
    let p = sock_path("stop");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    serve(
        &p,
        Arc::new(move |method: &str, path: &str| {
            log.lock().unwrap().push(format!("{method} {path}"));
            let id = path.strip_prefix("/v1.41/containers/").and_then(|p| p.strip_suffix("/stop?t=5"));
            match id {
                Some("aa") => Reply::Empty(204),
                Some("bb") => Reply::Empty(304),
                Some("cc") => Reply::Status(404),
                _ => Reply::Status(500),
            }
        }),
    );
    let ids: Vec<String> = ["aa", "bb", "cc", "dd"].iter().map(|s| s.to_string()).collect();
    assert_eq!(stop_containers(&p, &ids), 3, "500 is not down");
    let mut seen = seen.lock().unwrap().clone();
    seen.sort();
    assert_eq!(seen, ["POST /v1.41/containers/aa/stop?t=5", "POST /v1.41/containers/bb/stop?t=5", "POST /v1.41/containers/cc/stop?t=5", "POST /v1.41/containers/dd/stop?t=5"]);
    // An id that could escape the path is never sent; no daemon means not down.
    assert_eq!(stop_containers(&p, &["../x".to_string()]), 0);
    assert_eq!(stop_containers(&sock_path("stop-none"), &ids[..1]), 0);
    let _ = std::fs::remove_file(&p);
}

#[test]
fn post_keeps_the_connection_after_a_bodyless_answer() {
    let p = sock_path("post");
    serve(&p, Arc::new(|method: &str, _: &str| if method == "POST" { Reply::Empty(204) } else { Reply::Ok(b"[]") }));
    let mut c = http::Client::new(&p);
    let r = c.post("/v1.41/containers/aa/stop?t=5", Duration::from_secs(1)).unwrap();
    assert_eq!((r.status, r.body.len(), r.keep_alive), (204, 0, true));
    assert_eq!(c.get("/v1.41/containers/json").unwrap(), b"[]");
    assert_eq!(c.connects, 1);
    let _ = std::fs::remove_file(&p);
}

// ------------------------------------------------------------------ live

/// `cargo test --lib docker::tests::live -- --ignored --nocapture` with Docker up.
#[test]
#[ignore]
fn live() {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let sock = resolve_socket(|k| std::env::var(k).ok(), &home);
    println!("socket: {sock:?}");
    let mut d = Docker::new(sock);
    for round in 0..2 {
        if round > 0 {
            std::thread::sleep(Duration::from_secs(2));
        }
        let t = Instant::now();
        let stacks = d.scan();
        let took = t.elapsed();
        let Some(stacks) = stacks else {
            println!("scan {round}: Docker not reachable ({took:?})");
            return;
        };
        let total: u64 = stacks.iter().map(|s| s.mem).sum();
        println!("scan {round}: {} stacks, {:.1} MiB in {took:?}", stacks.len(), total as f64 / MIB as f64);
        for s in &stacks {
            println!("  {:>8.1} MiB {:>5} pm  {}  {:?}", s.mem as f64 / MIB as f64, s.cpu_pm, s.name, s.services);
        }
    }
}
