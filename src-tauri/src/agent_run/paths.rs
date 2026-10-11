//! One way to read a repo-relative path (P0.1). Every check that compares
//! paths (write scopes, group owners, hidden files, checkpoints) goes
//! through these, so no spelling of a path slips past one of them.
//!
//! `norm_rel` keeps the path's case (it's also shown and written to);
//! `key` is for comparing: on macOS and Windows any case names the same
//! file, so it's folded there.

/// Whether paths compare in any case here.
pub const ANY_CASE: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// `path` as a repo-relative path with `/`: no `.` or empty parts, no
/// trailing `/`. None for a path that isn't inside the project: one with a
/// `..` part, an absolute one, or one with a drive (`C:`). The project root
/// itself is "".
pub fn norm_rel(path: &str) -> Option<String> {
    let p = path.trim().replace('\\', "/");
    let b = p.as_bytes();
    if p.starts_with('/') || (b.len() > 1 && b[1] == b':' && b[0].is_ascii_alphabetic()) {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// `path` for comparing with another (see the module's note).
pub fn key(path: &str) -> Option<String> {
    key_as(path, ANY_CASE)
}

pub fn key_as(path: &str, any_case: bool) -> Option<String> {
    norm_rel(path).map(|p| if any_case { p.to_lowercase() } else { p })
}

/// Whether two paths name the same file in the project.
pub fn same(a: &str, b: &str) -> bool {
    matches!((key(a), key(b)), (Some(a), Some(b)) if a == b)
}

/// Whether `path` is `folder` or inside it (the root "" holds everything).
pub fn under(folder: &str, path: &str) -> bool {
    under_as(folder, path, ANY_CASE)
}

pub fn under_as(folder: &str, path: &str, any_case: bool) -> bool {
    let (Some(folder), Some(path)) = (key_as(folder, any_case), key_as(path, any_case)) else {
        return false;
    };
    folder.is_empty()
        || path == folder
        || path
            .strip_prefix(&folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The folder `path` is in ("" for the root).
pub fn parent(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_path_is_one_path() {
        for (path, want) in [
            ("src/./x.ts", Some("src/x.ts")),
            ("src//x.ts", Some("src/x.ts")),
            ("src/x.ts/.", Some("src/x.ts")),
            ("src\\x.ts", Some("src/x.ts")),
            ("./src/x.ts", Some("src/x.ts")),
            (" src/x.ts ", Some("src/x.ts")),
            ("SRC/X.ts", Some("SRC/X.ts")),
            ("src/../x", None),
            ("..", None),
            ("/abs", None),
            ("C:\\x", None),
            ("c:x", None),
            ("src/", Some("src")),
            ("src", Some("src")),
            ("", Some("")),
            (".", Some("")),
        ] {
            assert_eq!(norm_rel(path).as_deref(), want, "{path}");
        }
        assert_eq!(key_as("SRC/X.ts", true).as_deref(), Some("src/x.ts"));
        assert_eq!(key_as("SRC/X.ts", false).as_deref(), Some("SRC/X.ts"));
    }

    #[test]
    fn under_a_folder() {
        assert!(under("src", "src/x.ts"));
        assert!(under("src/", "./src//x.ts"));
        assert!(under("src", "src"));
        assert!(!under("src", "srcx/a.ts"));
        assert!(!under("src", "src/../x"));
        assert!(under("", "anything/at/all"));
        assert!(under_as("SRC", "src/x.ts", true));
        assert!(!under_as("SRC", "src/x.ts", false));
        assert!(same("src/x.ts", "./src\\x.ts"));
        assert!(!same("src/../x", "x"));
    }
}
