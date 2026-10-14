//! Every file in the project, for "@" references in the chat: what git
//! would see (.gitignore respected, `.git` skipped), hidden files included
//! unless ignored. Plans (`.rovik/plans/`) are listed even when ignored:
//! projects often ignore `.rovik/`, and a plan is there to be referenced.
//! The file tree only knows folders it has loaded; this doesn't depend
//! on that.

use ignore::WalkBuilder;
use rovik_core::agent_run::plan::PLANS_DIR;
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
    // Plans: every one, past the limit too; once each if the walk found them.
    files.extend(plan_files(root));
    files.sort();
    files.dedup();
    ProjectFiles { files, truncated }
}

/// Every file in the plans folder, whatever ignores it.
fn plan_files(root: &Path) -> Vec<String> {
    // Joined part by part: the OS's own separators, as the walk names files.
    let mut dir = root.to_path_buf();
    dir.extend(PLANS_DIR.split('/'));
    WalkBuilder::new(dir)
        .standard_filters(false)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect()
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

    /// Listed paths relative to `dir`, forward slashes, in listed order.
    fn relative(dir: &Path, listed: &ProjectFiles) -> Vec<String> {
        listed
            .files
            .iter()
            .map(|f| {
                Path::new(f)
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

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
        assert_eq!(
            relative(&dir, &list_files(&dir, 100)),
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

    #[test]
    fn lists_plans_even_when_rovik_is_ignored() {
        let dir = temp_project(
            "files-plans-test",
            &[
                (".rovik/plans/2026-10-04-add-users.md", "x"),
                (".rovik/plans/old/first.md", "x"),
                (".rovik/project.json", "x"),
                ("src/main.rs", "x"),
                (".gitignore", ".rovik/\n"),
            ],
        );
        assert_eq!(
            relative(&dir, &list_files(&dir, 100)),
            [
                ".gitignore",
                ".rovik/plans/2026-10-04-add-users.md",
                ".rovik/plans/old/first.md",
                "src/main.rs"
            ]
        );
        // The limit cuts the walk, not the plans.
        let cut = list_files(&dir, 1);
        assert!(cut.truncated);
        let plans = relative(&dir, &cut)
            .into_iter()
            .filter(|f| f.starts_with(".rovik/plans/"))
            .count();
        assert_eq!(plans, 2);
    }
}
