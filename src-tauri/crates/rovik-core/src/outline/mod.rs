//! Outlines of source files: the declarations in a file (functions, types,
//! classes, modules, sections, keys…), with their line ranges, nesting and
//! signatures, from a tree-sitter parse.
//!
//! Each language (languages.rs) names the node kinds that are declarations
//! and, where a kind alone can't tell (a JavaScript `const` bound to a
//! function, a C prototype, an Elixir `def` call), adds query patterns that
//! capture them as `@item`. Everything else is the same for every language:
//!
//! - Nesting is containment: a declaration inside another's range is its
//!   child. No language-specific scope rules.
//! - A declaration's range takes in the wrapper it sits in (`export`,
//!   decorators, `template<…>`, a `const` around a declarator) so a read of
//!   the range gets all of it.
//! - The signature is the declaration's text up to its body, on one line.
//!
//! Parsing and querying are cut off after a time limit, and files over a
//! size limit aren't parsed: an outline is a convenience, never a hang.

mod languages;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tree_sitter::{Node, ParseOptions, Parser, QueryCursor, QueryCursorOptions, StreamingIterator};

pub use languages::language_for;

/// Larger files aren't parsed.
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Time allowed for parsing, and again for finding the declarations.
#[cfg(not(test))]
const TIME_LIMIT: Duration = Duration::from_secs(2);
/// Tests check what's found, not the editor's patience: built unoptimized
/// and run a few hundred at once, a big file's parse took a slow CI
/// machine past 2 s, and the outline was none.
#[cfg(test)]
const TIME_LIMIT: Duration = Duration::from_secs(30);
/// More declarations than this aren't collected.
const MAX_SYMBOLS: usize = 50_000;
/// The most characters of a signature.
const SIGNATURE_CHARS: usize = 160;
/// A signature spanning more lines than this is cut to its first.
const SIGNATURE_LINES: usize = 8;
/// The most characters of a name.
const NAME_CHARS: usize = 80;

/// One declaration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Symbol {
    /// What it's called, as best the tree says (a function's name, a
    /// heading's text, a key).
    pub name: String,
    /// The node kind it came from (`function_item`, `class_declaration`…).
    pub kind: String,
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    /// How many declarations it's inside.
    pub depth: usize,
    /// Its text up to its body, on one line: `pub fn read(path: &str) -> String`.
    pub signature: String,
}

/// The outline of `content`, the file at `path`. None when the file isn't
/// in a language with a grammar, is too large, or can't be parsed in time.
/// An empty list: parsed, and no declarations.
pub fn outline(path: &str, content: &str) -> Option<Vec<Symbol>> {
    analyze(path, content, false).map(|a| a.symbols)
}

/// Editor-facing commands (see `src/lib/outlineIntelliSense.ts`).
///
/// The editor holds unsaved text, so these take `content` directly instead
/// of reading from disk: no round-trip, no stale results while typing.
/// Parsing is capped by time and size (see above), so this never hangs the
/// editor on generated or minified files — those return None.
///
/// This covers what tree-sitter can do without a language server:
/// document symbols, same-file hover signatures and same-file go-to-
/// definition. It can't replace typed intelligence (cross-file TS
/// definitions, hover types, completions, diagnostics): that still comes
/// from the TypeScript worker / language servers.
pub fn outline_symbols(path: String, content: String) -> Option<Vec<Symbol>> {
    outline(&path, &content)
}

/// Outlines of files on disk, by absolute path: modified time and size,
/// and the outline then. Cleared whole when it grows past CACHED_FILES.
type FileCache = HashMap<std::path::PathBuf, (std::time::SystemTime, u64, Arc<Vec<Symbol>>)>;
static FILE_CACHE: LazyLock<Mutex<FileCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
const CACHED_FILES: usize = 5_000;

/// Forgets the cached outline for `abs`, if any. Call after writing the
/// file: mtime granularity (and same-size rewrites) can otherwise serve a
/// stale outline, and with it a stale repo map mid-run.
pub fn invalidate_path(abs: &std::path::Path) {
    FILE_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(abs);
}

