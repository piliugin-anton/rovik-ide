// src-tauri/src/agent_tools.rs — new file
//
// File-system tools exposed to the AI agent (as opposed to `commands.rs`,
// which backs the editor UI directly). Three things every tool here does:
//
//   1. Confines paths to the project root the user opened (`resolve_within_root`).
//      A model-supplied path like "../../.ssh/id_rsa" must not escape it.
//   2. Skips ignored/heavy/sensitive files. `ignore::WalkBuilder` handles
//      .gitignore, global gitignore, .git/info/exclude, and dotfiles (all
//      hidden by default) for us. On top of that we apply a small built-in
//      denylist (`is_denylisted_name`) that applies *regardless* of whether
//      a .gitignore exists — a repo with no .gitignore yet shouldn't leak
//      node_modules or a stray .pem into the model's context.
//   3. Returns errors as `Result<_, String>` rather than panicking, since
//      these run inside a model-driven loop that should degrade to an error
//      string the model can see and react to, not crash the app.

use ignore::WalkBuilder;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const DENYLIST_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".venv",
    ".next",
    ".nuxt",
    "coverage",
];
const DENYLIST_EXTENSIONS: &[&str] = &["pem", "key", "pfx", "p12"];

fn is_denylisted_name(name: &str) -> bool {
    if DENYLIST_DIRS.contains(&name) {
        return true;
    }
    if let Some(ext) = name.rsplit('.').next() {
        if DENYLIST_EXTENSIONS.contains(&ext) {
            return true;
        }
    }
    false
}

fn canonicalize_or_err(p: &Path) -> Result<PathBuf, String> {
    p.canonicalize()
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// Resolves `candidate` (as given by the model, relative to the project
/// root unless absolute) and checks it stays inside `root`.
///
/// `..` components are rejected outright rather than only checked after the
/// fact — that keeps this correct for paths that don't exist yet (write
/// targets), where we can't canonicalize to catch a symlink escape anyway.
/// For paths that *do* exist, we additionally canonicalize to catch the case
/// where a path component is itself a symlink pointing outside root.
fn resolve_within_root(root: &str, candidate: &str) -> Result<PathBuf, String> {
    let root_canon = canonicalize_or_err(Path::new(root))?;
    let candidate_path = Path::new(candidate);

    if candidate_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("path must not contain '..': {candidate}"));
    }

    let joined = if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        root_canon.join(candidate_path)
    };

    let check = if joined.exists() {
        canonicalize_or_err(&joined)?
    } else {
        joined.clone()
    };

    if !check.starts_with(&root_canon) {
        return Err(format!("path escapes project root: {candidate}"));
    }

    Ok(joined)
}

fn looks_binary(path: &Path) -> Result<bool, String> {
    use std::io::Read;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 8000];
    let n = file.read(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf[..n].contains(&0))
}

/// Minimal glob: only leading/trailing `*` wildcards. Good enough for
/// "*.ts" / "src/*" style filters from the model without pulling in a globbing
/// crate for one feature.
fn simple_glob_match(glob: &str, path: &str) -> bool {
    match (glob.starts_with('*'), glob.ends_with('*')) {
        (true, true) if glob.len() >= 2 => path.contains(&glob[1..glob.len() - 1]),
        (true, _) => path.ends_with(&glob[1..]),
        (false, true) => path.starts_with(&glob[..glob.len() - 1]),
        (false, false) => path.contains(glob),
    }
}

#[derive(Serialize)]
pub struct ToolDirEntry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
}

