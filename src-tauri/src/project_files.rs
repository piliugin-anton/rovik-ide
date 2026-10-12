//! Every file in the project, for "@" references in the chat: what git
//! would see (.gitignore respected, `.git` skipped), hidden files included
//! unless ignored (so `.rovik/plans/*.md` can be referenced). The file tree
//! only knows folders it has loaded; this doesn't depend on that.

use ignore::WalkBuilder;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFiles {
    /// Absolute paths, sorted.
    pub files: Vec<String>,
    pub truncated: bool,
}

pub fn list_files(root: &Path, limit: usize) -> ProjectFiles {
    let mut files = Vec::new();
    let mut truncated = false;
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if files.len() >= limit {
            truncated = true;
            break;
        }
        files.push(entry.path().to_string_lossy().into_owned());
    }
    files.sort();
    ProjectFiles { files, truncated }
}

#[tauri::command]
pub async fn list_project_files(
    root: String,
    limit: Option<usize>,
) -> Result<ProjectFiles, String> {
    let root = std::path::PathBuf::from(root);
    if !root.is_dir() {
        return Err(format!("{}: not a folder", root.display()));
    }
    let limit = limit.unwrap_or(20_000);
    tauri::async_runtime::spawn_blocking(move || list_files(&root, limit))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typings::test_support::temp_project;

    #[test]
    fn lists_files_in_subfolders_respecting_gitignore() {
        let dir = temp_project(
            "files-test",
            &[
                ("src/deep/nested/mod.rs", "x"),
                ("src/main.rs", "x"),
                (".rovik/plans/plan.md", "x"),
                ("target/debug/out.bin", "x"),
                (".git/HEAD", "x"),
                ("Cargo.toml", "x"),
                (".gitignore", "target/\n"),
            ],
        );
        let got: Vec<String> = list_files(&dir, 100)
            .files
            .iter()
            .map(|f| {
                Path::new(f)
                    .strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            got,
            [
                ".gitignore",
                ".rovik/plans/plan.md",
                "Cargo.toml",
                "src/deep/nested/mod.rs",
                "src/main.rs"
            ]
        );
        assert!(list_files(&dir, 2).truncated);
    }
}
