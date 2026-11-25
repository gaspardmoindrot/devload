//! Tick cost at the app's real cadence: one engine tick every 5 s on this
//! Mac, CPU time of the ticking thread (`Engine::last_cost_ns`) and wall time.
//! Unlike the criterion bench, caches and the kernel see the same rhythm as
//! in the app. Writes into a scratch folder.
//!
//!     cargo run --release --example tick_cost [-- TICKS [DIR]]

use std::time::{Duration, Instant};

use devload_core::collect::Config;
use devload_core::engine::Engine;

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let ticks: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    let dir = std::env::args().nth(2).map(Into::into).unwrap_or_else(|| std::env::temp_dir().join("devload-tick-cost"));
    let _ = std::fs::remove_dir_all(&dir);
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut e = Engine::new(Config::from_env(), &dir, now());
    let (mut cpu, mut wall) = (vec![], vec![]);
    let mut first = 0.0;
    for i in 0..=ticks {
        let t = Instant::now();
        e.tick(now());
        let w = t.elapsed().as_secs_f64() * 1e3;
        let c = e.last_cost_ns as f64 / 1e6;
        if i == 0 {
            first = c; // cold: every process is new, args and repo roots uncached
        } else {
            cpu.push(c);
            wall.push(w);
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    let mean = cpu.iter().sum::<f64>() / cpu.len() as f64;
    println!("{ticks} ticks at 5 s (first, cold tick: {first:.2} ms CPU, not counted)");
    println!("thread CPU per tick: mean {mean:.2} ms, median {:.2}, p95 {:.2}, max {:.2}", pct(&mut cpu, 0.5), pct(&mut cpu, 0.95), pct(&mut cpu, 1.0));
    println!("wall per tick:       median {:.2} ms, p95 {:.2}, max {:.2}", pct(&mut wall, 0.5), pct(&mut wall, 0.95), pct(&mut wall, 1.0));
    println!("=> {:.3} % of one core for ticking", mean / 5000.0 * 100.0);
    let _ = std::fs::remove_dir_all(&dir);
}
