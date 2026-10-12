//! What the editor's TypeScript IntelliSense gets from the project: its own
//! JavaScript/TypeScript sources (`sources`) and the type declarations of
//! its dependencies (`node_modules`). Monaco's TypeScript worker only knows
//! files it's given, so without these an import of anything not open in a
//! tab can't be resolved: no go-to-definition, hover types or completions
//! for it.
//!
//! Each set is read in one walk, within `budget::Limits`, and crosses to the
//! webview as one payload that stays in memory there (src/lib/intellisense):
//! a huge repository or dependency tree is cut off, never loaded whole.

pub mod budget;
pub mod node_modules;
pub mod sources;

use ignore::WalkBuilder;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Folders never worth looking into, even when not gitignored.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "out",
    "target",
    ".next",
    ".svelte-kit",
    "coverage",
];

/// A walk of the project as git sees it (.gitignore respected, hidden
/// files skipped), with dependencies and build output pruned.
pub(crate) fn project_walk(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .filter_entry(|e| {
            !(e.file_type().is_some_and(|t| t.is_dir())
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SKIP_DIRS.contains(&n)))
        })
        .build()
}

#[derive(Serialize)]
pub struct LoadedFile {
    /// Absolute path, as the file tree uses it.
    pub path: String,
    pub content: String,
}

/// A symbolic link to a folder, as `node_modules` is full of them (pnpm,
/// workspaces, `npm link`). Files are loaded under their real path only;
/// the editor resolves a path through the links, as Node does.
#[derive(Serialize)]
pub struct Link {
    pub path: String,
    pub target: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedFiles {
    pub files: Vec<LoadedFile>,
    pub links: Vec<Link>,
    /// A limit was hit: some files weren't loaded.
    pub truncated: bool,
}

/// Runs a project walk off the async runtime, once `root` is known to be a
/// folder: walking and reading thousands of files must not stall the other
/// commands.
async fn load_from(root: String, walk: fn(&Path) -> LoadedFiles) -> Result<LoadedFiles, String> {
    let root = PathBuf::from(root);
    if !root.is_dir() {
        return Err(format!("{}: not a folder", root.display()));
    }
    tauri::async_runtime::spawn_blocking(move || walk(&root))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A fresh temporary folder with these files (relative path, content).
    pub fn temp_project(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (path, content) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir
    }

    /// Loaded paths relative to `dir`, forward slashes, sorted.
    pub fn relative_paths(dir: &Path, files: &super::LoadedFiles) -> Vec<String> {
        let mut out: Vec<String> = files
            .files
            .iter()
            .map(|f| {
                Path::new(&f.path)
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        out.sort();
        out
    }
}
