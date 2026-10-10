//! The project's own JavaScript/TypeScript sources, for the editor's
//! IntelliSense. Monaco's TypeScript service only knows files it's given:
//! without these, an import of a file that isn't open in a tab can't be
//! resolved, so go-to-definition, hover types and completions for anything
//! defined elsewhere in the project don't work.
//!
//! Walks the project the way git sees it (.gitignore respected), skipping
//! dependencies and build output, with limits so a huge repository can't
//! flood the editor.

use ignore::WalkBuilder;
use serde::Serialize;
use std::fs;
use std::path::Path;

const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];
/// Folders never worth loading even when not gitignored.
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
const MAX_FILES: usize = 5_000;
/// Bigger files are usually generated or bundled code.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_TOTAL_BYTES: u64 = 40 * 1024 * 1024;

#[derive(Serialize)]
pub struct SourceFile {
    /// Absolute path, as the file tree uses it.
    pub path: String,
    pub content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSources {
    pub files: Vec<SourceFile>,
    /// A limit was hit: some files weren't loaded.
    pub truncated: bool,
}

pub fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e))
}

pub fn collect(root: &Path) -> ProjectSources {
    let mut files = Vec::new();
    let mut total: u64 = 0;
    let mut truncated = false;
    let walker = WalkBuilder::new(root)
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
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) || !is_source_file(path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        if files.len() >= MAX_FILES || total + meta.len() > MAX_TOTAL_BYTES {
            truncated = true;
            break;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        total += meta.len();
        files.push(SourceFile {
            path: path.to_string_lossy().into_owned(),
            content,
        });
    }
    ProjectSources { files, truncated }
}

#[tauri::command]
pub async fn load_project_sources(root: String) -> Result<ProjectSources, String> {
    let root = std::path::PathBuf::from(root);
    if !root.is_dir() {
        return Err(format!("{}: not a folder", root.display()));
    }
    tauri::async_runtime::spawn_blocking(move || collect(&root))
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFiles {
    /// Absolute paths, sorted.
    pub files: Vec<String>,
    pub truncated: bool,
}

/// Every file in the project, for "@" references in the chat: what git
/// would see (.gitignore respected, `.git` skipped), hidden files included
/// unless ignored (so `.rovik/plans/*.md` can be referenced). The file tree
/// only knows folders it has loaded; this doesn't depend on that.
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

    #[test]
    fn loads_sources_and_skips_dependencies_and_ignored_files() {
        let dir = std::env::temp_dir().join(format!("sources-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (p, c) in [
            ("src/a.ts", "export class A {}"),
            ("src/b.jsx", "export const B = 1"),
            ("src/readme.md", "#"),
            ("node_modules/x/index.js", "x"),
            ("dist/bundle.js", "x"),
            ("generated/out.ts", "x"),
            (".gitignore", "generated/\n"),
        ] {
            let path = dir.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, c).unwrap();
        }
        let mut got: Vec<String> = collect(&dir)
            .files
            .iter()
            .map(|f| {
                Path::new(&f.path)
                    .strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        got.sort();
        assert_eq!(got, ["src/a.ts", "src/b.jsx"]);
    }

    #[test]
    fn lists_files_in_subfolders_respecting_gitignore() {
        let dir = std::env::temp_dir().join(format!("files-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (p, c) in [
            ("src/deep/nested/mod.rs", "x"),
            ("src/main.rs", "x"),
            (".rovik/plans/plan.md", "x"),
            ("target/debug/out.bin", "x"),
            (".git/HEAD", "x"),
            ("Cargo.toml", "x"),
            (".gitignore", "target/\n"),
        ] {
            let path = dir.join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, c).unwrap();
        }
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
