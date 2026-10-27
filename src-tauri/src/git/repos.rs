//! The repositories the window's folders are in (D10): what the Git panel
//! shows, a section per repository, and the scopes its commands work in. A
//! project is the list of one folder; a workspace's folders may share a
//! repository or be in one each.

use super::open_repo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// A folder of the window, as the chats and the tree name it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Folder {
    pub path: String,
    /// Its stored name: a workspace's name for it (D8), or its folder's name.
    pub name: String,
}

/// The repository one or more of the window's folders are in.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Repo {
    /// Its canonical git folder (`Repository::path()`): two folders in one
    /// repository share it; a linked worktree has its own.
    pub key: String,
    /// Its working folder, as the window spells its folders: the window finds
    /// a tab's file in it (`repoFileOf`) by its path.
    pub workdir: String,
    /// The folder's stored name when the repository is exactly one of the
    /// window's folders; else the working folder's name, with its parent's
    /// when two clash.
    pub name: String,
    /// The window's folders in it, relative to `workdir` ("" when a folder is
    /// the working folder: all of it), in the window's order.
    pub scopes: Vec<String>,
    /// The same folders as the window gave them: the panel asks git from the
    /// first, and a change in any of them refreshes the repository's section.
    pub folders: Vec<String>,
}

/// What `git_repos` answers.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Repos {
    pub repos: Vec<Repo>,
    /// The folders in no repository: each has a section of its own, offering
    /// to make one.
    pub without: Vec<Folder>,
}

/// A folder's repository, as `repos_of` collects it.
struct Found {
    key: String,
    workdir: PathBuf,
    scope: String,
}

/// The window's folders by repository, in the window's order: one entry per
/// repository, whose `scopes` are its folders.
pub fn repos_of(folders: &[Folder]) -> Repos {
    let mut repos: Vec<Repo> = Vec::new();
    let mut without: Vec<Folder> = Vec::new();
    let mut workdirs: Vec<PathBuf> = Vec::new();
    for folder in folders {
        let Some(found) = find_repo(folder) else {
            without.push(folder.clone());
            continue;
        };
        match repos.iter_mut().position(|repo| repo.key == found.key) {
            Some(at) => {
                if !repos[at].scopes.contains(&found.scope) {
                    repos[at].scopes.push(found.scope);
                }
                repos[at].folders.push(folder.path.clone());
            }
            None => {
                repos.push(Repo {
                    key: found.key,
                    workdir: display_path(&found.workdir),
                    name: String::new(),
                    scopes: vec![found.scope],
                    folders: vec![folder.path.clone()],
                });
                workdirs.push(found.workdir);
            }
        }
    }
    name_repos(&mut repos, &workdirs, folders);
    Repos { repos, without }
}

/// A folder's repository: its canonical git folder, its working folder, and
/// the folder's path inside it ("" when the folder is the working folder).
/// None for a folder in no repository.
fn find_repo(folder: &Folder) -> Option<Found> {
    let repo = open_repo(&folder.path).ok()?;
    let workdir = repo.workdir()?;
    let key = fs::canonicalize(repo.path()).unwrap_or_else(|_| repo.path().to_path_buf());
    let path = Path::new(&folder.path);
    let scope = relative_to(workdir, path);
    Some(Found {
        key: display_path(&key),
        workdir: spelled_as(path, &scope, workdir),
        scope,
    })
}

/// `workdir` spelled as the window spells `folder`, the folder `scope` names
/// in it: the folder's path with the scope's parts taken off. Git names the
/// working folder as the disk does, links resolved (macOS's temp folders are
/// under `/private`, Windows expands short names like `RUNNER~1`), and the
/// window's paths of the folder's files are found in it only under the
/// window's spelling. `workdir` itself when the folder's path doesn't lead
/// there (it was opened through a link inside the repository).
fn spelled_as(folder: &Path, scope: &str, workdir: &Path) -> PathBuf {
    let mut spelled = folder.to_path_buf();
    for _ in scope.split('/').filter(|part| !part.is_empty()) {
        spelled.pop();
    }
    match (spelled.canonicalize(), workdir.canonicalize()) {
        (Ok(spelled_at), Ok(workdir_at)) if spelled_at == workdir_at => spelled,
        _ => workdir.to_path_buf(),
    }
}

/// `folder` inside `workdir`, "/"-separated: "" for the working folder itself
/// (all of it), and for a folder outside it (a link elsewhere, or a working
/// folder that can't be resolved).
fn relative_to(workdir: &Path, folder: &Path) -> String {
    let (Ok(workdir), Ok(folder)) = (workdir.canonicalize(), folder.canonicalize()) else {
        return String::new();
    };
    let Ok(rel) = folder.strip_prefix(&workdir) else {
        return String::new();
    };
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    parts.join("/")
}

/// A path as the app spells it: `\` on Windows (libgit2 writes `/`), no
/// trailing separator.
fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let text = if cfg!(windows) {
        text.replace('/', "\\")
    } else {
        text.into_owned()
    };
    text.trim_end_matches(['/', '\\']).to_string()
}

/// The last part of a path: its folder's name (the path itself at a root).
fn base_name(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let after = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    if after.is_empty() {
        trimmed.to_string()
    } else {
        after.to_string()
    }
}

