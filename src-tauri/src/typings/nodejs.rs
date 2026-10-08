use std::fs;
use std::path::PathBuf;
use walkdir::WalkDir;

#[derive(serde::Serialize)]
pub struct MonacoLibEntry {
    // The relative virtual path Monaco should use, e.g., "file:///node_modules/solid-js/types/index.d.ts"
    name: String,
    // The raw string content of the d.ts or package.json file
    content: String,
}

#[tauri::command]
pub async fn load_nodejs_project_typings(
    project_root: String,
) -> Result<Vec<MonacoLibEntry>, String> {
    let mut node_modules_path = PathBuf::from(project_root);
    node_modules_path.push("node_modules");

    // Fast return: If there is no node_modules, exit early with an empty list.
    // This prevents errors and keeps app loading instantaneous.
    if !node_modules_path.exists() || !node_modules_path.is_dir() {
        return Ok(Vec::new());
    }

    let mut libs = Vec::new();

    // Optimize WalkDir parameters:
    // - max_depth handles performance ceilings in highly chaotic workspaces
    // - We dynamically skip unneeded compiler files, builds, binary assets, and markdown/images
    let mut visited = std::collections::HashSet::new();
    let walker = WalkDir::new(&node_modules_path)
        .follow_links(true) // Following symlinks in node_modules can introduce infinite loops
        .max_depth(40)
        .into_iter()
        .filter_entry(move |e| {
            // Detect symlinks and dedupe by their resolved target, so a symlink
            // cycle (or the same target linked from multiple places, common with
            // pnpm/workspaces) is only ever descended into once.
            if e.path_is_symlink() {
                match fs::canonicalize(e.path()) {
                    Ok(real_path) => {
                        if !visited.insert(real_path) {
                            return false; // already walked this real target — skip
                        }
                    }
                    Err(_) => return false, // broken symlink — skip
                }
            }

            filter_entry(e.file_name(), e.file_type().is_dir())
        });

    for entry in walker.filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let full_path = entry.path();

            // Extract the virtual directory structure relative to the project's root node_modules
            if let Ok(relative_path) = full_path.strip_prefix(&node_modules_path) {
                if let Ok(content) = fs::read_to_string(full_path) {
                    // Normalize the path separation rules for Monaco's virtual URL scheme
                    let normalized_path = relative_path.to_string_lossy().replace('\\', "/");

                    libs.push(MonacoLibEntry {
                        name: format!("file:///node_modules/{}", normalized_path),
                        content,
                    });
                }
            }
        }
    }

    Ok(libs)
}

/// Whether a directory entry inside `node_modules` is worth descending into
/// (for directories) or collecting (for files: `*.d.ts` and `package.json`,
/// which the TS worker needs to resolve bare imports via the `types` field).
///
/// pnpm's content store (`.pnpm`), its set-aside copies (`.ignored`) and the
/// store dir (`.store`) never participate in Node resolution — the top-level
/// links (e.g. `node_modules/solid-js` → `.pnpm/...`) are what TypeScript
/// walks up to. Collecting the store too duplicates every package (it was
/// roughly a third of a 32MB postMessage to the TS worker) under paths like
/// `node_modules/.pnpm/...` that resolve nothing.
fn filter_entry(file_name: &std::ffi::OsStr, is_dir: bool) -> bool {
    let file_name = file_name.to_string_lossy();
    if is_dir {
        return file_name != ".bin"
            && file_name != ".cache"
            && file_name != ".pnpm"
            && file_name != ".ignored"
            && file_name != ".store"
            && file_name != "test"
            && file_name != "tests"
            && file_name != "__tests__"
            && file_name != "coverage"
            && file_name != "docs"
            && file_name != "examples"
            && file_name != ".github";
    }
    file_name.ends_with(".d.ts") || file_name == "package.json"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn keeps_typings_and_package_json_but_skips_pnpm_store_and_test_dirs() {
        // Directories: pnpm's store dirs are pruned, everything else descends.
        for dir in [".pnpm", ".ignored", ".store", "test", "node_modules", "solid-js"] {
            assert_eq!(
                filter_entry(OsStr::new(dir), true),
                ![".pnpm", ".ignored", ".store", "test"].contains(&dir),
                "dir {dir}"
            );
        }
        // Files: only declarations and package manifests are collected.
        assert!(filter_entry(OsStr::new("index.d.ts"), false));
        assert!(filter_entry(OsStr::new("package.json"), false));
        assert!(!filter_entry(OsStr::new("index.js"), false));
        assert!(!filter_entry(OsStr::new("README.md"), false));
    }
}
