//! Exclusion rules.
//!
//! MacSync synchronises *everything* below the root unless the user
//! excludes it: macOS volume metadata files (switchable), MacSync's own
//! private directory, the user's globs and the build and package folders of
//! the [`ECOSYSTEMS`] switched off in the settings.
//!
//! Excluding never deletes: an excluded path is no longer compared, its
//! files stay on both Macs (the index forgets them without tombstones).

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

/// When a folder name counts as a build or package folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// Always (the name is unambiguous, e.g. `node_modules`).
    Always,
    /// Only next to this project file (`target` next to `Cargo.toml`).
    Sibling(&'static str),
    /// Only if the folder contains this file (`venv/pyvenv.cfg`).
    Inside(&'static str),
}

/// Folders of one language or tool that can be recreated at any time
/// (installed or built again). Switched off per ecosystem in the settings.
#[derive(Debug)]
pub struct Ecosystem {
    pub key: &'static str,
    pub title: &'static str,
    pub dirs: &'static [(&'static str, Marker)],
}

/// Build and package folders per ecosystem. Deliberately narrow: lock files
/// (`Cargo.lock`, `package-lock.json`, `.terraform.lock.hcl`), state
/// (`*.tfstate`) and sources always stay synchronised.
pub const ECOSYSTEMS: &[Ecosystem] = &[
    Ecosystem {
        key: "node",
        title: "Node.js / JavaScript",
        dirs: &[
            ("node_modules", Marker::Always),
            ("bower_components", Marker::Always),
            (".next", Marker::Always),
            (".nuxt", Marker::Always),
            (".svelte-kit", Marker::Always),
            (".turbo", Marker::Always),
            (".parcel-cache", Marker::Always),
        ],
    },
    Ecosystem {
        key: "python",
        title: "Python",
        dirs: &[
            ("__pycache__", Marker::Always),
            (".venv", Marker::Always),
            ("venv", Marker::Inside("pyvenv.cfg")),
            ("env", Marker::Inside("pyvenv.cfg")),
            (".mypy_cache", Marker::Always),
            (".pytest_cache", Marker::Always),
            (".ruff_cache", Marker::Always),
            (".tox", Marker::Always),
        ],
    },
    Ecosystem { key: "rust", title: "Rust (Cargo)", dirs: &[("target", Marker::Sibling("Cargo.toml"))] },
    Ecosystem {
        key: "swift",
        title: "Swift / Xcode",
        dirs: &[
            (".build", Marker::Sibling("Package.swift")),
            ("DerivedData", Marker::Always),
            ("Pods", Marker::Sibling("Podfile")),
        ],
    },
    Ecosystem { key: "go", title: "Go", dirs: &[("vendor", Marker::Sibling("go.mod"))] },
    Ecosystem {
        key: "terraform",
        title: "Terraform",
        dirs: &[(".terraform", Marker::Always), (".terragrunt-cache", Marker::Always)],
    },
];

/// Git's own folder, switchable too (synchronised by default).
pub const GIT_DIR: (&str, Marker) = (".git", Marker::Always);

/// File names that are Mac-local and would otherwise cause permanent
/// pseudo-changes. Can be disabled with `default_excludes = false`.
pub const DEFAULT_NAMES: &[&str] =
    &[".DS_Store", ".Spotlight-V100", ".Trashes", ".fseventsd", ".TemporaryItems", ".DocumentRevisions-V100"];

#[derive(Clone, Debug)]
pub struct Excludes {
    names: Vec<String>,
    globs: GlobSet,
    patterns: Vec<String>,
    /// Folder rules of the ecosystems switched off (and `.git` if off).
    dirs: Vec<(&'static str, Marker)>,
    /// Sync root, for the marker checks on disk.
    root: Option<PathBuf>,
}

impl Default for Excludes {
    fn default() -> Self {
        Excludes::new(&[], true).expect("empty pattern list is valid")
    }
}

impl Excludes {
    /// `patterns` are globs matched against root-relative paths, e.g.
    /// `"**/target"` or `"Scratch/*.tmp"`.
    pub fn new(patterns: &[String], default_names: bool) -> Result<Excludes, globset::Error> {
        let mut b = GlobSetBuilder::new();
        for p in patterns {
            b.add(Glob::new(p)?);
        }
        Ok(Excludes {
            names: if default_names { DEFAULT_NAMES.iter().map(|s| s.to_string()).collect() } else { Vec::new() },
            globs: b.build()?,
            patterns: patterns.to_vec(),
            dirs: Vec::new(),
            root: None,
        })
    }

    /// Also exclude the folders of the ecosystems `skip` (keys of
    /// [`ECOSYSTEMS`]; unknown keys are ignored) and, if `git` is false,
    /// Git's own folders. Markers are checked below `root`.
    pub fn with_ecosystems(mut self, root: &Path, skip: &[String], git: bool) -> Excludes {
        for e in ECOSYSTEMS.iter().filter(|e| skip.iter().any(|k| k == e.key)) {
            self.dirs.extend_from_slice(e.dirs);
        }
        if !git {
            self.dirs.push(GIT_DIR);
        }
        self.root = Some(root.to_path_buf());
        self
    }