/// Recursively lists a directory, respecting .gitignore and the built-in
/// denylist. Capped on both depth and total entries so a huge or
/// gitignore-less project can't produce an unbounded response.
#[tauri::command]
pub fn tool_list_dir(
    root: String,
    path: Option<String>,
    max_depth: Option<usize>,
) -> Result<Vec<ToolDirEntry>, String> {
    let target_rel = path.unwrap_or_default();
    let target = resolve_within_root(&root, &target_rel)?;
    if !target.is_dir() {
        return Err(format!("{target_rel}: not a directory"));
    }
    let root_canon = canonicalize_or_err(Path::new(&root))?;
    let depth = max_depth.unwrap_or(3).clamp(1, 6);

    let walker = WalkBuilder::new(&target)
        .max_depth(Some(depth))
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    let mut out = Vec::new();
    let mut visited = 0usize;

    for entry in walker {
        visited += 1;
        if visited > 20_000 || out.len() >= 2000 {
            break;
        }
        let Ok(entry) = entry else { continue };
        let p = entry.path();
        if p == target {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_denylisted_name(&name) {
            continue;
        }
        let rel_depth = p
            .strip_prefix(&target)
            .map(|r| r.components().count())
            .unwrap_or(1);
        out.push(ToolDirEntry {
            path: p
                .strip_prefix(&root_canon)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string(),
            name,
            is_dir,
            depth: rel_depth,
        });
    }

    Ok(out)
}

#[derive(Serialize)]
pub struct SearchMatch {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Case-insensitive substring search across project files (not a full
/// regex engine — kept intentionally simple). Skips ignored files, binaries,
/// and anything over ~2MB.
#[tauri::command]
pub fn tool_search_code(
    root: String,
    query: String,
    path_glob: Option<String>,
    max_results: Option<usize>,
) -> Result<Vec<SearchMatch>, String> {
    if query.trim().is_empty() {
        return Err("query must not be empty".to_string());
    }
    let root_canon = canonicalize_or_err(Path::new(&root))?;
    let cap = max_results.unwrap_or(100).clamp(1, 500);
    let needle = query.to_lowercase();

    let walker = WalkBuilder::new(&root_canon)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    let mut results = Vec::new();
    let mut visited = 0usize;

    for entry in walker {
        if results.len() >= cap {
            break;
        }
        visited += 1;
        if visited > 50_000 {
            break;
        }

        let Ok(entry) = entry else { continue };
        let p = entry.path();
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }

        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if is_denylisted_name(&name) {
            continue;
        }

        if let Some(glob) = &path_glob {
            if !simple_glob_match(glob, &p.to_string_lossy()) {
                continue;
            }
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > 2_000_000 {
            continue;
        }

        let Ok(content) = fs::read_to_string(p) else {
            continue;
        };
        let rel_path = p
            .strip_prefix(&root_canon)
            .unwrap_or(p)
            .to_string_lossy()
            .to_string();

        for (i, line) in content.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                results.push(SearchMatch {
                    path: rel_path.clone(),
                    line: i + 1,
                    text: line.trim().chars().take(300).collect(),
                });
                if results.len() >= cap {
                    break;
                }
            }
        }
    }

    Ok(results)
}

/// Reads a text file, optionally a 1-indexed inclusive line range. Full-file
/// reads are truncated past ~20k characters — the model should ask for a
/// range on large files rather than pulling the whole thing into context.
#[tauri::command]
pub fn tool_read_file_range(
    root: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    let target = resolve_within_root(&root, &path)?;
    if !target.is_file() {
        return Err(format!("{path}: not a file"));
    }
    if looks_binary(&target)? {
        return Err(format!("{path}: binary file, cannot read"));
    }
    let content = fs::read_to_string(&target).map_err(|e| format!("{path}: {e}"))?;

    if start_line.is_none() && end_line.is_none() {
        const MAX_CHARS: usize = 20_000;
        if content.chars().count() > MAX_CHARS {
            let truncated: String = content.chars().take(MAX_CHARS).collect();
            let remaining = content.chars().count() - MAX_CHARS;
            return Ok(format!(
                "{truncated}\n\n[...truncated, {remaining} more characters — request a line range to see more]"
            ));
        }
        return Ok(content);
    }

    let lines: Vec<&str> = content.lines().collect();
    let start = start_line.unwrap_or(1).max(1);
    let end = end_line.unwrap_or(lines.len());
    let start_idx = start - 1;
    if start_idx >= lines.len() {
        return Ok(String::new());
    }
    let end_idx = end.min(lines.len());
    Ok(lines[start_idx..end_idx].join("\n"))
}

/// Writes (creating parent directories as needed) or overwrites a file
/// inside the project root.
#[tauri::command]
pub fn tool_write_file(root: String, path: String, content: String) -> Result<String, String> {
    // Reject '..' up front (same rule as resolve_within_root) before we
    // create any directories on disk.
    let target = resolve_within_root(&root, &path)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&target, &content).map_err(|e| format!("{path}: {e}"))?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}
