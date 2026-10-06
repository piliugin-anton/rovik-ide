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

pub(crate) fn is_denylisted_name(name: &str) -> bool {
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
pub async fn tool_list_dir(
    root: String,
    path: Option<String>,
    max_depth: Option<usize>,
) -> Result<Vec<ToolDirEntry>, String> {
    // Walking a large tree must not block the main thread.
    tauri::async_runtime::spawn_blocking(move || list_dir(root, path, max_depth))
        .await
        .map_err(|e| e.to_string())?
}

fn list_dir(root: String, path: Option<String>, max_depth: Option<usize>) -> Result<Vec<ToolDirEntry>, String> {
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
        .require_git(false)
        // Prune denylisted directories instead of walking into them.
        .filter_entry(|e| !is_denylisted_name(&e.file_name().to_string_lossy()))
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


/// Text or regex search across the project on ripgrep's engine (see
/// code_search.rs). Runs off the main thread.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn tool_search_code(
    root: String,
    query: String,
    path_glob: Option<String>,
    max_results: Option<usize>,
    regex: Option<bool>,
    case_sensitive: Option<bool>,
    context_lines: Option<usize>,
) -> Result<crate::code_search::SearchResult, String> {
    if query.trim().is_empty() {
        return Err("query must not be empty".to_string());
    }
    let root_canon = canonicalize_or_err(Path::new(&root))?;
    // Several globs may be given, comma-separated: "*.ts, !*.test.ts".
    let globs = path_glob
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_string)
        .collect();
    let opts = crate::code_search::SearchOptions {
        query,
        regex: regex.unwrap_or(false),
        case_sensitive: case_sensitive.unwrap_or(false),
        whole_word: false,
        globs,
        max_results: max_results.unwrap_or(100).clamp(1, 500),
        context_lines: context_lines.unwrap_or(0).min(5),
    };
    tauri::async_runtime::spawn_blocking(move || {
        crate::code_search::search(&root_canon, &opts, is_denylisted_name)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Reads a text file: a 1-indexed inclusive line range, one declaration by
/// name (`symbol`, see read_symbol.rs), or all of it. A file too large to
/// read whole comes back as its start and a map of its declarations, and
/// long ranges stop with a note (see read_file.rs).
#[tauri::command]
pub fn tool_read_file_range(
    root: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    symbol: Option<String>,
) -> Result<String, String> {
    let symbol = symbol.filter(|s| !s.is_empty());
    if symbol.is_some() && (start_line.is_some() || end_line.is_some()) {
        return Err("give either \"symbol\" or start_line/end_line, not both".into());
    }
    let target = resolve_within_root(&root, &path)?;
    if !target.is_file() {
        return Err(format!("{path}: not a file"));
    }
    if looks_binary(&target)? {
        return Err(format!("{path}: binary file, cannot read"));
    }
    let content = fs::read_to_string(&target).map_err(|e| format!("{path}: {e}"))?;
    match symbol {
        Some(symbol) => crate::read_symbol::read(&path, &content, &symbol),
        None => crate::read_file::read(&path, &content, start_line, end_line),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcome {
    /// The whole file after the edit (diffed for review; open tabs refresh
    /// with it).
    pub(crate) content: String,
    /// The tool result for the model.
    pub(crate) summary: String,
}

/// Replaces the one occurrence of `old_text` in a project file with
/// `new_text`, optionally only within lines `start_line..=end_line`
/// (see edit_file.rs). With `dry_run`, computes the result without writing,
/// for the approval review.
#[tauri::command]
pub fn tool_edit_file(
    root: String,
    path: String,
    old_text: String,
    new_text: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    dry_run: Option<bool>,
) -> Result<EditOutcome, String> {
    let target = resolve_within_root(&root, &path)?;
    if !target.is_file() {
        return Err(format!(
            "{path}: no such file. edit_file changes existing files; use write_file to create one."
        ));
    }
    if looks_binary(&target)? {
        return Err(format!("{path}: binary file, cannot edit"));
    }
    let current = fs::read_to_string(&target).map_err(|e| format!("{path}: {e}"))?;
    let (content, span) =
        crate::edit_file::apply_edit(&current, &old_text, &new_text, start_line, end_line)
            .map_err(|e| format!("{path}: {e}"))?;
    if !dry_run.unwrap_or(false) {
        // Plain write, like write_file: keeps the inode (the editor's file id).
        fs::write(&target, &content).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(EditOutcome {
        summary: crate::edit_file::describe_edit(&path, &span),
        content,
    })
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
