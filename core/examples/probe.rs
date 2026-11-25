//! Dev probe: a few real ticks, printed. `cargo run --release --example probe`
//! The output names your real projects: keep it local.

use devload_core::collect::{Collector, Config};
use devload_core::docker::{Docker, Provider, resolve_socket};
use devload_core::engine::aggregate::{ArgsCache, aggregate};
use devload_core::engine::groups::Registry;
use devload_core::engine::parts::Members;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn main() {
    let cfg = Config::from_env();
    let home = cfg.home.clone();
    let mut col = Collector::new(cfg);
    let socket = resolve_socket(|k| std::env::var(k).ok(), Path::new(&home));
    let provider = socket.as_deref().map_or(Provider::Unknown, Provider::of_socket);
    println!("docker provider: {provider:?}");
    let mut docker = Docker::new(socket);
    let mut reg = Registry::new(&home, vec![]);
    let mut args = ArgsCache::default();
    let mut members = Members::default();
    let t = Instant::now();
    let stacks = docker.scan();
    println!("docker scan {:?}: {} stacks", t.elapsed(), stacks.as_ref().map_or(0, Vec::len));
    let mut last = None;
    for i in 0..3 {
        if i > 0 {
            std::thread::sleep(Duration::from_secs(5));
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let scan = col.tick(now);
        let t = Instant::now();
        let agg = aggregate(&scan, &mut reg, col.repos(), provider, stacks.as_deref(), &mut args, &mut members);
        println!(
            "tick {i}: collect {:.1} ms cpu, aggregate {:?}, {} groups, {} procs",
            scan.cost_ns as f64 / 1e6,
            t.elapsed(),
            agg.groups.len(),
            agg.procs.len()
        );
        last = Some((scan, agg));
    }
    let (scan, agg) = last.unwrap();
    let gb = |b: u64| b as f64 / 1e9;
    println!("used {:.2} / {:.0} GB, cpu {} pm", gb(scan.machine.used), gb(scan.machine.total), scan.machine.cpu_pm);
    let sum: u64 = agg.groups.iter().map(|g| g.footprint).sum();
    println!("sum of group footprints {:.2} GB", gb(sum));
    for r in &agg.rows {
        println!("{:?} {:<28} {:>6.2} GB {:>5} pm  {}", r.kind, r.name, gb(r.footprint), r.cpu_pm, r.detail);
    }
}
