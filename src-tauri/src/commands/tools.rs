//! The agent's file tools as the editor uses them (agent_tools.rs), the
//! Files tab's search (code_search.rs) and source outlines (outline).

use crate::agent_tools::{self, EditOutcome, EditorMarker, ToolDirEntry};
use crate::code_search::{self, SearchResult};
use crate::outline::{self, Symbol};

/// Recursively lists a directory, respecting .gitignore and the built-in
/// denylist.
#[tauri::command]
pub async fn tool_list_dir(
    root: String,
    path: Option<String>,
    max_depth: Option<usize>,
) -> Result<Vec<ToolDirEntry>, String> {
    agent_tools::tool_list_dir(root, path, max_depth).await
}

/// Text or regex search across the project on ripgrep's engine.
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
) -> Result<SearchResult, String> {
    agent_tools::tool_search_code(
        root,
        query,
        path_glob,
        max_results,
        regex,
        case_sensitive,
        context_lines,
    )
    .await
}

/// The editor's current markers for a file (fire-and-forget from the
/// frontend's marker listener).
#[tauri::command]
pub fn report_editor_diagnostics(
    root: String,
    path: String,
    markers: Vec<EditorMarker>,
) -> Result<(), String> {
    agent_tools::report_editor_diagnostics(root, path, markers)
}

/// Reads a text file: a 1-indexed inclusive line range, one declaration by
/// name (`symbol`), or all of it.
#[tauri::command]
pub fn tool_read_file_range(
    root: String,
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    symbol: Option<String>,
) -> Result<String, String> {
    agent_tools::tool_read_file_range(root, path, start_line, end_line, symbol)
}

/// Replaces the one occurrence of `old_text` in a project file with
/// `new_text`. With `dry_run`, computes the result without writing, for the
/// approval review.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn tool_edit_file(
    root: String,
    path: String,
    old_text: String,
    new_text: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
    dry_run: Option<bool>,
) -> Result<EditOutcome, String> {
    agent_tools::tool_edit_file(
        root, path, old_text, new_text, start_line, end_line, dry_run,
    )
}

/// Writes (creating parent directories as needed) or overwrites a file
/// inside the project root.
#[tauri::command]
pub fn tool_write_file(root: String, path: String, content: String) -> Result<String, String> {
    agent_tools::tool_write_file(root, path, content)
}

/// Search for the Files tab: larger result limit, include/exclude globs,
/// whole-word matching.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn search_in_files(
    root: String,
    query: String,
    regex: Option<bool>,
    case_sensitive: Option<bool>,
    whole_word: Option<bool>,
    include: Option<String>,
    exclude: Option<String>,
    max_results: Option<usize>,
) -> Result<SearchResult, String> {
    code_search::search_in_files(
        root,
        query,
        regex,
        case_sensitive,
        whole_word,
        include,
        exclude,
        max_results,
    )
    .await
}

/// The outline of a file's content as the editor has it.
#[tauri::command]
pub fn outline_symbols(path: String, content: String) -> Option<Vec<Symbol>> {
    outline::outline_symbols(path, content)
}
