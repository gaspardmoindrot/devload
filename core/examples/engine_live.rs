//! Live check of the engine on this Mac: a few real ticks, then the
//! snapshot, graph fills and prompt, with timings. Names are masked.
//!
//!     cargo run --release --example engine_live [data_dir]

use std::time::{Duration, Instant};

use devload_core::collect::Config;
use devload_core::docker::{Docker, Provider, resolve_socket};
use devload_core::engine::{Engine, Mode, Period, SeriesBufs};

fn mask(s: &str) -> String {
    let h = s.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    format!("{}…#{:04x}", s.chars().next().unwrap_or('?'), h & 0xffff)
}

fn main() {
    let dir = std::env::args().nth(1).map(Into::into).unwrap_or_else(|| std::env::temp_dir().join("devload-engine-live"));
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let t = Instant::now();
    let mut e = Engine::new(Config::from_env(), &dir, now());
    println!("open: {:?} db={}", t.elapsed(), e.db_path().display());

    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
    if let Some(sock) = resolve_socket(|k| std::env::var(k).ok(), &home) {
        let t = Instant::now();
        let stacks = Docker::new(Some(sock.clone())).scan();
        println!("docker: {:?} {} stacks", t.elapsed(), stacks.as_ref().map_or(0, |s| s.len()));
        e.set_stacks(now(), Provider::of_socket(&sock), stacks);
    }

    for i in 0..4 {
        let t = Instant::now();
        let out = e.tick(now());
        println!("tick {i}: wall {:?} cpu {:.2} ms {out:?}", t.elapsed(), e.last_cost_ns as f64 / 1e6);
        std::thread::sleep(Duration::from_secs(5));
    }

    let s = e.snapshot();
    let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
    println!("\nused {:.1}/{:.0} GB, {:?} since {}, CPU {}%", gb(s.used), gb(s.total), s.pressure, s.since, s.cpu_pct);
    println!("Dev projects:");
    for r in &s.dev {
        println!("  {:<12} {:>5.2} GB  {}", mask(&r.name), gb(r.footprint), r.detail);
    }
    println!("  +{} more ({:.2} GB)", s.dev_more.count, gb(s.dev_more.footprint));
    println!("Apps:");
    for r in &s.apps {
        println!("  {:<24} {:>5.2} GB  {}", r.name, gb(r.footprint), r.detail);
    }
    println!("  +{} more ({:.2} GB)", s.apps_more.count, gb(s.apps_more.footprint));

    e.set_popover_open(true);
    let w = 320;
    for p in [Period::H1, Period::H6, Period::H24, Period::D7] {
        for m in [Mode::Pressure, Mode::Groups] {
            let (mut lo, mut hi, mut lv, mut st, mut hd) = (vec![0.0; w], vec![0.0; w], vec![0u8; w], vec![0.0; 4 * w], vec![false; w]);
            let t = Instant::now();
            let meta = e.series(now(), p, m, SeriesBufs { lo: &mut lo, hi: &mut hi, level: &mut lv, stack: &mut st, has_data: &mut hd });
            let cols = match m {
                Mode::Pressure => lv.iter().filter(|&&l| l != 255).count(),
                Mode::Groups => hd.iter().filter(|&&b| b).count(),
            };
            let top: Vec<String> = meta.top.iter().map(|(_, n)| mask(n)).collect();
            println!("series {p:?}/{m:?}: {:?} ({cols} cols) {top:?}", t.elapsed());
        }
    }
    e.set_popover_open(false);

    let t = Instant::now();
    let prompt = e.prompt(now(), Period::H1);
    println!("\nprompt: {:?}, {} chars, {} lines", t.elapsed(), prompt.len(), prompt.lines().count());
}