/// The outline of the file at `abs` (named `rel` in the project), from the
/// cache while the file is unchanged. None as for `outline`, or when the
/// file can't be read.
pub fn outline_of_file(abs: &std::path::Path, rel: &str) -> Option<Arc<Vec<Symbol>>> {
    let meta = std::fs::metadata(abs).ok()?;
    let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let size = meta.len();
    if let Some((m, s, symbols)) = FILE_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(abs)
    {
        if *m == modified && *s == size {
            return Some(symbols.clone());
        }
    }
    language_for(rel, "")?; // cheap: no grammar, no read
                            // Too large to outline: known from its size, without reading it.
    if size > MAX_BYTES as u64 {
        return None;
    }
    let content = std::fs::read_to_string(abs).ok()?;
    let symbols = Arc::new(outline(rel, &content)?);
    let mut cache = FILE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= CACHED_FILES {
        cache.clear();
    }
    cache.insert(abs.to_path_buf(), (modified, size, symbols.clone()));
    Some(symbols)
}

/// A file's declarations, and the names it uses with how often.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tags {
    pub symbols: Vec<Symbol>,
    /// Every identifier in the file (its own declarations' names too).
    pub identifiers: HashMap<String, u32>,
}

/// The outline and the identifiers, from one parse (for the repo map).
pub fn tags(path: &str, content: &str) -> Option<Tags> {
    analyze(path, content, true)
}

/// One syntax problem: an ERROR or MISSING node from the parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    /// 1-based, inclusive.
    pub start_line: usize,
    /// 1-based, in bytes.
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// `syntax error: "fn broken(("` or ``missing ";"``.
    pub message: String,
}

/// Syntax problems collected from one file, at most.
const MAX_SYNTAX_ERRORS: usize = 50;
/// Characters of source shown in a syntax problem.
const SYNTAX_SNIPPET_CHARS: usize = 80;

/// The syntax problems in `content` (the file at `path`): ERROR and MISSING
/// nodes from a tree-sitter parse, outermost only (an ERROR node containing
/// another is one problem, not two). None when the file isn't in a language
/// with a grammar, is too large, or can't be parsed in time — the same
/// limits as `outline`. An empty list: parses clean.
///
/// This is untyped (a missing bracket, not a wrong type): the agent's
/// `diagnostics` tool pairs it with what the editor's language servers
/// report.
pub fn syntax_errors(path: &str, content: &str) -> Option<Vec<SyntaxError>> {
    let lang = language_for(path, content)?;
    if content.len() > MAX_BYTES {
        return None;
    }
    let grammar = lang.grammar();
    let mut parser = Parser::new();
    parser.set_language(&grammar).ok()?;

    let deadline = Instant::now() + TIME_LIMIT;
    let mut parse_progress = |_: &tree_sitter::ParseState| {
        if Instant::now() > deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let bytes = content.as_bytes();
    let tree = parser.parse_with_options(
        &mut |offset, _| {
            if offset < bytes.len() {
                &bytes[offset..]
            } else {
                &[] as &[u8]
            }
        },
        None,
        Some(ParseOptions::new().progress_callback(&mut parse_progress)),
    )?;
    let root = tree.root_node();
    if !root.has_error() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut visited = 0usize;
    // Nodes whose whole subtree may hold an error; clean subtrees never
    // enter (has_error), and an error node's insides never do either.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visited += 1;
            if visited.is_multiple_of(4096) && Instant::now() > deadline {
                return Some(out);
            }
            if out.len() >= MAX_SYNTAX_ERRORS {
                return Some(out);
            }
            if child.is_error() {
                out.push(error_of(content, &child, false));
            } else if child.is_missing() {
                out.push(error_of(content, &child, true));
            } else if child.has_error() {
                stack.push(child);
            }
        }
    }
    Some(out)
}

