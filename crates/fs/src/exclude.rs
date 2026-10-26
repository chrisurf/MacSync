//! Exclusion rules.
//!
//! MacSync synchronises *everything* below the root unless the user
//! explicitly excludes it. The only built-in exclusions are macOS volume
//! metadata files (switchable) and MacSync's own private directory.

use globset::{Glob, GlobSet, GlobSetBuilder};

/// File names that are Mac-local and would otherwise cause permanent
/// pseudo-changes. Can be disabled with `default_excludes = false`.
pub const DEFAULT_NAMES: &[&str] =
    &[".DS_Store", ".Spotlight-V100", ".Trashes", ".fseventsd", ".TemporaryItems", ".DocumentRevisions-V100"];

#[derive(Clone, Debug)]
pub struct Excludes {
    names: Vec<String>,
    globs: GlobSet,
    patterns: Vec<String>,
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
        })
    }

    /// `root_rel` is the path relative to the sync root, `name` its last component.
    pub fn is_excluded(&self, root_rel: &str, name: &str) -> bool {
        if crate::is_private(root_rel) {
            return true;
        }
        self.names.iter().any(|n| n == name) || (!self.patterns.is_empty() && self.globs.is_match(root_rel))
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
}
