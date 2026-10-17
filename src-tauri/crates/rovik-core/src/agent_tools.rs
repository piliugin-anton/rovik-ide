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
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use crate::file_guard::{Seen, WriteError};

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
    // Not `canonicalize`: an isolated chat's view on Windows can't be.
    crate::vfs::canonical(p).map_err(|e| format!("{}: {e}", p.display()))
}

/// `path` with symlinks resolved as far as it exists: its deepest existing
/// ancestor canonicalized, the rest (not created yet) appended. Comparing a
/// canonical root with a raw new path refused paths inside the project:
/// `\\?\C:\proj` vs `C:\proj\new.txt` on Windows, `/private/tmp/p` vs
/// `/tmp/p/new.txt` on macOS. A dangling symlink counts as existing, so it
/// fails here instead of letting a write follow it out of the project.
fn canonicalize_existing(path: &Path) -> Result<PathBuf, String> {
    let mut rest = Vec::new();
    let mut existing = path;
    while existing.symlink_metadata().is_err() {
        let name = existing
            .file_name()
            .ok_or_else(|| format!("{}: not a valid path", path.display()))?;
        rest.push(name);
        existing = existing
            .parent()
            .ok_or_else(|| format!("{}: not a valid path", path.display()))?;
    }
    let mut out = canonicalize_or_err(existing)?;
    out.extend(rest.iter().rev());
    Ok(out)
}

