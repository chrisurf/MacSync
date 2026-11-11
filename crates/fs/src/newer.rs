//! Files every Mac rewrites on its own – an editor's window layout, plugin
//! settings. When both Macs changed one, the newer version wins instead of a
//! conflict; the other goes to the trash.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Obsidian keeps its window layout and the plugin settings in the vault and
/// writes them on every click, on each Mac.
pub const DEFAULT_NEWER_WINS: &[&str] =
    &["**/.obsidian/workspace.json", "**/.obsidian/workspace-mobile.json", "**/.obsidian/plugins/*/data.json"];

#[derive(Clone, Debug)]
pub struct NewerWins {
    globs: GlobSet,
    patterns: Vec<String>,
}

impl Default for NewerWins {
    fn default() -> Self {
        NewerWins::new(&[]).expect("empty pattern list is valid")
    }
}

impl NewerWins {
    /// `patterns` are globs matched against root-relative paths; `*` stays
    /// within one folder, `**` crosses folders.
    pub fn new(patterns: &[String]) -> Result<NewerWins, globset::Error> {
        let mut b = GlobSetBuilder::new();
        for p in patterns {
            b.add(GlobBuilder::new(p).literal_separator(true).build()?);
        }
        Ok(NewerWins { globs: b.build()?, patterns: patterns.to_vec() })
    }

    pub fn defaults() -> Vec<String> {
        DEFAULT_NEWER_WINS.iter().map(|s| s.to_string()).collect()
    }

    pub fn matches(&self, root_rel: &str) -> bool {
        !self.patterns.is_empty() && self.globs.is_match(root_rel)
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obsidian_layout_and_plugin_settings_match() {
        let n = NewerWins::new(&NewerWins::defaults()).unwrap();
        assert!(n.matches("Obsidian/verification/.obsidian/workspace.json"));
        assert!(n.matches("vault/.obsidian/workspace-mobile.json"));
        assert!(n.matches("Obsidian/verification/.obsidian/plugins/couchdb-sync/data.json"));
        assert!(!n.matches("Obsidian/verification/.obsidian/plugins/couchdb-sync/main.js"));
        assert!(!n.matches("Obsidian/verification/.obsidian/plugins/a/b/data.json"));
        assert!(!n.matches("Obsidian/verification/notes/workspace.json"));
        assert!(!NewerWins::default().matches("vault/.obsidian/workspace.json"));
    }
}
