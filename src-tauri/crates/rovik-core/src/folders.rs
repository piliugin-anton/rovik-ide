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
pub struct Folders {
    list: Vec<Folder>,
    /// A workspace chat's workspace (its project's ID), whose chats share
    /// their build folder and their commands' history. None for a folder
    /// chat's folder, and where only the folders matter (a landing's).
    workspace: Option<String>,
}

impl Folders {
    /// A folder chat's folder.
    pub fn one(path: PathBuf) -> Self {
        Folders {
            list: vec![Folder {
                name: String::new(),
                path,
            }],
            workspace: None,
        }
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
        Ok(Folders {
            list,
            workspace: None,
        })
    }

    /// The same folders, of the workspace `id`.
    pub fn in_workspace(mut self, id: &str) -> Self {
        self.workspace = Some(id.to_string());
        self
    }

    /// The workspace a workspace chat's folders are of, when it's known.
    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    /// A folder chat's folder; None for a workspace chat's.
    pub fn single(&self) -> Option<&Path> {
        match self.list.as_slice() {
            [only] if only.name.is_empty() => Some(&only.path),
            _ => None,
        }
    }

    /// Where plans go (D28): the first folder.
    pub fn first(&self) -> &Folder {
        &self.list[0]
    }

    /// The folders, in the workspace's order.
    pub fn iter(&self) -> std::slice::Iter<'_, Folder> {
        self.list.iter()
    }

    /// The folder a chat path is in, and the path in it: `api/src/x.ts` is
    /// (api, `src/x.ts`); a folder chat's path is its folder's as it is.
    /// None for a name that's none of the chat's folders. Where the file
    /// system ignores case, the name is matched as it does.
    pub fn split<'a>(&self, path: &'a str) -> Option<(&Folder, &'a str)> {
        if self.single().is_some() {
            return Some((&self.list[0], path));
        }
        let path = path.trim_start_matches("./");
        let (name, rest) = match path.find(separator) {
            Some(at) => (&path[..at], &path[at + 1..]),
            None => (path, ""),
        };
        let folder = self.list.iter().find(|f| f.name == name).or_else(|| {
            cfg!(any(target_os = "macos", windows))
                .then(|| {
                    self.list
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
        for part in rest.split(separator).filter(|p| !p.is_empty() && *p != ".") {
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
        let mut by_depth: Vec<&Folder> = self.list.iter().collect();
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

    /// For reading paths in commands' output run at `view` ([`OutputRoots`]):
    /// each folder's path, its spelling with links resolved and its place in
    /// the view, each read as `name/` (a folder chat's as its own paths), and
    /// the view's root as the chat's.
    pub fn output_roots(&self, view: &Path) -> OutputRoots {
        let views: Vec<PathBuf> = std::iter::once(view.to_path_buf())
            .chain(view.canonicalize().ok())
            .collect();
        let mut roots: Vec<(PathBuf, String)> = Vec::new();
        for folder in &self.list {
            let prefix = match folder.name.is_empty() {
                true => String::new(),
                false => format!("{}/", folder.name),
            };
            roots.push((folder.path.clone(), prefix.clone()));
            if let Ok(resolved) = folder.path.canonicalize() {
                roots.push((resolved, prefix.clone()));
            }
            if !folder.name.is_empty() {
                for view in &views {
                    roots.push((view.join(&folder.name), prefix.clone()));
                }
            }
        }
        for view in &views {
            roots.push((view.clone(), String::new()));
        }
        OutputRoots::new(roots, view)
    }
}

/// How a chat's commands' output names its files ([`Folders::output_roots`]):
/// an absolute path in one of the chat's folders, in their places in its
/// view or in the view itself is a chat path (`/home/me/api/src/x.ts` is
/// `api/src/x.ts`; in a folder chat of `/home/me/api`, `src/x.ts`), and a path
/// a command printed relative to the folder it ran in is that folder's file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputRoots {
    /// Each folder, `/`-separated, and what a path in it reads as: the
    /// longest first, so a folder's place in the view is matched before the
    /// view's root.
    roots: Vec<(String, String)>,
    /// Where the commands ran: the chat's view, or its folder.
    view: Option<PathBuf>,
}

impl OutputRoots {
    fn new(roots: Vec<(PathBuf, String)>, view: &Path) -> OutputRoots {
        let mut list: Vec<(String, String)> = Vec::new();
        for (path, prefix) in roots {
            let path = path.to_string_lossy().replace('\\', "/");
            let path = path.trim_end_matches('/');
            // `/` alone would read every absolute path as the chat's.
            if path.len() > 1 && !list.iter().any(|(p, _)| p == path) {
                list.push((path.to_string(), prefix));
            }
        }
        list.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        OutputRoots {
            roots: list,
            view: Some(view.to_path_buf()),
        }
    }

    /// The folders, for looking in (a Python module of the project's own).
    pub fn paths(&self) -> Vec<String> {
        self.roots.iter().map(|(path, _)| path.clone()).collect()
    }

    /// An absolute `path` as a chat path, when it's in one of the folders.
    pub fn chat_path(&self, path: &str) -> Option<String> {
        let path = path.replace('\\', "/");
        self.roots.iter().find_map(|(root, prefix)| {
            let rest = path.strip_prefix(root.as_str())?.strip_prefix('/')?;
            Some(format!("{prefix}{rest}"))
        })
    }

    /// `text` with every path in one of the folders written as a chat path.
    pub fn in_text(&self, text: &str) -> String {
        self.roots
            .iter()
            .fold(text.to_string(), |text, (root, prefix)| {
                text.replace(&format!("{root}/"), prefix)
            })
    }

    /// `path` as a command run in `folder` of the view printed it
    /// (`cd api && …`): relative to that folder, so its file there when
    /// there is one (`src/a.ts` is `api/src/a.ts`). Otherwise, an absolute
    /// path or one already the chat's, as it is.
    pub fn from_folder(&self, folder: &str, path: &str) -> String {
        let Some(view) = self.view.as_ref().filter(|_| !folder.is_empty()) else {
            return path.to_string();
        };
        match crate::agent_run::paths::norm_rel(&format!("{folder}/{path}")) {
            Some(in_folder) if view.join(&in_folder).exists() => in_folder,
            _ => path.to_string(),
        }
    }
}

/// Whether `c` separates a chat path's parts: `/`, and on Windows `\\` too
/// (elsewhere a file's name may hold one).
fn separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
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
        assert_eq!(folders.workspace(), None);
        // A workspace chat of one folder is still a workspace chat's.
        let named = Folders::named(vec![Folder {
            name: "api".into(),
            path: "/w/api".into(),
        }])
        .unwrap()
        .in_workspace("w1");
        assert_eq!(named.single(), None);
        assert_eq!(named.workspace(), Some("w1"));
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
    fn output_roots_read_a_folders_path_its_resolved_spelling_and_its_place_in_the_view() {
        let base = crate::testing::temp_dir();
        std::fs::create_dir_all(base.0.join("api")).unwrap();
        std::fs::create_dir_all(base.0.join("shop/web")).unwrap();
        std::fs::create_dir_all(base.0.join("view/api")).unwrap();
        let folders = workspace(&base.0);
        let view = base.0.join("view");
        let roots = folders.output_roots(&view);
        let read = |path: &Path| roots.chat_path(&format!("{}/x.ts", path.display()));
        assert_eq!(read(&base.0.join("api")).as_deref(), Some("api/x.ts"));
        let web = base.0.join("shop").join("web");
        assert_eq!(read(&web).as_deref(), Some("web/x.ts"));
        let resolved = base.0.join("api").canonicalize().unwrap();
        assert_eq!(read(&resolved).as_deref(), Some("api/x.ts"));
        // A folder's place in the view is read before the view's root.
        assert_eq!(read(&view.join("web")).as_deref(), Some("web/x.ts"));
        assert_eq!(read(&view).as_deref(), Some("x.ts"), "the view's root");
        assert_eq!(read(&base.0.join("elsewhere")), None);
        // Text is read with the roots' `/` (`chat_path` takes either).
        let slashed = |path: &Path| path.to_string_lossy().replace('\\', "/");
        let printed = format!("{}/src/a.ts: error\n{}/b.ts", slashed(&web), slashed(&view));
        assert_eq!(roots.in_text(&printed), "web/src/a.ts: error\nb.ts");

        // A folder chat's folder and view both read as its own paths.
        let one = Folders::one(base.0.join("api"));
        let roots = one.output_roots(&view);
        let read = |path: &Path| roots.chat_path(&format!("{}/x.ts", path.display()));
        assert_eq!(read(&base.0.join("api")).as_deref(), Some("x.ts"));
        assert_eq!(read(&view).as_deref(), Some("x.ts"));
        // The longest root first: one inside another goes whole. `/` reads
        // no path as the chat's.
        let nested = Folders::one(PathBuf::from("/a/b")).output_roots(Path::new("/a/b/c"));
        assert_eq!(
            nested.in_text("/a/b/c/x.ts /a/b/y.ts /a/bc/z.ts"),
            "x.ts y.ts /a/bc/z.ts"
        );
        let all = Folders::one(PathBuf::from("/")).output_roots(Path::new("/"));
        assert_eq!(all.in_text("/x.ts"), "/x.ts");
        assert_eq!(all.chat_path("/x.ts"), None);
    }

    /// A path a command printed relative to the folder it ran in (`cd api &&
    /// …`) is that folder's file, where there's one; else as it was.
    #[test]
    fn a_path_printed_in_a_folder_is_tried_as_that_folders_first() {
        let base = crate::testing::temp_dir();
        let view = base.0.join("view");
        std::fs::create_dir_all(view.join("api/src")).unwrap();
        std::fs::write(view.join("api/src/a.ts"), "").unwrap();
        let roots = workspace(&base.0).output_roots(&view);
        assert_eq!(roots.from_folder("api", "src/a.ts"), "api/src/a.ts");
        assert_eq!(roots.from_folder("api", "./src/a.ts"), "api/src/a.ts");
        // Already the chat's (an absolute path, read), or no file there.
        assert_eq!(roots.from_folder("api", "api/src/a.ts"), "api/src/a.ts");
        assert_eq!(roots.from_folder("web", "src/a.ts"), "src/a.ts");
        assert_eq!(roots.from_folder("api", "../web/a.ts"), "../web/a.ts");
        assert_eq!(roots.from_folder("", "src/a.ts"), "src/a.ts");
        assert_eq!(
            OutputRoots::default().from_folder("api", "src/a.ts"),
            "src/a.ts"
        );
    }
}
