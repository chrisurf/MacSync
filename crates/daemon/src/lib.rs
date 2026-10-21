//! The RepoSync daemon: configuration, file system watcher, scheduler with
//! quiescence detection, anti-entropy, peer management and the IPC socket
//! used by the CLI (and later the menu bar app).

pub mod config;
pub mod ipc;
pub mod runtime;
pub mod service;

pub use config::Config;
pub use runtime::{Daemon, Node};