/// The folders' own names, where a repository is exactly one of them: the
/// name the chats know that folder by. Otherwise the working folder's name,
/// with its parent's in front when two repositories would share one.
fn name_repos(repos: &mut [Repo], workdirs: &[PathBuf], folders: &[Folder]) {
    let stored: HashMap<PathBuf, &str> = folders
        .iter()
        .filter_map(|folder| {
            let path = fs::canonicalize(&folder.path).ok()?;
            Some((path, folder.name.as_str()))
        })
        .collect();
    let mut names: Vec<String> = repos
        .iter()
        .zip(workdirs)
        .map(|(repo, workdir)| {
            let sole_folder = repo.scopes.len() == 1 && repo.scopes[0].is_empty();
            let own = fs::canonicalize(workdir)
                .ok()
                .and_then(|path| stored.get(&path).map(|name| (*name).to_string()));
            match own {
                Some(name) if sole_folder => name,
                _ => base_name(&repo.workdir),
            }
        })
        .collect();
    // Two repositories named alike are told apart by their parents.
    for i in 0..names.len() {
        if names.iter().filter(|name| **name == names[i]).count() < 2 {
            continue;
        }
        let parent = base_name(&repos[i].workdir);
        let above = Path::new(&repos[i].workdir)
            .parent()
            .map(|p| base_name(&display_path(p)))
            .unwrap_or_default();
        if !above.is_empty() && above != parent {
            names[i] = format!("{above}/{parent}");
        }
    }
    for (repo, name) in repos.iter_mut().zip(names) {
        repo.name = name;
    }
}

#[tauri::command]
pub async fn git_repos(folders: Vec<Folder>) -> Result<Repos, String> {
    crate::util::blocking(move || Ok(repos_of(&folders))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &Path, name: &str) -> Folder {
        Folder {
            path: path.to_string_lossy().into_owned(),
            name: name.to_string(),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("git-repos-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn commit_all(repo: &git2::Repository) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }

    #[test]
    fn two_folders_of_one_repository_are_one_repository_with_two_scopes() {
        let dir = temp("shared");
        let repo = git2::Repository::init(&dir).unwrap();
        fs::create_dir_all(dir.join("app/src")).unwrap();
        fs::create_dir_all(dir.join("web")).unwrap();
        commit_all(&repo);
        let repos = repos_of(&[
            folder(&dir.join("app"), "api"),
            folder(&dir.join("web"), "storefront"),
        ]);
        assert_eq!(repos.without, []);
        assert_eq!(repos.repos.len(), 1, "one repository");
        assert_eq!(repos.repos[0].scopes, ["app", "web"]);
        assert_eq!(
            repos.repos[0].folders,
            [
                dir.join("app").to_string_lossy(),
                dir.join("web").to_string_lossy()
            ],
            "each folder as the window gave it, in its order"
        );
        assert_eq!(repos.repos[0].workdir, display_path(&dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_working_folder_is_spelled_as_the_window_spells_its_folder() {
        let dir = temp("spelled");
        let real = dir.join("real");
        fs::create_dir_all(real.join("app")).unwrap();
        git2::Repository::init(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.join("link")).unwrap();
        // Opened through a link above the repository: the tabs' paths are
        // under the link, and so is the working folder they're found in.
        let repos = repos_of(&[folder(&dir.join("link/app"), "app")]);
        assert_eq!(repos.repos[0].scopes, ["app"]);
        assert_eq!(repos.repos[0].workdir, display_path(&dir.join("link")));
        // Opened through a link inside it: no path of the window's leads to
        // the working folder, which keeps the disk's own.
        std::os::unix::fs::symlink(real.join("app"), dir.join("app-link")).unwrap();
        let repos = repos_of(&[folder(&dir.join("app-link"), "app")]);
        assert_eq!(repos.repos[0].scopes, ["app"]);
        assert_eq!(
            repos.repos[0].workdir,
            display_path(&fs::canonicalize(&real).unwrap())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_is_the_repository_takes_its_stored_name() {
        let dir = temp("sole");
        git2::Repository::init(&dir).unwrap();
        let repos = repos_of(&[folder(&dir, "api")]);
        assert_eq!(repos.repos[0].name, "api");
        assert_eq!(repos.repos[0].scopes, [""], "all of it");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_in_no_repository_is_listed_as_such() {
        let dir = temp("loose");
        fs::create_dir_all(dir.join("notes")).unwrap();
        let repos = repos_of(&[folder(&dir.join("notes"), "notes")]);
        assert!(repos.repos.is_empty());
        assert_eq!(
            repos.without,
            [folder(&dir.join("notes"), "notes")],
            "the panel offers to make one"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_worktree_is_a_repository_of_its_own() {
        let dir = temp("worktree");
        let repo = git2::Repository::init(&dir).unwrap();
        fs::write(dir.join("a.txt"), "a\n").unwrap();
        commit_all(&repo);
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let branch = repo.branch("other", &head, false).unwrap();
        let tree_path = dir.with_extension("worktree-other");
        let _ = fs::remove_dir_all(&tree_path);
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(branch.get()));
        repo.worktree("other", &tree_path, Some(&opts)).unwrap();
        let repos = repos_of(&[folder(&dir, "main"), folder(&tree_path, "other")]);
        assert_eq!(
            repos.repos.len(),
            2,
            "a linked worktree has its own git folder"
        );
        assert_ne!(repos.repos[0].key, repos.repos[1].key);
        drop(branch);
        let _ = fs::remove_dir_all(&tree_path);
        let _ = fs::remove_dir_all(&dir);
    }
}