    /// Git's own folders are not synchronised.
    pub fn skips_git(&self) -> bool {
        self.dirs.contains(&GIT_DIR)
    }

    /// `root_rel` is the path relative to the sync root, `name` its last
    /// component. Only this component is checked – the scanner never
    /// descends into an excluded folder. For a stored path whose folder
    /// may be excluded, use [`Excludes::is_excluded_path`].
    pub fn is_excluded(&self, root_rel: &str, name: &str) -> bool {
        self.is_excluded_with(root_rel, name, &|m| self.marker_on_disk(root_rel, m))
    }

    /// Like [`Excludes::is_excluded`], with the marker check supplied
    /// (e.g. from a directory listing of the other Mac).
    pub fn is_excluded_with(&self, root_rel: &str, name: &str, marker: &dyn Fn(&Marker) -> bool) -> bool {
        if crate::is_private(root_rel) {
            return true;
        }
        self.names.iter().any(|n| n == name)
            || (!self.patterns.is_empty() && self.globs.is_match(root_rel))
            || self.dirs.iter().any(|(n, m)| *n == name && (*m == Marker::Always || marker(m)))
    }

    /// True if `root_rel` or any folder above it is excluded.
    pub fn is_excluded_path(&self, root_rel: &str) -> bool {
        let mut end = 0;
        for part in root_rel.split('/') {
            end += part.len();
            if self.is_excluded(&root_rel[..end], part) {
                return true;
            }
            end += 1;
        }
        false
    }

    /// Whether the marker of the folder `root_rel` exists on this Mac.
    fn marker_on_disk(&self, root_rel: &str, m: &Marker) -> bool {
        let Some(root) = &self.root else { return false };
        let dir = root.join(root_rel);
        match m {
            Marker::Always => true,
            Marker::Sibling(f) => dir.parent().is_some_and(|p| p.join(f).exists()),
            Marker::Inside(f) => dir.join(f).exists(),
        }
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_globs() {
        let ex = Excludes::new(&["**/target".into(), "Scratch/*.tmp".into()], true).unwrap();
        assert!(ex.is_excluded("a/.DS_Store", ".DS_Store"));
        assert!(ex.is_excluded(".macsync", ".macsync"));
        assert!(ex.is_excluded(".macsync/trash/x", "x"));
        assert!(ex.is_excluded(".reposync", ".reposync"));
        assert!(!ex.is_excluded(".macsyncx", ".macsyncx"));
        assert!(ex.is_excluded("proj/target", "target"));
        assert!(ex.is_excluded("Scratch/x.tmp", "x.tmp"));
        assert!(!ex.is_excluded("proj/node_modules/x", "x"));
        assert!(!ex.is_excluded("proj/.git/HEAD", "HEAD"));
        let none = Excludes::new(&[], false).unwrap();
        assert!(!none.is_excluded("a/.DS_Store", ".DS_Store"));
    }

    #[test]
    fn ecosystems_with_markers() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        for p in ["rs/target", "photos/target", "py/venv", "py/plain/venv", "web/node_modules", "app/.git"] {
            std::fs::create_dir_all(r.join(p)).unwrap();
        }
        std::fs::write(r.join("rs/Cargo.toml"), "").unwrap();
        std::fs::write(r.join("py/venv/pyvenv.cfg"), "").unwrap();
        let skip: Vec<String> = ["rust", "python", "node"].iter().map(|s| s.to_string()).collect();
        let ex = Excludes::new(&[], true).unwrap().with_ecosystems(r, &skip, true);
        assert!(ex.is_excluded("rs/target", "target"), "next to Cargo.toml");
        assert!(!ex.is_excluded("photos/target", "target"), "an ordinary folder called target");
        assert!(ex.is_excluded("py/venv", "venv"));
        assert!(!ex.is_excluded("py/plain/venv", "venv"), "no pyvenv.cfg inside");
        assert!(ex.is_excluded("web/node_modules", "node_modules"));
        assert!(!ex.is_excluded("app/.git", ".git"), "git synchronised");
        // Files below an excluded folder: the whole path is checked.
        assert!(ex.is_excluded_path("rs/target/debug/app"));
        assert!(ex.is_excluded_path("web/node_modules/x/index.js"));
        assert!(!ex.is_excluded_path("rs/src/main.rs"));
        assert!(!ex.is_excluded_path("photos/target/a.jpg"));
        // A listing of the other Mac can supply the marker.
        assert!(ex.is_excluded_with("new/target", "target", &|m| *m == Marker::Sibling("Cargo.toml")));
        // Switched on again: nothing excluded; Git off excludes `.git`.
        let none = Excludes::new(&[], true).unwrap().with_ecosystems(r, &[], false);
        assert!(!none.is_excluded("web/node_modules", "node_modules"));
        assert!(none.is_excluded_path("app/.git/HEAD"));
        assert!(none.skips_git());
    }
}