/// `path` (as the model gave it: relative, absolute, "./src", "src\\")
/// relative to the project, with '/' between folders and "" for the project
/// itself. Resolved like the tools resolve it (symlinks, the file system's
/// own case and separators), so it names what they used. None when it
/// isn't inside the project.
pub fn project_relative(root: &str, path: &str) -> Option<String> {
    let target = canonicalize_existing(&resolve_within_root(root, path).ok()?).ok()?;
    let root = canonicalize_or_err(Path::new(root)).ok()?;
    let relative = target.strip_prefix(&root).ok()?;
    Some(
        relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// Resolves `candidate` (as given by the model, relative to the project
/// root unless absolute) and checks it stays inside `root`.
///
/// `..` components are rejected outright rather than only checked after the
/// fact — that keeps this correct for paths that don't exist yet (write
/// targets), where we can't canonicalize to catch a symlink escape anyway.
/// For paths that *do* exist, we additionally canonicalize to catch the case
/// where a path component is itself a symlink pointing outside root.
pub(crate) fn resolve_within_root(root: &str, candidate: &str) -> Result<PathBuf, String> {
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

    let check = canonicalize_existing(&joined)?;
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
pub async fn tool_list_dir(
    root: String,
    path: Option<String>,
    max_depth: Option<usize>,
) -> Result<Vec<ToolDirEntry>, String> {
    // Walking a large tree must not block the main thread.
    crate::rt::spawn_blocking(move || list_dir(root, path, max_depth))
        .await
        .map_err(|e| e.to_string())?
}

fn list_dir(
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
    let globs = crate::code_search::split_globs(&path_glob.unwrap_or_default());
    let opts = crate::code_search::SearchOptions {
        query,
        regex: regex.unwrap_or(false),
        case_sensitive: case_sensitive.unwrap_or(false),
        whole_word: false,
        globs,
        max_results: max_results.unwrap_or(100).clamp(1, 500),
        context_lines: context_lines.unwrap_or(0).min(5),
    };
    crate::rt::spawn_blocking(move || {
        crate::code_search::search(&root_canon, &opts, is_denylisted_name)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The one-hop structural neighbourhood of a file from the repository map's
/// graph (see repo_map.rs): which parsed files it uses by name, and which
/// use it. Runs off the main thread; the walk reuses the map's cache.
pub async fn tool_neighbours(root: String, path: String) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("path must not be empty".to_string());
    }
    // Confine to the project like every other tool: no `..`, and an
    // absolute path must sit under the root (then made relative: the graph
    // keys are project-relative).
    let rel = {
        let root_canon = canonicalize_or_err(Path::new(&root))?;
        let candidate = Path::new(&path);
        if candidate.is_absolute() {
            // Canonicalize when possible: the path may be symlinked (macOS
            // /tmp → /private/tmp) while the root is canonical.
            let canon = canonicalize_or_err(candidate).unwrap_or_else(|_| candidate.to_path_buf());
            canon
                .strip_prefix(&root_canon)
                .map_err(|_| format!("path escapes project root: {path}"))?
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            if candidate
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(format!("path must not contain '..': {path}"));
            }
            path.clone()
        }
    };
    crate::rt::spawn_blocking(move || {
        crate::repo_map::neighbours(Path::new(&root), &rel, &crate::repo_map::Options::default())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Where a symbol is declared and which files use the name, from the
/// repository map's graph (see repo_map.rs). For exact use sites (lines),
/// follow up with search_code. Runs off the main thread.
pub async fn tool_callers(root: String, symbol: String) -> Result<String, String> {
    if symbol.trim().is_empty() {
        return Err("symbol must not be empty".to_string());
    }
    crate::rt::spawn_blocking(move || {
        crate::repo_map::callers(
            Path::new(&root),
            &symbol,
            &crate::repo_map::Options::default(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A file's declarations with line ranges and signatures, without its
/// content: the same tree-sitter outline as read's symbol navigation and
/// the repo map. Cheaper than reading a large file to learn its shape.
pub fn tool_outline(root: String, path: String) -> Result<String, String> {
    let (abs, rel) = resolve_project_file(&root, &path, "outline")?;
    let symbols = match crate::outline::outline_of_file(&abs, &rel) {
        Some(s) => s,
        // `.h`/`.m` need content to pick a language; the cache lookup
        // above only tries the extension.
        None => {
            let meta = fs::metadata(&abs).map_err(|e| format!("{path}: {e}"))?;
            if meta.len() > 8 * 1024 * 1024 {
                return Err(format!(
                    "{path}: too large to outline; read it by start_line/end_line instead."
                ));
            }
            let content = fs::read_to_string(&abs).map_err(|e| format!("{path}: {e}"))?;
            match crate::outline::outline(&rel, &content) {
                Some(symbols) => std::sync::Arc::new(symbols),
                None => {
                    return Err(match crate::outline::language_for(&rel, &content) {
                        None => format!("{path}: no outline (language not known); read it by start_line/end_line instead."),
                        Some(_) => format!("{path}: couldn't be outlined (too large or too slow to parse); read it by start_line/end_line instead."),
                    });
                }
            }
        }
    };
    Ok(render_outline(&rel, &symbols))
}

fn render_outline(rel: &str, symbols: &[crate::outline::Symbol]) -> String {
    if symbols.is_empty() {
        return format!(
            "## Outline of {rel}\n(no declarations found; the file parses but declares nothing)"
        );
    }
    const MAX_CHARS: usize = 8_000;
    let mut out = vec![format!("## Outline of {rel}")];
    let mut len = out[0].len();
    let mut omitted = 0usize;
    for (i, s) in symbols.iter().enumerate() {
        let lines = if s.end_line > s.start_line {
            format!("{}-{}", s.start_line, s.end_line)
        } else {
            s.start_line.to_string()
        };
        let line = format!("{}{lines} {}", "  ".repeat(s.depth + 1), s.signature);
        // Stop at the cap: the rest is left out as a block, never with
        // holes where a long line didn't fit but a later short one did.
        if len + line.len() + 1 > MAX_CHARS {
            omitted = symbols.len() - i;
            break;
        }
        len += line.len() + 1;
        out.push(line);
    }
    if omitted > 0 {
        out.push(format!(
            "[…{omitted} more declarations; read by symbol for the rest]"
        ));
    }
    out.push("Read a declaration with read by symbol.".into());
    out.join("\n")
}

/*
 * ---------------------------------------------------------------------------
 * Diagnostics: what the editor shows, for the agent
 * ---------------------------------------------------------------------------
 *
 * The frontend reports its visible markers (TypeScript worker,
 * rust-analyzer via LSP) per open file; the agent reads them back with
 * `diagnostics`, paired with a fresh tree-sitter syntax check of the file
 * on disk. Backend-run language servers would duplicate the ones the
 * editor already runs, so the bridge goes the other way: editor → backend
 * → agent.
 */

/// One editor marker, as the frontend's monaco reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorMarker {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// "error" | "warning" | "info" | "hint".
    pub severity: String,
    pub message: String,
    /// Which owner set it ("typescript", "rust-analyzer"…).
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

struct DiagReport {
    markers: Vec<EditorMarker>,
    reported_at: SystemTime,
}

/// Editor-reported diagnostics by (canonical root, project-relative path).
static EDITOR_DIAGS: LazyLock<Mutex<std::collections::HashMap<(PathBuf, String), DiagReport>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
/// Reported files kept per project root, at most.
const DIAG_MAX_FILES: usize = 300;
/// Markers kept per file, at most (errors first).
const DIAG_MAX_MARKERS: usize = 100;

/// The editor's current markers for a file (fire-and-forget from the
/// frontend's marker listener). Never fails the caller: bad input is an
/// error string, never a panic.
pub fn report_editor_diagnostics(
    root: String,
    path: String,
    markers: Vec<EditorMarker>,
) -> Result<(), String> {
    let root_canon = canonicalize_or_err(Path::new(&root))?;
    let rel = if Path::new(&path).is_absolute() {
        // Canonicalize when possible: the caller may hand us a symlinked
        // path (macOS /tmp → /private/tmp), while the root is canonical.
        let canon = canonicalize_or_err(Path::new(&path))
            .unwrap_or_else(|_| Path::new(&path).to_path_buf());
        canon
            .strip_prefix(&root_canon)
            .map_err(|_| format!("path escapes project root: {path}"))?
            .to_string_lossy()
            .replace('\\', "/")
    } else {
        if Path::new(&path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("path must not contain '..': {path}"));
        }
        path.trim().replace('\\', "/")
    };
    let mut markers = markers;
    for m in &mut markers {
        m.start_line = m.start_line.max(1);
        m.end_line = m.end_line.max(m.start_line);
        m.start_col = m.start_col.max(1);
        m.end_col = m.end_col.max(1);
        m.severity = m.severity.to_ascii_lowercase();
        if !["error", "warning", "info", "hint"].contains(&m.severity.as_str()) {
            m.severity = "info".to_string();
        }
        // At most 1,000 bytes, cut on a character boundary: a byte index
        // inside a multi-byte character (Cyrillic, CJK) would panic.
        if m.message.len() > 1_000 {
            let mut cut = 1_000;
            while !m.message.is_char_boundary(cut) {
                cut -= 1;
            }
            m.message.truncate(cut);
        }
    }
    // Errors first, then by line: the cap keeps what matters.
    markers.sort_by_key(|m| {
        (
            match m.severity.as_str() {
                "error" => 0,
                "warning" => 1,
                _ => 2,
            },
            m.start_line,
            m.start_col,
        )
    });
    markers.truncate(DIAG_MAX_MARKERS);
    let mut diags = EDITOR_DIAGS.lock().unwrap_or_else(|e| e.into_inner());
    diags.insert(
        (root_canon, rel),
        DiagReport {
            markers,
            reported_at: SystemTime::now(),
        },
    );
    // Evict the oldest reports beyond the cap.
    while diags.len() > DIAG_MAX_FILES {
        let oldest = diags
            .iter()
            .min_by_key(|(_, r)| r.reported_at)
            .map(|(k, _)| k.clone());
        match oldest {
            Some(k) => {
                diags.remove(&k);
            }
            None => break,
        }
    }
    Ok(())
}

/// A project file, resolved and checked: its absolute path and its
/// project-relative path with forward slashes.
fn resolve_project_file(root: &str, path: &str, verb: &str) -> Result<(PathBuf, String), String> {
    if path.trim().is_empty() {
        return Err("path must not be empty".to_string());
    }
    let abs = resolve_within_root(root, path)?;
    if !abs.is_file() {
        return Err(format!("{path}: not a file"));
    }
    if looks_binary(&abs)? {
        return Err(format!("{path}: binary file, cannot {verb}"));
    }
    let root_canon = canonicalize_or_err(Path::new(root))?;
    let rel = abs
        .strip_prefix(&root_canon)
        .unwrap_or(&abs)
        .to_string_lossy()
        .replace('\\', "/");
    Ok((abs, rel))
}

/// Errors and warnings for one file (its editor markers plus a fresh syntax
/// check), or, without a path, a summary of the files with reported
/// problems. Typed cross-file errors need the project's type checker;
/// the result says which command that is.
pub fn tool_diagnostics(root: String, path: Option<String>) -> Result<String, String> {
    let root_canon = canonicalize_or_err(Path::new(&root))?;
    match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => file_diagnostics(&root, &root_canon, &p),
        None => diagnostics_summary(&root, &root_canon),
    }
}

fn file_diagnostics(root: &str, root_canon: &Path, path: &str) -> Result<String, String> {
    let (abs, rel) = resolve_project_file(root, path, "check")?;
    let mut out = vec![format!("## Diagnostics for {rel}")];

    // What the editor shows (possibly newer or older than the file on disk).
    let editor = EDITOR_DIAGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(root_canon.to_path_buf(), rel.clone()))
        .map(|r| (r.markers.clone(), r.reported_at));
    match editor {
        // Reported, and the editor shows nothing for it.
        Some((markers, at)) if markers.is_empty() => {
            let stale = fs::metadata(&abs)
                .ok()
                .and_then(|m| m.modified().ok())
                .is_some_and(|m| m > at);
            out.push(format!(
                "Editor: no problems (reported {}{}).",
                ago(at),
                if stale {
                    ", stale: the file changed since"
                } else {
                    ""
                }
            ));
        }
        Some((markers, at)) => {
            let stale = fs::metadata(&abs)
                .ok()
                .and_then(|m| m.modified().ok())
                .is_some_and(|m| m > at);
            let when = ago(at);
            let mut lines = vec![format!(
                "Editor (reported {when}{}):",
                if stale {
                    ", stale: the file changed since"
                } else {
                    ""
                }
            )];
            for m in markers.iter().take(DIAG_MAX_MARKERS) {
                let at_ = if m.start_line == m.end_line {
                    format!("{}:{}", m.start_line, m.start_col)
                } else {
                    format!(
                        "{}:{}-{}:{}",
                        m.start_line, m.start_col, m.end_line, m.end_col
                    )
                };
                let kind = match m.code.as_deref().filter(|c| !c.is_empty()) {
                    Some(code) => format!(" [{code}]"),
                    None => String::new(),
                };
                let from = m
                    .source
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!(" ({s})"))
                    .unwrap_or_default();
                lines.push(format!(
                    "- {at_} {}: {}{}{}",
                    m.severity,
                    m.message.lines().next().unwrap_or(""),
                    kind,
                    from
                ));
            }
            out.push(lines.join("\n"));
        }
        None => out.push("Editor: no diagnostics reported (the file may not be open).".into()),
    }

    // A fresh syntax check of what's on disk now.
    match fs::read_to_string(&abs) {
        Ok(content) => match crate::outline::syntax_errors(&rel, &content) {
            Some(errors) if !errors.is_empty() => {
                let mut lines = vec!["Syntax (current):".to_string()];
                for e in errors {
                    lines.push(format!("- {}:{}: {}", e.start_line, e.start_col, e.message));
                }
                out.push(lines.join("\n"));
            }
            Some(_) => out.push("Syntax: parses clean.".into()),
            None => out.push("Syntax: not checked (language not known or file too large).".into()),
        },
        Err(e) => out.push(format!("Syntax: couldn't read the file: {e}")),
    }

    if let Some(hint) = typecheck_hint(root) {
        out.push(hint);
    }
    Ok(out.join("\n\n"))
}

/// How long ago a report came in: seconds under a minute, else minutes.
fn ago(at: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(at)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else {
        format!("{}m ago", secs / 60)
    }
}

/// The command for typed errors, from the detected project checks.
fn typecheck_hint(root: &str) -> Option<String> {
    let profile = crate::project_profile::detect(Path::new(root));
    let typed = profile
        .checks
        .iter()
        .find(|c| c.kind == "typecheck")
        .or_else(|| profile.checks.iter().find(|c| c.kind == "lint"))?;
    Some(format!(
        "Typed (cross-file) errors need the type checker: run `{}` with bash. The IDE also runs checks itself at the end when verification is on.",
        typed.command
    ))
}

fn diagnostics_summary(root: &str, root_canon: &Path) -> Result<String, String> {
    let diags = EDITOR_DIAGS.lock().unwrap_or_else(|e| e.into_inner());
    let mut files: Vec<(String, usize, usize, String)> = Vec::new(); // rel, errors, warnings, age/stale
    for ((r, rel), report) in diags.iter() {
        if r != root_canon {
            continue;
        }
        let errors = report
            .markers
            .iter()
            .filter(|m| m.severity == "error")
            .count();
        let warnings = report
            .markers
            .iter()
            .filter(|m| m.severity == "warning")
            .count();
        if errors == 0 && warnings == 0 {
            continue;
        }
        let stale = Path::new(root)
            .join(rel)
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .is_some_and(|m| m > report.reported_at);
        files.push((
            rel.clone(),
            errors,
            warnings,
            format!(
                "{}{}",
                ago(report.reported_at),
                if stale { ", stale" } else { "" }
            ),
        ));
    }
    drop(diags);
    if files.is_empty() {
        return Ok("## Diagnostics summary\n(no editor diagnostics reported for this project yet — open files in the editor, or query one file for its syntax check.)".into());
    }
    files.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
    const MAX_FILES: usize = 20;
    let total = files.len();
    let mut out = vec!["## Diagnostics summary (editor-reported)".to_string()];
    for (rel, errors, warnings, age) in files.into_iter().take(MAX_FILES) {
        let mut parts = Vec::new();
        if errors > 0 {
            parts.push(format!(
                "{} error{}",
                errors,
                if errors == 1 { "" } else { "s" }
            ));
        }
        if warnings > 0 {
            parts.push(format!(
                "{} warning{}",
                warnings,
                if warnings == 1 { "" } else { "s" }
            ));
        }
        out.push(format!("- {rel}: {} ({age})", parts.join(", ")));
    }
    if total > MAX_FILES {
        out.push(format!("(…and {} more files)", total - MAX_FILES));
    }
    out.push(
        "Query one file with diagnostics for details; syntax is checked fresh per file.".into(),
    );
    Ok(out.join("\n"))
}

/// Reads a text file: a 1-indexed inclusive line range, one declaration by
/// name (`symbol`, see read_symbol.rs), or all of it. A file too large to
/// read whole comes back as its start and a map of its declarations, and
/// long ranges stop with a note (see read_file.rs).
pub fn tool_read_file_range(
    root: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    symbol: Option<String>,
) -> Result<String, String> {
    read_file_range(&root, &path, start_line, end_line, symbol, None)
}

/// [`tool_read_file_range`] for a chat's run: the file's content counts as
/// seen by it (file_guard.rs), so it may write the file whole afterwards.
pub fn read_file_range(
    root: &str,
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    symbol: Option<String>,
    seen: Option<&Seen>,
) -> Result<String, String> {
    let (root, path) = (root.to_string(), path.to_string());
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
    if let Some(seen) = seen {
        seen.record(&target, content.as_bytes());
    }
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
pub fn tool_edit_file(
    root: String,
    path: String,
    old_text: String,
    new_text: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    dry_run: Option<bool>,
) -> Result<EditOutcome, String> {
    let edit = Edit {
        old_text: &old_text,
        new_text: &new_text,
        start_line,
        end_line,
    };
    edit_file(&root, &path, &edit, dry_run.unwrap_or(false), None)
}

/// One `old_text` → `new_text` replacement, as the model gave it.
pub struct Edit<'a> {
    pub old_text: &'a str,
    pub new_text: &'a str,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

/// [`tool_edit_file`], and for a chat's run (`seen`): the edit applies to
/// the file as it is when written, under its lock (file_guard.rs), so a
/// change made meanwhile by someone else is kept. The edit's own text
/// still having to be there once is its check.
pub fn edit_file(
    root: &str,
    path: &str,
    edit: &Edit,
    dry_run: bool,
    seen: Option<&Seen>,
) -> Result<EditOutcome, String> {
    let target = resolve_within_root(root, path)?;
    if !target.is_file() {
        return Err(format!(
            "{path}: no such file. edit changes existing files; use write to create one."
        ));
    }
    if looks_binary(&target)? {
        return Err(format!("{path}: binary file, cannot edit"));
    }
    let apply = |current: &str| {
        crate::edit_file::apply_edit(
            current,
            edit.old_text,
            edit.new_text,
            edit.start_line,
            edit.end_line,
        )
        .map_err(|e| format!("{path}: {e}"))
    };
    let (content, span) = if dry_run {
        apply(&fs::read_to_string(&target).map_err(|e| format!("{path}: {e}"))?)?
    } else {
        // In place, like write: keeps the inode (the editor's file id).
        let throwaway;
        let seen = match seen {
            Some(seen) => seen,
            None => {
                throwaway = Seen::new("");
                &throwaway
            }
        };
        crate::file_guard::update_for_chat(&target, seen, apply)?
    };
    Ok(EditOutcome {
        summary: crate::edit_file::describe_edit(&path, &span),
        content,
    })
}

/// Writes (creating parent directories as needed) or overwrites a file
/// inside the project root.
pub fn tool_write_file(root: String, path: String, content: String) -> Result<String, String> {
    write_file(&root, &path, &content, None)
}

/// [`tool_write_file`], and for a chat's run (`seen`): refused when the file
/// changed since the run last saw it, or is another writer's change it never
/// saw (file_guard.rs). The refusal shows the file as it is now, so the
/// model can redo its change on it.
pub fn write_file(
    root: &str,
    path: &str,
    content: &str,
    seen: Option<&Seen>,
) -> Result<String, String> {
    // Reject '..' up front (same rule as resolve_within_root) before we
    // create any directories on disk.
    let target = resolve_within_root(root, path)?;
    // What was there: None for a new file, Some(None) when it isn't text.
    let before = fs::read(&target)
        .ok()
        .map(|bytes| String::from_utf8(bytes).ok());
    if let Some(Some(current)) = &before {
        if let Some(error) = empty_overwrite_error(&path, current, &content) {
            return Err(error);
        }
    }
    match seen {
        Some(seen) => match crate::file_guard::write_for_chat(&target, content.as_bytes(), seen) {
            Ok(()) => {}
            Err(WriteError::Changed(changed)) => {
                return Err(changed_error(path, &target, &changed, seen))
            }
            Err(WriteError::Io(e)) => return Err(format!("{path}: {e}")),
        },
        None => {
            let _lock = crate::file_guard::lock(&target);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::write(&target, content).map_err(|e| format!("{path}: {e}"))?;
        }
    }
    let changes = match &before {
        None => line_changes("", content),
        Some(Some(current)) => line_changes(current, content),
        Some(None) => return Ok(format!("wrote {} bytes to {path}", content.len())),
    };
    Ok(format!(
        "wrote {} bytes to {path} (+{} -{} lines)",
        content.len(),
        changes.0,
        changes.1
    ))
}

/// Lines added and removed turning `before` into `after`, counted as the
/// chat's diffs count them: a changed line is one removed and one added.
pub fn line_changes(before: &str, after: &str) -> (usize, usize) {
    use crate::agent_run::diff::{diff_lines, split_lines};
    fn lines(text: &str) -> Vec<&str> {
        let mut lines = split_lines(text);
        // The newline ending the last line doesn't start another.
        if lines.last() == Some(&"") {
            lines.pop();
        }
        lines
    }
    diff_lines(&lines(before), &lines(after))
        .iter()
        .fold((0, 0), |(added, removed), h| {
            (added + h.added.len(), removed + h.deleted.len())
        })
}

/// The model's error for a whole-file write over a change it hasn't seen:
/// who changed the file, and the file now. When that shows the whole file,
/// it counts as seen, so a corrected write can follow at once.
fn changed_error(
    path: &str,
    target: &Path,
    changed: &crate::file_guard::Changed,
    seen: &Seen,
) -> String {
    let by = crate::file_guard::describe_changer(changed.by.as_ref(), Some(seen.owner()));
    let head = format!(
        "Error: {path} changed since you last read it ({by}); your write was not applied. \
         Redo your change on the file as it is now"
    );
    if crate::merge::is_binary(&changed.current) {
        return format!("{head} (a binary file).");
    }
    let current = String::from_utf8_lossy(&changed.current);
    match crate::read_file::read(path, &current, None, None) {
        Ok(shown) => {
            if !shown.starts_with(crate::read_file::PARTIAL_PREFIX) {
                seen.record(target, &changed.current);
            }
            format!("{head}:\n\n{shown}")
        }
        Err(_) => format!("{head}; read it first."),
    }
}

/// A whole-file write with nothing in it over a file that has content:
/// almost always a call that lost its content, not a wish to empty the
/// file, and it would wipe the file before anything could notice.
pub fn empty_overwrite_error(path: &str, current: &str, content: &str) -> Option<String> {
    if !content.trim().is_empty() || current.trim().is_empty() {
        return None;
    }
    Some(format!(
        "refusing to replace {path} ({} lines) with empty content; the file was not changed. \
         A write without old_text must contain the complete new file. To change part of it, use old_text; \
         to delete the file, run a command or ask the user.",
        current.lines().count()
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A project reached through a symlink, like any project under /tmp on
    /// macOS (/tmp → /private/tmp).
    fn linked_project(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("agent-tools-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        (base, link)
    }

    #[test]
    fn folders_are_named_relative_to_the_project() {
        let (base, root) = linked_project("relative");
        fs::create_dir_all(root.join("src/lib")).unwrap();
        let root_s = root.to_string_lossy().to_string();
        let real = base.join("real").to_string_lossy().to_string();
        let nested = format!("{real}/src/lib");
        for (path, dir) in [
            ("", ""),
            (".", ""),
            ("./", ""),
            (root_s.as_str(), ""),
            (real.as_str(), ""),
            ("src", "src"),
            ("./src/lib/", "src/lib"),
            (nested.as_str(), "src/lib"),
        ] {
            assert_eq!(
                project_relative(&root_s, path).as_deref(),
                Some(dir),
                "{path:?}"
            );
        }
        assert_eq!(project_relative(&root_s, "/etc"), None);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn line_changes_count_like_the_chat() {
        assert_eq!(line_changes("", "a\nb\n"), (2, 0));
        assert_eq!(line_changes("a\nb\nc\n", "a\nB\nc\nd\n"), (2, 1));
        assert_eq!(line_changes("same\n", "same\n"), (0, 0));
        assert_eq!(line_changes("a\r\nb\r\n", "a\nb"), (0, 0));
    }

    #[test]
    fn empty_content_never_overwrites_a_file() {
        let (base, root) = linked_project("empty");
        let root_s = root.to_string_lossy().to_string();
        fs::write(root.join("a.txt"), "keep\nme\n").unwrap();
        let err = tool_write_file(root_s.clone(), "a.txt".into(), "  \n".into()).unwrap_err();
        assert!(err.contains("refusing to replace a.txt (2 lines)"), "{err}");
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "keep\nme\n"
        );
        // Empty files may still be created, or written empty again.
        tool_write_file(root_s.clone(), "pkg/__init__.py".into(), String::new()).unwrap();
        tool_write_file(root_s.clone(), "pkg/__init__.py".into(), String::new()).unwrap();
        // Content is fine whatever was there.
        let wrote = tool_write_file(root_s, "a.txt".into(), "new\n".into()).unwrap();
        assert_eq!(wrote, "wrote 4 bytes to a.txt (+1 -2 lines)");
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "new\n");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn new_files_given_by_absolute_path_stay_inside() {
        let (base, root) = linked_project("abs");
        let root_s = root.to_str().unwrap();
        let new_file = root.join("src").join("new.rs");
        assert_eq!(
            resolve_within_root(root_s, new_file.to_str().unwrap()).unwrap(),
            new_file
        );
        assert!(resolve_within_root(root_s, "src/new.rs").is_ok());
        assert!(resolve_within_root(root_s, base.join("elsewhere.rs").to_str().unwrap()).is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_dangling_symlink_out_of_the_project_is_refused() {
        let (base, root) = linked_project("dangling");
        std::os::unix::fs::symlink(base.join("outside.txt"), root.join("escape.txt")).unwrap();
        assert!(resolve_within_root(root.to_str().unwrap(), "escape.txt").is_err());
        let _ = fs::remove_dir_all(&base);
    }

    fn outline_project(name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf, String) {
        let (base, root) = linked_project(name);
        for (path, content) in files {
            let p = root.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        let root_s = root.to_string_lossy().to_string();
        (base, root, root_s)
    }

    #[test]
    fn outline_lists_declarations_without_content() {
        let (base, _root, root_s) = outline_project(
            "outline",
            &[(
                "src/a.ts",
                "export class Store {\n  append(text: string) {}\n}\n\nexport function load() {}\n",
            )],
        );
        let out = tool_outline(root_s, "src/a.ts".into()).unwrap();
        assert!(out.contains("## Outline of src/a.ts"), "{out}");
        assert!(out.contains("export class Store"), "{out}");
        assert!(out.contains("append(text: string)"), "{out}");
        assert!(out.contains("export function load()"), "{out}");
        assert!(out.contains("read by symbol"), "{out}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn diagnostics_reports_syntax_errors_fresh() {
        let (base, _root, root_s) = outline_project(
            "diags",
            &[
                ("a.rs", "pub fn broken( {\n    1 +\n}\n"),
                ("ok.rs", "pub fn fine() {}\n"),
            ],
        );
        let broken = tool_diagnostics(root_s.clone(), Some("a.rs".into())).unwrap();
        assert!(broken.contains("## Diagnostics for a.rs"), "{broken}");
        assert!(broken.contains("Syntax (current):"), "{broken}");
        let clean = tool_diagnostics(root_s, Some("ok.rs".into())).unwrap();
        assert!(clean.contains("parses clean"), "{clean}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn diagnostics_shows_reported_editor_markers_and_summary() {
        let (base, root, root_s) = outline_project("diagstore", &[("a.ts", "const x = 1;\n")]);
        let abs = root.join("a.ts").to_string_lossy().to_string();
        report_editor_diagnostics(
            root_s.clone(),
            abs,
            vec![EditorMarker {
                start_line: 1,
                start_col: 7,
                end_line: 1,
                end_col: 8,
                severity: "error".into(),
                message: "Cannot find name 'y'.".into(),
                source: Some("typescript".into()),
                code: Some("2304".into()),
            }],
        )
        .unwrap();
        let out = tool_diagnostics(root_s.clone(), Some("a.ts".into())).unwrap();
        assert!(out.contains("Cannot find name 'y'"), "{out}");
        assert!(out.contains("[2304]"), "{out}");
        assert!(out.contains("(typescript)"), "{out}");
        let summary = tool_diagnostics(root_s, None).unwrap();
        assert!(
            summary.contains("a.ts") && summary.contains("1 error"),
            "{summary}"
        );
        let _ = fs::remove_dir_all(&base);
    }

    fn marker(severity: &str, message: String) -> EditorMarker {
        EditorMarker {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 2,
            severity: severity.into(),
            message,
            source: None,
            code: None,
        }
    }

    #[test]
    fn long_multibyte_marker_messages_are_cut_on_a_character() {
        let (base, root, root_s) = outline_project("diagutf8", &[("a.ts", "const x = 1;\n")]);
        let abs = root.join("a.ts").to_string_lossy().to_string();
        // Byte 1,000 falls inside a 2-byte character: cutting there panicked.
        let message = format!("a{}", "ж".repeat(600));
        report_editor_diagnostics(root_s.clone(), abs, vec![marker("error", message)]).unwrap();
        let out = tool_diagnostics(root_s, Some("a.ts".into())).unwrap();
        let line = out
            .lines()
            .find(|l| l.contains("error: a"))
            .expect("the marker");
        let shown = line.split("error: ").nth(1).unwrap();
        assert!(
            shown.len() <= 1_000 && shown.starts_with("aжж"),
            "{} bytes",
            shown.len()
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_clean_reported_file_says_so() {
        let (base, root, root_s) = outline_project("diagclean", &[("a.ts", "const x = 1;\n")]);
        let abs = root.join("a.ts").to_string_lossy().to_string();
        report_editor_diagnostics(root_s.clone(), abs, Vec::new()).unwrap();
        let out = tool_diagnostics(root_s.clone(), Some("a.ts".into())).unwrap();
        assert!(
            out.contains("Editor: no problems (reported 0s ago)"),
            "{out}"
        );
        assert!(!out.contains("may not be open"), "{out}");
        // Never reported: that one may really not be open.
        fs::write(root.join("b.ts"), "const y = 2;\n").unwrap();
        let out = tool_diagnostics(root_s, Some("b.ts".into())).unwrap();
        assert!(out.contains("may not be open"), "{out}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_summary_says_seconds_for_fresh_reports() {
        let (base, root, root_s) = outline_project("diagage", &[("a.ts", "const x = 1;\n")]);
        let abs = root.join("a.ts").to_string_lossy().to_string();
        report_editor_diagnostics(
            root_s.clone(),
            abs,
            vec![marker("warning", "unused".into())],
        )
        .unwrap();
        let summary = tool_diagnostics(root_s, None).unwrap();
        assert!(summary.contains("a.ts: 1 warning (0s ago)"), "{summary}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn graph_and_diagnostics_paths_stay_in_the_project() {
        let (base, root, root_s) =
            outline_project("confine", &[("src/a.rs", "pub fn alpha_one() {}\n")]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // neighbours: no `..`, no absolute path outside the root.
        let err = rt
            .block_on(tool_neighbours(root_s.clone(), "../outside.rs".into()))
            .unwrap_err();
        assert!(err.contains("must not contain '..'"), "{err}");
        let err = rt
            .block_on(tool_neighbours(root_s.clone(), "/etc/passwd".into()))
            .unwrap_err();
        assert!(err.contains("escapes project root"), "{err}");
        // An absolute path through the symlinked root still resolves.
        let inside = root.join("src/a.rs").to_string_lossy().to_string();
        let n = rt
            .block_on(tool_neighbours(root_s.clone(), inside))
            .unwrap();
        assert!(n.contains("## Neighbours of src/a.rs"), "{n}");
        // Editor reports: the same confinement.
        let err =
            report_editor_diagnostics(root_s.clone(), "../x.ts".into(), Vec::new()).unwrap_err();
        assert!(err.contains("must not contain '..'"), "{err}");
        let err = report_editor_diagnostics(root_s, "/etc/hosts".into(), Vec::new()).unwrap_err();
        assert!(err.contains("escapes project root"), "{err}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_long_outline_stops_at_the_cap_without_holes() {
        // Long and short declarations mixed: a skipped long line must not
        // let a later short one in.
        let mut src = String::new();
        for i in 0..400 {
            if i % 2 == 0 {
                src.push_str(&format!("export function f{i}() {{}}\n"));
            } else {
                src.push_str(&format!(
                    "export function f{i}_{}(a: string) {{}}\n",
                    "x".repeat(60)
                ));
            }
        }
        let (base, _root, root_s) = outline_project("outlinecap", &[("big.ts", &src)]);
        let out = tool_outline(root_s, "big.ts".into()).unwrap();
        let shown: Vec<usize> = out
            .lines()
            .filter_map(|l| l.split("function f").nth(1))
            .map(|rest| {
                rest.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap()
            })
            .collect();
        assert!(
            !shown.is_empty() && shown.len() < 400,
            "{} shown",
            shown.len()
        );
        assert_eq!(
            shown,
            (0..shown.len()).collect::<Vec<_>>(),
            "contiguous, no holes"
        );
        assert!(
            out.contains(&format!("[…{} more declarations", 400 - shown.len())),
            "{out}"
        );
        assert!(out.len() <= 8_200, "{} chars", out.len());
        let _ = fs::remove_dir_all(&base);
    }
}