fn error_of(content: &str, node: &Node, missing: bool) -> SyntaxError {
    let start = node.start_position();
    let end = node.end_position();
    let message = if missing {
        format!("missing `{}`", node.kind())
    } else {
        let text = &content[node.start_byte()..node.end_byte()];
        let first = text.lines().next().unwrap_or("").trim();
        let snippet = crate::text::shortened(&collapse(first), SYNTAX_SNIPPET_CHARS);
        if snippet.is_empty() {
            "syntax error".to_string()
        } else {
            format!("syntax error: `{snippet}`")
        }
    };
    SyntaxError {
        start_line: start.row + 1,
        start_col: start.column + 1,
        end_line: end.row + 1,
        end_col: end.column + 1,
        message,
    }
}

/// Leaf node kinds that are names, beyond `…identifier`.
const NAME_KINDS: &[&str] = &[
    "constant",
    "word",
    "name",
    "sym_lit",
    "symbol",
    "atom",
    "variable",
    "simple_identifier",
    "type_name",
    "variable_name",
    "field_name",
    "property_name",
    "method_name",
    "function_name",
    "package_name",
];
/// Longer names are noise (generated, minified).
const MAX_IDENTIFIER_CHARS: usize = 80;

fn is_name_kind(kind: &str) -> bool {
    kind.ends_with("identifier") || NAME_KINDS.contains(&kind)
}

