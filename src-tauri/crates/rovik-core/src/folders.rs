//! The folders a chat works in (docs/design/workspaces-plan.md, 4.1): a
//! folder chat's one, or a workspace chat's, each by its stored name. A
//! workspace chat's paths start with a folder's name (`api/src/x.ts`); a
//! folder chat's are its folder's own (`src/x.ts`). The overlay, the run and
//! the cloud client share this one type.

use std::path::{Component, Path, PathBuf};

/// One folder a chat works in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Its stored name (D8): the first part of a workspace chat's paths in
    /// it. "" for a folder chat's, whose paths are the folder's own.
    pub name: String,
    /// Where it is, as the window opened it.
    pub path: PathBuf,
}

/// The folders a chat works in: one named "" (a folder chat), or a
/// workspace chat's, each by its stored name, in the workspace's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folders(Vec<Folder>);

impl Folders {
    /// A folder chat's folder.
    pub fn one(path: PathBuf) -> Self {
        Folders(vec![Folder {
            name: String::new(),
            path,
        }])
    }

    /// A workspace chat's folders: at least one, each named by one plain
    /// part (no separator, not `.` or `..`), no two alike in any case, as
    /// some file systems ignore it.
    pub fn named(list: Vec<Folder>) -> Result<Self, String> {
        if list.is_empty() {
            return Err("A chat across folders needs one folder at least.".into());
        }
        let mut seen: Vec<String> = Vec::new();
        for folder in &list {
            let name = folder.name.as_str();
            let plain = matches!(
                Path::new(name).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            ) && !name.contains(['/', '\\']);
            if !plain {
                return Err(format!("“{name}” can't name a folder of a chat."));
            }
            let folded = name.to_lowercase();
            if seen.contains(&folded) {
                return Err(format!("Two of the chat's folders are named “{name}”."));
            }
            seen.push(folded);
        }
        Ok(Folders(list))
    }

    /// A folder chat's folder; None for a workspace chat's.
    pub fn single(&self) -> Option<&Path> {
        match self.0.as_slice() {
            [only] if only.name.is_empty() => Some(&only.path),
            _ => None,
        }
    }

    /// Where plans go (D28): the first folder.
    pub fn first(&self) -> &Folder {
        &self.0[0]
    }

