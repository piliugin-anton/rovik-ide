//! The editor's IntelliSense definitions (rovik_core::typings), as
//! commands. Each runs off the async runtime: walking and reading
//! thousands of files must not stall the other commands.

use rovik_core::typings::{self, ImportSite, LoadedFiles, LoadedTypings, ProjectDefinitions};
use std::path::PathBuf;

async fn off_runtime<T: Send + 'static>(
    root: String,
    load: impl FnOnce(&std::path::Path) -> T + Send + 'static,
) -> Result<T, String> {
    let root = PathBuf::from(root);
    if !root.is_dir() {
        return Err(format!("{}: not a folder", root.display()));
    }
    tauri::async_runtime::spawn_blocking(move || load(&root))
        .await
        .map_err(|e| e.to_string())
}

/// The project's sources and the dependencies they reach: what a project
/// opens with.
#[tauri::command]
pub async fn load_project_definitions(root: String) -> Result<ProjectDefinitions, String> {
    off_runtime(root, typings::load_definitions).await
}

/// The project's sources alone (within limits).
#[tauri::command]
pub async fn load_project_sources(root: String) -> Result<LoadedFiles, String> {
    off_runtime(root, typings::load_sources).await
}

/// The dependencies the project's sources reach, again: after an install.
#[tauri::command]
pub async fn load_project_typings(root: String) -> Result<LoadedTypings, String> {
    off_runtime(root, typings::load_typings).await
}

/// The packages these imports reach that the editor doesn't have yet
/// (`exclude`: the package folders it has).
#[tauri::command]
pub async fn load_dependency_typings(
    root: String,
    sites: Vec<ImportSite>,
    exclude: Vec<String>,
) -> Result<LoadedTypings, String> {
    off_runtime(root, move |_| typings::load_dependencies(&sites, &exclude)).await
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
