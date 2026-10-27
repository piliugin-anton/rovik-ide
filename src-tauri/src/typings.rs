//! The editor's IntelliSense definitions (rovik_core::typings), as
//! commands. Each runs off the async runtime (util::blocking_in_folders):
//! walking and reading thousands of files must not stall the other commands.
//! A workspace's folders are loaded as one project, within one set of limits
//! (C06).

use crate::util::{blocking_in, blocking_in_folders};
use rovik_core::typings::{self, ImportSite, LoadedFiles, LoadedTypings, ProjectDefinitions};

/// The project's sources and the dependencies they reach: what a project
/// opens with. `roots` are its folders: a project's one, a workspace's
/// several.
#[tauri::command]
pub async fn load_project_definitions(roots: Vec<String>) -> Result<ProjectDefinitions, String> {
    blocking_in_folders(roots, typings::load_definitions).await
}

/// The project's sources alone (within limits).
#[tauri::command]
pub async fn load_project_sources(roots: Vec<String>) -> Result<LoadedFiles, String> {
    blocking_in_folders(roots, typings::load_sources).await
}

/// The dependencies the project's sources reach, again: after an install.
#[tauri::command]
pub async fn load_project_typings(roots: Vec<String>) -> Result<LoadedTypings, String> {
    blocking_in_folders(roots, typings::load_typings).await
}

/// The packages these imports reach that the editor doesn't have yet
/// (`exclude`: the package folders it has).
#[tauri::command]
pub async fn load_dependency_typings(
    root: String,
    sites: Vec<ImportSite>,
    exclude: Vec<String>,
) -> Result<LoadedTypings, String> {
    blocking_in(root, move |_| typings::load_dependencies(&sites, &exclude)).await
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::PathBuf;

    /// A fresh temporary folder with these files (relative path, content).
    pub fn temp_project(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (path, content) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir
    }
}
