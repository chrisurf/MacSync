//! Helpers for unit-relative paths (`/`-separated, NFC, no `..`).

use unicode_normalization::UnicodeNormalization;

/// Last component of a relative path (`""` for the unit root).
pub fn file_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Parent of a relative path. The parent of a top-level entry is `""`.
/// The unit root has no parent.
pub fn parent(path: &str) -> Option<&str> {
    if path.is_empty() {
        return None;
    }
    Some(match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    })
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Number of components (`""` → 0, `a` → 1, `a/b` → 2).
pub fn depth(path: &str) -> usize {
    if path.is_empty() {
        0
    } else {
        path.matches('/').count() + 1
    }
}

/// True if `path` equals `prefix` or lies below it. The empty prefix contains
/// every path.
pub fn is_within(path: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || path == prefix
        || (path.len() > prefix.len() && path.starts_with(prefix) && path.as_bytes()[prefix.len()] == b'/')
}

/// NFC normalisation used for path keys.
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Key used to detect case collisions on case-insensitive volumes.
pub fn case_key(s: &str) -> String {
    nfc(s).to_lowercase()
}

/// Validate a relative path received from a peer. Rejects absolute paths,
/// `.`/`..` components, empty components, backslashes and NUL bytes so that a
/// malicious peer can never address files outside the unit.
pub fn validate(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Ok(());
    }
    if path.starts_with('/') || path.ends_with('/') {
        return Err(format!("not a relative path: {path:?}"));
    }
    for c in path.split('/') {
        if c.is_empty() || c == "." || c == ".." || c.contains('\0') || c.contains('\\') {
            return Err(format!("invalid path component in {path:?}"));
        }
    }
    Ok(())
}

/// Validate a unit name (relative path below the root, or `.`).
pub fn validate_unit(name: &str) -> Result<(), String> {
    if name == crate::entry::ROOT_UNIT {
        return Ok(());
    }
    if name.is_empty() {
        return Err("empty unit name".into());
    }
    validate(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components() {
        assert_eq!(file_name("a/b/c"), "c");
        assert_eq!(file_name("c"), "c");
        assert_eq!(parent("a/b/c"), Some("a/b"));
        assert_eq!(parent("c"), Some(""));
        assert_eq!(parent(""), None);
        assert_eq!(join("", "x"), "x");
        assert_eq!(join("a", "x"), "a/x");
        assert_eq!(depth(""), 0);
        assert_eq!(depth("a/b"), 2);
    }

    #[test]
    fn within() {
        assert!(is_within("a/b", "a"));
        assert!(is_within("a", "a"));
        assert!(!is_within("ab", "a"));
        assert!(is_within("x", ""));
    }

    #[test]
    fn validation_rejects_escapes() {
        for bad in ["/etc/passwd", "../x", "a/../b", "a//b", "a/./b", "a/", "a\\b"] {
            assert!(validate(bad).is_err(), "{bad}");
        }
        for good in ["", "a", "a/b.c", ".git/HEAD", "ü/ß"] {
            assert!(validate(good).is_ok(), "{good}");
        }
        assert!(validate_unit(".").is_ok());
        assert!(validate_unit("").is_err());
    }

    #[test]
    fn nfc_and_case() {
        let nfd = "u\u{308}"; // u + combining diaeresis
        assert_eq!(nfc(nfd), "ü");
        assert_eq!(case_key("Foo.JS"), case_key("foo.js"));
    }
}
