//! Content hashes, only for files that have a same-named partner of the same
//! size (different sizes never need reading).

use std::collections::HashMap;
use std::fs::File;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use crate::walk::Tree;

pub(crate) type Hashes = HashMap<usize, [u8; 32]>;

/// Files worth hashing: same name key and same size as another file, with
/// content on disk and not empty.
pub(crate) fn candidates(t: &Tree) -> Vec<usize> {
    let mut by: HashMap<(&str, u64), Vec<usize>> = HashMap::new();
    for i in t.files() {
        let n = &t.nodes[i];
        if n.size > 0 && !n.dataless {
            by.entry((n.key.as_str(), n.size)).or_default().push(i);
        }
    }
    let mut out: Vec<usize> = by.into_values().filter(|v| v.len() > 1).flatten().collect();
    out.sort_unstable();
    out
}

/// Hash `files` in parallel. Unreadable files are left out (content unknown).
pub(crate) fn hash_all(t: &Tree, files: &[usize], progress: bool) -> (Hashes, u64) {
    let next = AtomicUsize::new(0);
    let bytes = AtomicU64::new(0);
    let done = AtomicBool::new(false);
    let out = Mutex::new(HashMap::with_capacity(files.len()));
    let total: u64 = files.iter().map(|&i| t.nodes[i].size).sum();
    let workers = thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = files.get(k) else { break };
                        if let Some(h) = hash_file(t, i) {
                            mine.push((i, h));
                        }
                        bytes.fetch_add(t.nodes[i].size, Ordering::Relaxed);
                    }
                    out.lock().unwrap().extend(mine);
                })
            })
            .collect();
        if progress && total > 0 {
            s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    let b = bytes.load(Ordering::Relaxed);
                    eprint!("\rcomparing contents … {} %   ", b * 100 / total);
                    thread::sleep(Duration::from_millis(250));
                }
                eprintln!("\rcomparing contents … 100 %   ");
            });
        }
        for h in handles {
            let _ = h.join();
        }
        done.store(true, Ordering::Relaxed);
    });
    (out.into_inner().unwrap(), total)
}

fn hash_file(t: &Tree, i: usize) -> Option<[u8; 32]> {
    let f = File::open(&t.nodes[i].abs).ok()?;
    let mut h = blake3::Hasher::new();
    h.update_reader(f).ok()?;
    Some(*h.finalize().as_bytes())
}
