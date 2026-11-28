//! Portable paths and restrictive write scopes for frozen snapshots.

use crate::{require, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Validate one portable repository-relative path. Symlinks are checked separately.
pub fn validate_path(path: &str) -> Result<()> {
    require(
        !path.is_empty() && path.len() <= 4096 && !path.starts_with('/') && !path.contains('\\'),
        format!("invalid relative path: {path:?}"),
    )?;
    for part in path.split('/') {
        require(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with([' ', '.'])
                && !part
                    .chars()
                    .any(|c| c.is_control() || ":*?<>|\"".contains(c)),
            format!("non-portable path: {path:?}"),
        )?;
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        require(
            !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9')),
            "reserved device path",
        )?;
    }
    Ok(())
}

/// The supported scope language is a literal path or a directory ending in `/**`.
pub fn validate_pattern(pattern: &str) -> Result<()> {
    if pattern == "**" {
        return Ok(());
    }
    validate_path(pattern.strip_suffix("/**").unwrap_or(pattern))
}

/// Match a validated pattern on path components, never a string prefix alone.
pub fn matches(pattern: &str, path: &str) -> bool {
    pattern == "**"
        || pattern == path
        || pattern
            .strip_suffix("/**")
            .is_some_and(|p| path.starts_with(&format!("{p}/")))
}

/// Authorized writes; protected paths always win.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    /// One of these patterns must allow each changed path.
    pub allow_write: Vec<String>,
    /// No changed path may match these patterns.
    pub protected: Vec<String>,
}
impl Scope {
    /// Reject empty or ambiguous scope definitions.
    pub fn validate(&self) -> Result<()> {
        require(!self.allow_write.is_empty(), "empty write scope")?;
        for patterns in [&self.allow_write, &self.protected] {
            let mut seen = BTreeSet::new();
            for p in patterns {
                validate_pattern(p)?;
                require(seen.insert(p), "duplicate scope pattern")?;
            }
        }
        Ok(())
    }
    /// Validate additions, deletions and both ends of renames identically.
    pub fn check(&self, path: &str) -> Result<()> {
        validate_path(path)?;
        require(
            !path.split('/').any(|p| p.eq_ignore_ascii_case(".git")),
            "Git metadata is never worker writable",
        )?;
        require(
            self.allow_write.iter().any(|p| matches(p, path)),
            format!("out-of-scope: {path}"),
        )?;
        require(
            !self.protected.iter().any(|p| matches(p, path)),
            format!("protected-path: {path}"),
        )
    }
}

/// Reject case collisions in a complete candidate, including unchanged files.
pub fn validate_tree_paths<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    case_insensitive: bool,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut directories = std::collections::BTreeMap::new();
    for path in paths {
        validate_path(path)?;
        let key = if case_insensitive {
            path.to_lowercase()
        } else {
            path.to_string()
        };
        require(seen.insert(key), format!("path collision: {path}"))?;
        let parts: Vec<_> = path.split('/').collect();
        for end in 1..=parts.len() {
            let prefix = parts[..end].join("/");
            let key = if case_insensitive {
                prefix.to_lowercase()
            } else {
                prefix.clone()
            };
            if let Some(old) = directories.insert(key, (prefix.clone(), end == parts.len())) {
                require(
                    old.0 == prefix && !old.1 && end < parts.len(),
                    format!("path or directory collision: {path}"),
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_and_platform_aliases_cannot_enter_a_snapshot() {
        for p in [
            "../a", "/a", "a//b", "a/./b", "a\\b", "C:a", "a\0b", "a.", "NUL.txt", "a/COM1",
        ] {
            assert!(validate_path(p).is_err(), "{p}");
        }
        assert!(validate_tree_paths(["src/A.rs", "src/a.rs"], true).is_err());
        assert!(validate_tree_paths(["Src/a.rs", "src/b.rs"], true).is_err());
        assert!(validate_tree_paths(["src", "src/a.rs"], false).is_err());
    }
    #[test]
    fn renaming_out_of_a_protected_path_is_denied() {
        let scope = Scope {
            allow_write: vec!["src/**".into()],
            protected: vec!["src/oracle/**".into()],
        };
        assert!(scope.check("src/a.rs").is_ok());
        for p in ["src/oracle/a.rs", "src-other/a.rs", ".git/config"] {
            assert!(scope.check(p).is_err());
        }
    }
}
