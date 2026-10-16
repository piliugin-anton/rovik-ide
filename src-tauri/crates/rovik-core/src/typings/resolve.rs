//! Where a package is, from the folder that imports it: the nearest
//! `node_modules/<name>` up the tree, as Node and TypeScript find it. A
//! link (pnpm links every package into its store, workspaces link members
//! into the root) is followed to the real folder, which is loaded once
//! however many links lead to it, and the link is reported so the editor
//! resolves paths through it too.

use super::Link;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// A package's real folder, and the link that led to it if one did.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub dir: PathBuf,
    pub link: Option<Link>,
}

/// The nearest `node_modules/<name>` from `from` up, through links. A
/// folder itself named `node_modules` is skipped, as Node skips it.
pub fn find(
    from: &Path,
    name: &str,
    probe: &mut impl FnMut(&Path) -> Option<Found>,
) -> Option<Found> {
    from.ancestors()
        .take(64)
        .filter(|dir| dir.file_name().is_none_or(|n| n != "node_modules"))
        .find_map(|dir| probe(&dir.join("node_modules").join(name)))
}

/// What is at `candidate`: a folder, a link to one (followed), or nothing.
pub fn entry(candidate: &Path) -> Option<Found> {
    let meta = fs::symlink_metadata(candidate).ok()?;
    if meta.is_dir() {
        return Some(Found {
            dir: candidate.to_path_buf(),
            link: None,
        });
    }
    if !meta.file_type().is_symlink() {
        return None;
    }
    let target = link_target(candidate)?;
    target.is_dir().then(|| Found {
        link: Some(Link {
            path: candidate.to_string_lossy().into_owned(),
            target: target.to_string_lossy().into_owned(),
        }),
        dir: target,
    })
}

/// Where a link leads, through links to links, as the path Node resolves:
/// relative to the link's folder, `..` and `.` folded, but no further than
/// that (`fs::canonicalize` would also resolve links above the project,
/// giving paths the editor doesn't know). None for a broken or cyclic one.
pub fn link_target(link: &Path) -> Option<PathBuf> {
    let mut current = link.to_path_buf();
    for _ in 0..8 {
        let Ok(raw) = fs::read_link(&current) else {
            return current.exists().then_some(current);
        };
        let joined = if raw.is_absolute() {
            raw
        } else {
            current.parent()?.join(raw)
        };
        current = fold_dots(&joined);
    }
    None
}

fn fold_dots(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typings::test_support::{relative, temp_project};

    #[test]
    fn the_nearest_node_modules_wins_and_node_modules_itself_is_skipped() {
        let dir = temp_project(
            "resolve-nearest",
            &[
                ("node_modules/a/index.d.ts", ""),
                ("node_modules/b/index.d.ts", ""),
                ("packages/app/node_modules/a/index.d.ts", ""),
                ("packages/app/src/x.ts", ""),
            ],
        );
        let mut probe = entry;
        let from = dir.join("packages/app/src");
        let a = find(&from, "a", &mut probe).unwrap();
        assert_eq!(
            relative(&dir, &a.dir.to_string_lossy()),
            "packages/app/node_modules/a"
        );
        assert_eq!(a.link, None);
        let b = find(&from, "b", &mut probe).unwrap();
        assert_eq!(relative(&dir, &b.dir.to_string_lossy()), "node_modules/b");
        assert_eq!(find(&from, "c", &mut probe), None);
        // From inside a package: its own node_modules folder isn't probed as
        // `node_modules/node_modules/x`, its siblings are found.
        let inside = dir.join("node_modules/a");
        let b = find(&inside, "b", &mut probe).unwrap();
        assert_eq!(relative(&dir, &b.dir.to_string_lossy()), "node_modules/b");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_followed_to_its_folder_and_reported() {
        use std::os::unix::fs::symlink;
        let dir = temp_project(
            "resolve-links",
            &[
                ("node_modules/.pnpm/foo@1/node_modules/foo/index.d.ts", ""),
                ("node_modules/file.d.ts", ""),
            ],
        );
        symlink(".pnpm/foo@1/node_modules/foo", dir.join("node_modules/foo")).unwrap();
        symlink("missing", dir.join("node_modules/broken")).unwrap();
        symlink("file.d.ts", dir.join("node_modules/to-file")).unwrap();
        let found = entry(&dir.join("node_modules/foo")).unwrap();
        let rel = |p: &str| relative(&dir, p);
        assert_eq!(
            rel(&found.dir.to_string_lossy()),
            "node_modules/.pnpm/foo@1/node_modules/foo"
        );
        let link = found.link.unwrap();
        assert_eq!(rel(&link.path), "node_modules/foo");
        assert_eq!(
            rel(&link.target),
            "node_modules/.pnpm/foo@1/node_modules/foo"
        );
        assert_eq!(entry(&dir.join("node_modules/broken")), None);
        assert_eq!(entry(&dir.join("node_modules/to-file")), None);
        assert_eq!(entry(&dir.join("node_modules/file.d.ts")), None);
    }
}
