//! Performance benchmark (ignored by default):
//!
//! `cargo test --release -p reposync-engine --test bench -- --ignored --nocapture`
//!
//! `BENCH_FILES` sets the number of files (default 100 000).

mod common;

use std::time::Instant;

use common::*;
use reposync_engine::PullOptions;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore]
fn bench_many_small_files() {
    let n: usize = std::env::var("BENCH_FILES").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let p = Pair::new();
    let root = p.ra();
    let t = Instant::now();
    for i in 0..n {
        let dir = root.join(format!("proj/node_modules/pkg{}/lib{}", i % 500, i % 7));
        if i % 7 == 0 || !dir.exists() {
            std::fs::create_dir_all(&dir).unwrap();
        }
        std::fs::write(dir.join(format!("m{i}.js")), format!("module.exports = function f{i}() {{ return {i}; }};\n"))
            .unwrap();
    }
    let gen_ms = ms(t);

    let t = Instant::now();
    p.a.scan_all(false).unwrap();
    let cold_scan_ms = ms(t);

    let t = Instant::now();
    let r = p.a.scan_all(false).unwrap();
    let warm_scan_ms = ms(t);
    assert!(r.iter().all(|r| r.changed == 0 && r.hashed == 0));

    let t = Instant::now();
    let roots = p.a.local_roots().unwrap();
    let idle_root_compare_ms = ms(t);
    assert!(!roots.is_empty());

    p.b.scan_all(false).unwrap();
    let t = Instant::now();
    let r = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    let initial_pull_ms = ms(t);
    let fetched: u64 = r.iter().map(|r| r.bytes_fetched).sum();
    let phases: Vec<_> = r.iter().filter(|r| r.unit == "proj").flat_map(|r| r.timings_ms.clone()).collect();
    assert!(p.roots_equal());

    // One changed file deep in the tree.
    std::fs::write(root.join("proj/node_modules/pkg42/lib0/m42.js"), "module.exports = 'changed';\n").unwrap();
    let t = Instant::now();
    p.a.scan_unit("proj", &Default::default()).unwrap();
    let change_scan_ms = ms(t);
    let t = Instant::now();
    let r = p.b.sync_from(&p.a, None, PullOptions::default()).unwrap();
    let change_pull_ms = ms(t);
    let proj = r.iter().find(|r| r.unit == "proj").unwrap();
    assert_eq!(proj.writes, 1, "{:?}", proj.actions);
    assert!(p.roots_equal());

    let summary = serde_json::json!({
        "files": n,
        "generate_ms": gen_ms,
        "cold_scan_ms": cold_scan_ms,
        "warm_rescan_ms": warm_scan_ms,
        "idle_root_compare_ms": idle_root_compare_ms,
        "initial_pull_ms": initial_pull_ms,
        "initial_pull_files_per_s": n as f64 / (initial_pull_ms / 1000.0),
        "initial_bytes_fetched": fetched,
        "single_change_scan_ms": change_scan_ms,
        "single_change_pull_ms": change_pull_ms,
        "initial_pull_phases_ms": phases,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench-summary.json");
    let _ = std::fs::write(out, serde_json::to_string_pretty(&summary).unwrap());
}
