//! Cost of one engine tick on this Mac: the real collector (every process,
//! machine counters), grouping, the minute accumulator, and the amortized
//! database writes. The clock advances 5 s per iteration, as in the app, so
//! a flush runs every 60 iterations and the hourly maintenance every 720.
//!
//!     cargo bench --bench tick
//!
//! The data folder is a scratch folder under the system temp dir.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use devload_core::collect::Config;
use devload_core::engine::Engine;

fn tick(c: &mut Criterion) {
    let dir = std::env::temp_dir().join(format!("devload-bench-tick-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut e = Engine::new(Config::from_env(), &dir, start);
    let mut now = start;
    // Warm-up: caches (args, repo roots, groups) filled as after a few minutes of running.
    for _ in 0..24 {
        now += 5;
        e.tick(now);
    }
    c.bench_function("engine_tick", |b| {
        b.iter(|| {
            now += 5;
            black_box(e.tick(now))
        })
    });
    let _ = std::fs::remove_dir_all(&dir);
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(50).measurement_time(std::time::Duration::from_secs(20));
    targets = tick
}
criterion_main!(benches);