    /// The folders, in the workspace's order.
    pub fn iter(&self) -> std::slice::Iter<'_, Folder> {
        self.0.iter()
    }

    /// The folder a chat path is in, and the path in it: `api/src/x.ts` is
    /// (api, `src/x.ts`); a folder chat's path is its folder's as it is.
    /// None for a name that's none of the chat's folders. Where the file
    /// system ignores case, the name is matched as it does.
    pub fn split<'a>(&self, path: &'a str) -> Option<(&Folder, &'a str)> {
        if self.single().is_some() {
            return Some((&self.0[0], path));
        }
        let path = path.trim_start_matches("./");
        let (name, rest) = match path.find(['/', '\\']) {
            Some(at) => (&path[..at], &path[at + 1..]),
            None => (path, ""),
        };
        let folder = self.0.iter().find(|f| f.name == name).or_else(|| {
            cfg!(any(target_os = "macos", windows))
                .then(|| {
                    self.0
                        .iter()
                        .find(|f| f.name.to_lowercase() == name.to_lowercase())
                })
                .flatten()
        })?;
        Some((folder, rest))
    }

    /// The real file a chat path names. None for a path that names none of
    /// the chat's folders, and for one that leaves its folder (`..`, or
    /// absolute).
    pub fn abs(&self, path: &str) -> Option<PathBuf> {
        let (folder, rest) = self.split(path)?;
        let mut abs = folder.path.clone();
        for part in rest
            .split(['/', '\\'])
            .filter(|p| !p.is_empty() && *p != ".")
        {
            match Path::new(part).components().next() {
                Some(Component::Normal(_)) => abs.push(part),
                _ => return None,
            }
        }
        Some(abs)
    }

    /// The chat path of a real file, `/`-separated: the folder's name, then
    /// its path in that folder (the innermost folder, should one hold
    /// another). Its folder may be spelled with links resolved. None outside
    /// the chat's folders.
    pub fn chat_path(&self, abs: &Path) -> Option<String> {
        let mut by_depth: Vec<&Folder> = self.0.iter().collect();
        by_depth.sort_by_key(|f| std::cmp::Reverse(f.path.components().count()));
        let found = by_depth
            .iter()
            .find_map(|f| Some((*f, abs.strip_prefix(&f.path).ok()?.to_path_buf())))
            .or_else(|| {
                let resolved = abs.canonicalize().ok()?;
                by_depth.iter().find_map(|f| {
                    let folder = f.path.canonicalize().ok()?;
                    Some((*f, resolved.strip_prefix(folder).ok()?.to_path_buf()))
                })
            });
        let (folder, rest) = found?;
        let parts: Vec<String> = rest
            .components()
            .map(|c| match c {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Option<_>>()?;
        Some(join(&folder.name, &parts.join("/")))
    }

    /// For reading paths in commands' output: each folder's path, its
    /// spelling with links resolved and its place in the view at `view`,
    /// each with what it reads as (`name/`, or "" for a folder chat's), and
    /// the view's root as "". Longest first, so a folder's place in the view
    /// is matched before the view's root.
    pub fn output_roots(&self, view: &Path) -> Vec<(PathBuf, String)> {
        let mut roots: Vec<(PathBuf, String)> = Vec::new();
        let mut add = |path: PathBuf, prefix: &str| {
            if !roots.iter().any(|(p, _)| *p == path) {
                roots.push((path, prefix.to_string()));
            }
        };
        let views: Vec<PathBuf> = std::iter::once(view.to_path_buf())
            .chain(view.canonicalize().ok())
            .collect();
        for folder in &self.0 {
            let prefix = match folder.name.is_empty() {
                true => String::new(),
                false => format!("{}/", folder.name),
            };
            add(folder.path.clone(), &prefix);
            if let Ok(resolved) = folder.path.canonicalize() {
                add(resolved, &prefix);
            }
            if !folder.name.is_empty() {
                for view in &views {
                    add(view.join(&folder.name), &prefix);
                }
            }
        }
        for view in views {
            add(view, "");
        }
        roots.sort_by_key(|(path, _)| std::cmp::Reverse(path.as_os_str().len()));
        roots
    }
}

/// A chat path: the folder's name, then the path in it (`api/src/x.ts`);
/// a folder chat's is the path itself.
fn join(name: &str, rest: &str) -> String {
    match (name.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (false, true) => name.to_string(),
        (false, false) => format!("{name}/{rest}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(base: &Path) -> Folders {
        Folders::named(vec![
            Folder {
                name: "api".into(),
                path: base.join("api"),
            },
            Folder {
                name: "web".into(),
                path: base.join("shop").join("web"),
            },
        ])
        .unwrap()
    }

    #[test]
    fn a_chat_path_splits_by_its_first_part() {
        let folders = workspace(Path::new("/w"));
        let (folder, rest) = folders.split("api/src/x.ts").unwrap();
        assert_eq!((folder.name.as_str(), rest), ("api", "src/x.ts"));
        let (folder, rest) = folders.split("web").unwrap();
        assert_eq!((folder.name.as_str(), rest), ("web", ""));
        assert!(folders.split("docs/readme.md").is_none(), "no such folder");
        assert!(folders.split("").is_none());
    }

    #[test]
    fn abs_and_chat_path_go_and_come_back() {
        let base = crate::testing::temp_dir();
        std::fs::create_dir_all(base.0.join("api/src")).unwrap();
        std::fs::create_dir_all(base.0.join("shop/web")).unwrap();
        let folders = workspace(&base.0);
        let abs = folders.abs("web/src/y.ts").unwrap();
        assert_eq!(
            abs,
            base.0.join("shop").join("web").join("src").join("y.ts")
        );
        assert_eq!(folders.chat_path(&abs).as_deref(), Some("web/src/y.ts"));
        assert_eq!(
            folders.chat_path(&base.0.join("api")).as_deref(),
            Some("api"),
            "a folder itself is its name"
        );
        // Outside every folder, or leaving one: none.
        assert_eq!(folders.chat_path(&base.0.join("other/z.ts")), None);
        assert_eq!(folders.abs("api/../web/y.ts"), None);
        assert_eq!(folders.abs("other/y.ts"), None);
    }

    #[test]
    fn a_folder_chats_paths_are_its_folders() {
        let folders = Folders::one(PathBuf::from("/w/api"));
        assert_eq!(folders.single(), Some(Path::new("/w/api")));
        let (folder, rest) = folders.split("src/x.ts").unwrap();
        assert_eq!((folder.name.as_str(), rest), ("", "src/x.ts"));
        assert_eq!(
            folders.abs("src/x.ts"),
            Some(Path::new("/w/api").join("src").join("x.ts"))
        );
        assert_eq!(
            folders.chat_path(&Path::new("/w/api").join("src").join("x.ts")),
            Some("src/x.ts".into())
        );
        assert_eq!(folders.first().path, PathBuf::from("/w/api"));
        // A workspace chat of one folder is still a workspace chat's.
        let named = Folders::named(vec![Folder {
            name: "api".into(),
            path: "/w/api".into(),
        }])
        .unwrap();
        assert_eq!(named.single(), None);
        assert_eq!(
            named.abs("api/src/x.ts"),
            Some(Path::new("/w/api").join("src").join("x.ts"))
        );
    }

    #[test]
    fn a_workspace_chats_folders_have_plain_names_none_alike() {
        let folder = |name: &str| Folder {
            name: name.into(),
            path: PathBuf::from("/w").join(name),
        };
        assert!(Folders::named(Vec::new()).is_err());
        for bad in ["", ".", "..", "a/b", "a\\b"] {
            assert!(Folders::named(vec![folder(bad)]).is_err(), "{bad:?}");
        }
        assert!(Folders::named(vec![folder("api"), folder("API")]).is_err());
        assert!(Folders::named(vec![folder("api"), folder("web")]).is_ok());
    }

    #[test]
    fn output_roots_map_a_folders_path_its_resolved_spelling_and_its_place_in_the_view() {
        let base = crate::testing::temp_dir();
        std::fs::create_dir_all(base.0.join("api")).unwrap();
        std::fs::create_dir_all(base.0.join("shop/web")).unwrap();
        std::fs::create_dir_all(base.0.join("view/api")).unwrap();
        let folders = workspace(&base.0);
        let view = base.0.join("view");
        let roots = folders.output_roots(&view);
        let reads_as = |path: &Path| {
            roots
                .iter()
                .find(|(root, _)| root == path)
                .map(|(_, prefix)| prefix.as_str())
        };
        assert_eq!(reads_as(&base.0.join("api")), Some("api/"));
        assert_eq!(reads_as(&base.0.join("shop").join("web")), Some("web/"));
        assert_eq!(reads_as(&view.join("web")), Some("web/"));
        assert_eq!(reads_as(&view), Some(""), "the view's root");
        let resolved = base.0.join("api").canonicalize().unwrap();
        assert_eq!(reads_as(&resolved), Some("api/"));
        // A folder's place in the view is tried before the view's root.
        let at = |path: &Path| roots.iter().position(|(root, _)| root == path).unwrap();
        assert!(at(&view.join("api")) < at(&view));

        // A folder chat's folder and view both read as its own paths.
        let one = Folders::one(base.0.join("api"));
        let roots = one.output_roots(&view);
        assert!(
            roots.iter().all(|(_, prefix)| prefix.is_empty()),
            "{roots:?}"
        );
        assert!(roots.iter().any(|(root, _)| *root == base.0.join("api")));
    }
}