fn analyze(path: &str, content: &str, with_identifiers: bool) -> Option<Tags> {
    let lang = language_for(path, content)?;
    if content.len() > MAX_BYTES {
        return None;
    }
    let grammar = lang.grammar();
    let mut parser = Parser::new();
    parser.set_language(&grammar).ok()?;

    let deadline = Instant::now() + TIME_LIMIT;
    let mut parse_progress = |_: &tree_sitter::ParseState| {
        if Instant::now() > deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let bytes = content.as_bytes();
    let tree = parser.parse_with_options(
        &mut |offset, _| {
            if offset < bytes.len() {
                &bytes[offset..]
            } else {
                &[] as &[u8]
            }
        },
        None,
        Some(ParseOptions::new().progress_callback(&mut parse_progress)),
    )?;
    let root = tree.root_node();

    // Declarations by node kind: one walk over the tree.
    let kinds = lang.kind_ids();
    let mut items: Vec<Node> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    // Names a query pattern captured as `@name`, by declaration node.
    let mut names: HashMap<usize, Node> = HashMap::new();
    let mut identifiers: HashMap<String, u32> = HashMap::new();
    let mut cursor = root.walk();
    let mut steps = 0usize;
    'walk: loop {
        let node = cursor.node();
        if kinds.contains(&node.kind_id()) && seen.insert(node.id()) && items.len() < MAX_SYMBOLS {
            items.push(node);
        }
        if with_identifiers
            && node.child_count() == 0
            && node.is_named()
            && is_name_kind(node.kind())
        {
            if let Ok(text) = node.utf8_text(bytes) {
                if (2..=MAX_IDENTIFIER_CHARS).contains(&text.len()) {
                    *identifiers.entry(text.to_string()).or_default() += 1;
                }
            }
        }
        steps += 1;
        if steps.is_multiple_of(4096) && Instant::now() > deadline {
            return None;
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }

    // And by query, for the ones a kind can't tell.
    if let Some(query) = lang.query() {
        // `@item` in our patterns, `@definition.…` in a grammar's tags query.
        let item_indexes: HashSet<u32> = query
            .capture_names()
            .iter()
            .enumerate()
            .filter(|(_, n)| **n == "item" || n.starts_with("definition."))
            .map(|(i, _)| i as u32)
            .collect();
        let name_index = query.capture_index_for_name("name");
        let mut qcursor = QueryCursor::new();
        let mut query_progress = |_: &tree_sitter::QueryCursorState| {
            if Instant::now() > deadline {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let mut matches = qcursor.matches_with_options(
            query,
            root,
            bytes,
            QueryCursorOptions::new().progress_callback(&mut query_progress),
        );
        while let Some(m) = matches.next() {
            let name = m
                .captures()
                .iter()
                .find(|c| Some(c.index) == name_index)
                .map(|c| c.node);
            for c in m.captures() {
                if !item_indexes.contains(&c.index) {
                    continue;
                }
                if seen.insert(c.node.id()) && items.len() < MAX_SYMBOLS {
                    items.push(c.node);
                }
                if let Some(name) = name {
                    names.entry(c.node.id()).or_insert(name);
                }
            }
        }
        if Instant::now() > deadline {
            return None;
        }
    }

    Some(Tags {
        symbols: symbols(content, items, &names),
        identifiers,
    })
}

/// Ranges, signatures and nesting for the declarations found.
fn symbols(content: &str, items: Vec<Node>, names: &HashMap<usize, Node>) -> Vec<Symbol> {
    struct Found<'t> {
        item: Node<'t>,
        range: Node<'t>,
    }
    let mut found: Vec<Found> = items
        .into_iter()
        .map(|item| Found {
            item,
            range: range_node(item),
        })
        .collect();

    // A declaration directly inside another on the same line is the same
    // thing twice (`typedef struct {…} T`): keep the outer one. (Its own
    // wrapper, `export` or `type (…)`, doesn't count.)
    let item_ids: HashSet<usize> = found.iter().map(|f| f.item.id()).collect();
    found.retain(|f| {
        f.item.parent().is_none_or(|p| {
            !(item_ids.contains(&p.id()) && p.start_position().row == f.item.start_position().row)
        })
    });

    // Outer before inner: by start, the longer first.
    found.sort_by(|a, b| {
        (a.range.start_byte(), std::cmp::Reverse(a.range.end_byte()))
            .cmp(&(b.range.start_byte(), std::cmp::Reverse(b.range.end_byte())))
    });
    // The same range twice (two patterns, or a wrapper and its item): once.
    found.dedup_by(|b, a| {
        a.range.start_byte() == b.range.start_byte() && a.range.end_byte() == b.range.end_byte()
    });

    let mut open: Vec<usize> = Vec::new(); // end bytes of the enclosing ones
    let mut out = Vec::with_capacity(found.len());
    for f in found {
        while open.last().is_some_and(|&end| f.range.start_byte() >= end) {
            open.pop();
        }
        let start_line = f.range.start_position().row + 1;
        let end = f.range.end_position();
        // A node ending at the start of a line ends on the line before.
        let end_line = if end.column == 0 && end.row + 1 > start_line {
            end.row
        } else {
            end.row + 1
        };
        out.push(Symbol {
            name: match names.get(&f.item.id()) {
                Some(n) => crate::text::shortened(
                    &collapse(&content[n.start_byte()..n.end_byte()]),
                    NAME_CHARS,
                ),
                None => name(content, f.item),
            },
            kind: f.item.kind().to_string(),
            start_line,
            end_line,
            depth: open.len(),
            signature: signature(content, f.item, f.range),
        });
        open.push(f.range.end_byte());
    }
    out
}

/// Nodes that only wrap a declaration: its range is theirs.
const WRAPPERS: &[&str] = &[
    "export_statement",
    "ambient_declaration",
    "decorated_definition",
    "lexical_declaration",
    "variable_declaration",
    "template_declaration",
    "type_declaration",
    "const_declaration",
    "var_declaration",
    "expression_statement",
    "export_declaration",
    "attributed_statement",
    "annotated_declaration",
];

/// The node whose range a declaration covers: the declaration, or the
/// wrappers it's the whole of (`export`, `@decorator`, `template<…>`).
fn range_node(item: Node) -> Node {
    let mut node = item;
    while let Some(parent) = node.parent() {
        if parent.parent().is_none() || !WRAPPERS.contains(&parent.kind()) {
            break;
        }
        // The wrapper ends where the declaration does, give or take a `;`.
        if parent.end_byte() > node.end_byte() + 1 || parent.start_byte() > node.start_byte() {
            break;
        }
        node = parent;
    }
    node
}

/// Child kinds that are a declaration's body.
fn is_body(node: Node) -> bool {
    let kind = node.kind();
    if kind.contains("code") {
        return false; // Markdown's code blocks are content, not a body
    }
    kind.ends_with("body")
        || kind.ends_with("block")
        || kind == "declaration_list"
        || kind == "field_declaration_list"
        || kind == "compound_statement"
        || kind == "enumerator_list"
        || kind == "statement_block"
        || kind == "block_mapping"
        || kind == "template_body"
        || kind == "do_block"
}

/// The declaration's body, if it has one.
fn body(item: Node) -> Option<Node> {
    if let Some(b) = item.child_by_field_name("body") {
        return Some(b);
    }
    let mut cursor = item.walk();
    let found = item
        .named_children(&mut cursor)
        .find(|c| is_body(*c) && c.start_byte() > item.start_byte());
    found
}

/// The declaration's text up to its body, whitespace collapsed: from the
/// wrapper when it starts on the same line (`export default function…`),
/// else from the declaration itself (not its decorators).
fn signature(content: &str, item: Node, range: Node) -> String {
    let start = if range.start_position().row == item.start_position().row {
        range.start_byte()
    } else {
        item.start_byte()
    };
    let end = match body(item) {
        Some(b) if b.start_byte() > start => b.start_byte(),
        _ => {
            // No body: its first line.
            let text = &content[start..item.end_byte()];
            start + text.find('\n').unwrap_or(text.len())
        }
    };
    // Text up to a body that comes after a whole declarative part (Pascal's
    // `program`, VHDL's `architecture`) isn't a signature: its first line is.
    let end = if content[start..end].matches('\n').count() > SIGNATURE_LINES {
        start + content[start..end].find('\n').unwrap_or(end - start)
    } else {
        end
    };
    let mut text = collapse(&content[start..end]);
    if text.is_empty() {
        let line = &content[start..];
        text = collapse(line.lines().next().unwrap_or(""));
    }
    crate::text::shortened(&text, SIGNATURE_CHARS)
}

/// Field names that hold a declaration's name.
const NAME_FIELDS: &[&str] = &[
    "name",
    "declarator",
    "key",
    "tag",
    "target",
    "left",
    "pattern",
];

/// A declaration's name: its `name` field, or the first name-like node
/// near its start, or its first line.
fn name(content: &str, item: Node) -> String {
    fn find<'t>(node: Node<'t>, depth: usize) -> Option<Node<'t>> {
        for field in NAME_FIELDS {
            if let Some(n) = node.child_by_field_name(field) {
                return Some(innermost(n));
            }
        }
        if depth == 0 {
            return None;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if is_body(child) {
                break;
            }
            let kind = child.kind();
            // `const int size`: the type, modifiers and annotations come
            // before the name but aren't it.
            if kind == "type"
                || kind.ends_with("_type")
                || [
                    "modifier",
                    "annotation",
                    "attribute",
                    "decorator",
                    "comment",
                ]
                .iter()
                .any(|k| kind.contains(k))
            {
                continue;
            }
            if kind.contains("identifier") || kind.ends_with("name") || kind == "word" {
                return Some(child);
            }
            if let Some(n) = find(child, depth - 1) {
                return Some(n);
            }
        }
        None
    }
    /// `*foo`, `foo(int)` → foo: the name inside a declarator.
    fn innermost(node: Node) -> Node {
        let mut node = node;
        while node.kind().ends_with("declarator") {
            match node.child_by_field_name("declarator") {
                Some(inner) => node = inner,
                None => break,
            }
        }
        node
    }
    let text = match find(item, 3) {
        Some(n) => &content[n.start_byte()..n.end_byte()],
        None => content[item.start_byte()..item.end_byte()]
            .lines()
            .next()
            .unwrap_or(""),
    };
    crate::text::shortened(&collapse(text), NAME_CHARS)
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
