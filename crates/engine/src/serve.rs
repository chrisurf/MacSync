//! Answering a peer's requests from the local index.

use anyhow::Result;
use macsync_core::proto::{limits, DirListing, Request, Response, UnitRoot};
use macsync_core::{path as rpath, Entry, Hash};
use macsync_fs::ops;

use crate::{Engine, PeerApi};

impl Engine {
    /// Roots of all units that are not paused.
    pub fn serve_roots(&self) -> Result<Vec<UnitRoot>> {
        let paused: Vec<String> = self.store().units()?.into_iter().filter(|u| u.paused).map(|u| u.name).collect();
        Ok(self.local_roots()?.into_iter().filter(|r| !paused.contains(&r.name)).collect())
    }

    pub fn serve_tree(&self, unit: &str, dirs: &[String]) -> Result<Vec<DirListing>> {
        crate::validate_unit_name(unit)?;
        let tree = self.tree(unit)?;
        Ok(dirs
            .iter()
            .take(limits::TREE_BATCH)
            .map(|d| DirListing { dir: d.clone(), children: tree.children(d).map(|c| c.to_vec()) })
            .collect())
    }

    pub fn serve_entries(&self, unit: &str, paths: &[String], prefixes: &[String]) -> Result<Vec<Entry>> {
        crate::validate_unit_name(unit)?;
        let store = self.store();
        let mut out = Vec::new();
        for p in paths.iter().take(limits::ENTRY_BATCH) {
            if let Some(s) = store.entry(unit, p)? {
                out.push(s.entry);
            }
        }
        for p in prefixes.iter().take(limits::ENTRY_BATCH) {
            for s in store.entries_under(unit, p)? {
                if s.entry.path != *p {
                    out.push(s.entry);
                }
            }
        }
        Ok(out)
    }

    /// Read a blob from any live local copy, verifying its hash (optimistic
    /// snapshot: a changed file is reported as unavailable).
    pub fn serve_blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>> {
        let candidates = self.store().find_blob(hash)?;
        for (unit, _path, disk) in candidates {
            let abs = macsync_fs::abs_path(&self.cfg.root, &unit, &disk);
            if let Ok(Some(data)) = ops::read_verified(&abs, hash) {
                return Ok(Some(data));
            }
        }
        Ok(None)
    }

    pub fn serve_blobs(&self, hashes: &[Hash]) -> Result<Vec<Option<Vec<u8>>>> {
        let mut total = 0u64;
        let mut out = Vec::with_capacity(hashes.len());
        for h in hashes.iter().take(limits::BUNDLE_COUNT) {
            let b = if total > limits::BUNDLE_BYTES * 2 { None } else { self.serve_blob(h)? };
            total += b.as_ref().map(|b| b.len() as u64).unwrap_or(0);
            out.push(b);
        }
        self.count_served(total);
        Ok(out)
    }

    /// A range of a large blob. The file must still match the stat data of
    /// the index; the receiver verifies the full hash at the end.
    pub fn serve_blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>> {
        let len = len.min(limits::RANGE_CHUNK);
        let candidates = self.store().find_blob(hash)?;
        for (unit, path, disk) in candidates {
            let Some(stored) = self.store().entry(&unit, &path)? else {
                continue;
            };
            let abs = macsync_fs::abs_path(&self.cfg.root, &unit, &disk);
            let Ok(Some(now)) = macsync_fs::scan::stat(&abs, stored.entry.path.clone(), disk.clone()) else {
                continue;
            };
            let unchanged = now.size == stored.entry.size
                && now.mtime_ns == stored.entry.mtime_ns
                && now.ino == stored.stat.ino
                && now.ctime_ns == stored.stat.ctime_ns;
            if unchanged {
                let data = ops::read_range(&abs, offset, len)?;
                self.count_served(data.len() as u64);
                return Ok(Some(data));
            }
        }
        Ok(None)
    }

    /// Dispatch a protocol request (used by the network server).
    pub fn handle(&self, req: Request) -> Response {
        let r: Result<Response> = (|| {
            Ok(match req {
                Request::Roots => Response::Roots(self.serve_roots()?),
                Request::Tree { unit, dirs } => {
                    for d in &dirs {
                        rpath::validate(d).map_err(anyhow::Error::msg)?;
                    }
                    Response::Tree(self.serve_tree(&unit, &dirs)?)
                }
                Request::Entries { unit, paths, prefixes } => {
                    for p in paths.iter().chain(&prefixes) {
                        rpath::validate(p).map_err(anyhow::Error::msg)?;
                    }
                    Response::Entries(self.serve_entries(&unit, &paths, &prefixes)?)
                }
                Request::Blobs { hashes } => Response::Blobs(self.serve_blobs(&hashes)?),
                Request::BlobRange { hash, offset, len } => {
                    Response::BlobRange(self.serve_blob_range(&hash, offset, len)?)
                }
                Request::Ping => Response::Pong,
                Request::Hello { .. } | Request::Changed { .. } | Request::Prioritize { .. } => Response::Ok,
            })
        })();
        r.unwrap_or_else(|e| Response::Error(e.to_string()))
    }
}

impl PeerApi for Engine {
    fn roots(&self) -> Result<Vec<UnitRoot>> {
        self.serve_roots()
    }
    fn tree(&self, unit: &str, dirs: &[String]) -> Result<Vec<DirListing>> {
        self.serve_tree(unit, dirs)
    }
    fn entries(&self, unit: &str, paths: &[String], prefixes: &[String]) -> Result<Vec<Entry>> {
        self.serve_entries(unit, paths, prefixes)
    }
    fn blobs(&self, hashes: &[Hash]) -> Result<Vec<Option<Vec<u8>>>> {
        self.serve_blobs(hashes)
    }
    fn blob_range(&self, hash: &Hash, offset: u64, len: u64) -> Result<Option<Vec<u8>>> {
        self.serve_blob_range(hash, offset, len)
    }
}
