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
/// `..` part, an absolute one, or one with a drive (`C:`, `c:/`; `a:b.txt`
/// is a file name with a colon). The project root itself is "".
pub fn norm_rel(path: &str) -> Option<String> {
    let p = path.trim().replace('\\', "/");
    let b = p.as_bytes();
    // By shape, never by case (round 3): `C:`, `c:/x`; `a:b.txt`, `C:x`
    // are names with a colon.
    let drive = b.len() > 1
        && b[1] == b':'
        && b[0].is_ascii_alphabetic()
        && (b.len() == 2 || b[2] == b'/');
    if p.starts_with('/') || drive {
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

/// A path's drive (`C:` of `C:\proj` or `c:/proj`), else "".
pub fn drive(path: &str) -> &str {
    let b = path.as_bytes();
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\') {
        &path[..2]
    } else {
        ""
    }
}

/// `src/a.rs:12:5` without its line and column, `C:/p/a.rs:12` too (a
/// drive's colon isn't one); a path without them as it is.
pub fn without_line(path: &str) -> &str {
    // From the end (round 3): `src/a:b.rs:12:5` keeps the name's colon.
    let at = drive(path).len();
    let mut p = path;
    while let Some((head, tail)) = p.rsplit_once(':') {
        let number = tail.chars().all(|c| c.is_ascii_digit());
        let name = head.len() > at && (!tail.is_empty() || head.contains(['/', '.']));
        if number && name {
            p = head;
        } else {
            break;
        }
    }
    p
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
            ("c:/x", None),
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

    /// Round 2 (§4.2): names with spaces, accents and dots are kept as
    /// they are.
    #[test]
    fn spaces_unicode_and_dots_in_names_survive() {
        for path in ["src/a b.rs", "src/día/ünïcode.ts", "src/.hidden", "src/..."] {
            assert_eq!(norm_rel(path).as_deref(), Some(path), "{path}");
        }
        assert_eq!(norm_rel("./").as_deref(), Some(""));
    }

    /// Round 2 (§4.2): Windows spellings are rejected (outside the project)
    /// or folded to `/`.
    #[test]
    fn windows_spellings_are_rejected_or_folded() {
        for path in [
            "C:/x",
            "c:\\x",
            "\\\\server\\share\\x",
            "\\\\?\\C:\\x",
            "src\\a\\..\\b",
        ] {
            assert_eq!(norm_rel(path), None, "{path}");
        }
        assert_eq!(norm_rel(".\\src\\x.ts").as_deref(), Some("src/x.ts"));
    }

    /// Round 2 (§4.2): a folder holds what's inside it, not names that
    /// start with it.
    #[test]
    fn under_is_not_a_prefix_match() {
        assert!(!under("src/api", "src/api2/x.rs"));
        assert!(!under("src/x.ts", "src/x.ts.bak"));
        assert!(under("src/api/", "src/api"));
        assert!(under_as("Src", "src/x", true));
    }

    /// Round 2, P0.1: a colon in a name isn't a drive.
    #[test]
    fn colon_in_a_name_is_not_a_drive() {
        assert_eq!(norm_rel("a:b.txt").as_deref(), Some("a:b.txt"));
        assert_eq!(norm_rel("src/a:b.txt").as_deref(), Some("src/a:b.txt"));
        for drive in ["C:", "c:", "C:/x", "c:\\x"] {
            assert_eq!(norm_rel(drive), None, "{drive}");
        }
        // Round 3: by shape, not by case: `C:x` is the name `c:x` is.
        assert_eq!(norm_rel("A:b.txt").as_deref(), Some("A:b.txt"));
        assert_eq!(norm_rel("C:x").as_deref(), Some("C:x"));
        assert_eq!(norm_rel("c:x").as_deref(), Some("c:x"));
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

    /// Round 2, P4.8: a line and column come off a path, a drive's colon
    /// doesn't.
    #[test]
    fn without_line_keeps_a_drive() {
        for (path, want) in [
            ("src/a.rs:12", "src/a.rs"),
            ("src/a.rs:12:5", "src/a.rs"),
            ("src/a.rs:", "src/a.rs"),
            ("src/a.rs", "src/a.rs"),
            ("C:/p/a.rs:12", "C:/p/a.rs"),
            ("c:\\p\\a.rs:12:5", "c:\\p\\a.rs"),
            ("C:/p/a.rs", "C:/p/a.rs"),
            ("a:b.txt", "a:b.txt"),
            ("src/a:b.rs", "src/a:b.rs"),
        ] {
            assert_eq!(without_line(path), want, "{path}");
        }
        assert_eq!(drive("C:"), "");
        assert_eq!(drive("C:x"), "");
        assert_eq!(drive("d:\\x"), "d:");
    }
}
