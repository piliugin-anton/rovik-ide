//! What the agent's `read_file` tool returns for a file's text.
//!
//! A small file comes back whole. A large one used to be cut off after its
//! first 20,000 characters, so the model never learned what came after.
//! Now it gets the file's first lines and a map of where its declarations
//! are, by line number, so its next read can be the range it needs. A range
//! is capped the same way and says where to continue.
//!
//! A result showing less than was asked for starts with [`PARTIAL_PREFIX`],
//! so the history (chat_db/history.rs) doesn't take it for a full copy of
//! the file.
//!
//! The map is a stopgap until there's a real symbol index: a line counts as
//! a declaration when it starts with a declaring keyword (`fn`, `class`,
//! `def`, …) after any modifiers (`pub`, `export`, `async`, …), or is a
//! Markdown heading. It misses declarations that start with a type (C and
//! Java functions), which is fine for a map.
//!
//! Pure string work, tested below.

/// The most characters one read returns.
pub const MAX_CHARS: usize = 20_000;
/// Results showing less than was asked for start with this.
pub const PARTIAL_PREFIX: &str = "[Partial read: ";

/// The start of a large file, shown above its map.
const HEAD_LINES: usize = 60;
const HEAD_CHARS: usize = 6_000;
/// Room for the map, and the most characters of a line it shows.
const MAP_CHARS: usize = 8_000;
const MAP_LINE_CHARS: usize = 120;
/// Columns a tab counts as when measuring nesting.
const TAB_WIDTH: usize = 4;
/// Deeper nesting is shown at this indent in the map.
const MAP_MAX_INDENT: usize = 16;

/// Extensions the declaration keywords make sense for.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "py", "pyi", "go", "java", "kt", "kts", "swift", "rb",
    "php", "cs", "scala", "c", "h", "cc", "cpp", "hpp", "m", "mm", "lua", "ex", "exs", "dart", "vue", "svelte",
];
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdx"];

/// Words that start a declaration.
const KEYWORDS: &[&str] = &[
    "fn", "function", "class", "struct", "enum", "trait", "impl", "interface", "type", "mod", "module",
    "namespace", "def", "func", "fun", "union", "macro_rules",
];
/// Words that declare a variable: a declaration only at the top level,
/// otherwise a modifier (`const fn`, `static async foo()`).
const VARIABLES: &[&str] = &["const", "let", "var", "static"];
/// Words that may come before a keyword.
const MODIFIERS: &[&str] = &[
    "pub", "export", "default", "async", "public", "private", "protected", "internal", "abstract", "final",
    "sealed", "unsafe", "extern", "declare", "override", "readonly", "open", "inline", "virtual",
];

/// The text `read_file` returns for `content`, the file at `path`: all of
/// it, or lines `start_line..=end_line` (1-based) when either is given.
pub fn read(path: &str, content: &str, start_line: Option<usize>, end_line: Option<usize>) -> Result<String, String> {
    let lines: Vec<&str> = content.lines().collect();
    if start_line.is_none() && end_line.is_none() {
        let chars = content.chars().count();
        if chars <= MAX_CHARS {
            return Ok(content.to_string());
        }
        return Ok(overview(path, &lines, chars));
    }
    if lines.is_empty() {
        return Ok(String::new());
    }
    let start = start_line.unwrap_or(1).max(1);
    if start > lines.len() {
        return Err(format!("{path} has {} lines; start_line {start} is past the end", lines.len()));
    }
    let end = end_line.unwrap_or(lines.len());
    if end < start {
        return Err(format!("start_line ({start}) is after end_line ({end})"));
    }
    Ok(range(&lines, start, end.min(lines.len())))
}

/// Lines `start..=end`, stopping before the read gets too big.
fn range(lines: &[&str], start: usize, end: usize) -> String {
    let mut shown: Vec<&str> = Vec::new();
    let mut used = 0;
    for n in start..=end {
        let line = lines[n - 1];
        let len = line.chars().count() + 1;
        if used + len > MAX_CHARS {
            if shown.is_empty() {
                // One line longer than a whole read: its start is all there is.
                let more = if n < end { format!(" Continue with start_line={}.", n + 1) } else { String::new() };
                return format!(
                    "{PARTIAL_PREFIX}line {n} is {} characters long; shown: its first {MAX_CHARS}.{more}]\n{}",
                    len - 1,
                    cut(line, MAX_CHARS)
                );
            }
            return format!(
                "{PARTIAL_PREFIX}lines {start}-{} of {} shown: lines {start}-{end} are over ~{MAX_CHARS} characters. Continue with start_line={n}.]\n{}",
                n - 1,
                lines.len(),
                shown.join("\n")
            );
        }
        used += len;
        shown.push(line);
    }
    shown.join("\n")
}

/// A file too large to read whole: its first lines and its map.
fn overview(path: &str, lines: &[&str], chars: usize) -> String {
    let (head, last) = head(lines);
    let map = map(path, lines);
    let mut out = format!(
        "{PARTIAL_PREFIX}{path} has {} lines ({chars} characters), too large to read whole. Shown: lines 1-{last}{}. Read the part you need with start_line and end_line (up to ~{MAX_CHARS} characters per read).]\n{head}",
        lines.len(),
        if map.is_some() { ", then a map of its declarations by line number" } else { "" },
    );
    match map {
        Some(map) => out.push_str(&format!("\n\n[Map of {path} (line: declaration)]\n{map}")),
        None => out.push_str("\n\n[No map for this kind of file: read it in ranges.]"),
    }
    out
}

/// The file's first lines, and the number of the last one shown.
fn head(lines: &[&str]) -> (String, usize) {
    let mut shown: Vec<String> = Vec::new();
    let mut used = 0;
    for line in lines.iter().take(HEAD_LINES) {
        let len = line.chars().count() + 1;
        if used + len > HEAD_CHARS {
            if shown.is_empty() {
                shown.push(format!("{}…", cut(line, HEAD_CHARS)));
            }
            break;
        }
        used += len;
        shown.push(line.to_string());
    }
    let last = shown.len();
    (shown.join("\n"), last)
}

struct Decl {
    line: usize,
    /// Nesting: indent columns in code, heading level in Markdown.
    depth: usize,
    text: String,
}

/// Where the file's declarations are, one per line, within MAP_CHARS: the
/// most deeply nested ones are left out first. None when the file isn't a
/// kind the map knows or has no declarations.
fn map(path: &str, lines: &[&str]) -> Option<String> {
    let ext = extension(path);
    let markdown = MARKDOWN_EXTENSIONS.contains(&ext.as_str());
    if !markdown && !CODE_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }

    let mut decls = Vec::new();
    let mut in_fence = false;
    for (i, line) in lines.iter().enumerate() {
        let text = line.trim_start();
        if markdown {
            if text.starts_with("```") || text.starts_with("~~~") {
                in_fence = !in_fence;
            } else if let Some(level) = heading_level(text).filter(|_| !in_fence) {
                decls.push(Decl { line: i + 1, depth: (level - 1) * 2, text: text.trim_end().to_string() });
            }
        } else {
            let indent = width(&line[..line.len() - text.len()]);
            if is_declaration(text, indent) {
                decls.push(Decl { line: i + 1, depth: indent, text: text.trim_end().to_string() });
            }
        }
    }
    if decls.is_empty() {
        return None;
    }

    let rendered: Vec<String> = decls
        .iter()
        .map(|d| format!("{}: {}{}", d.line, " ".repeat(d.depth.min(MAP_MAX_INDENT)), shorten(&d.text)))
        .collect();
    let size = |max_depth: usize| -> usize {
        decls.iter().zip(&rendered).filter(|(d, _)| d.depth <= max_depth).map(|(_, r)| r.chars().count() + 1).sum()
    };
    let mut depths: Vec<usize> = decls.iter().map(|d| d.depth).collect();
    depths.sort_unstable();
    depths.dedup();
    // The deepest nesting that fits; if not even the top level fits, the
    // top level up to the limit.
    let max_depth = depths.iter().rev().copied().find(|&d| size(d) <= MAP_CHARS).unwrap_or(depths[0]);

    let mut out: Vec<&str> = Vec::new();
    let mut used = 0;
    for (d, r) in decls.iter().zip(&rendered) {
        if d.depth > max_depth {
            continue;
        }
        let len = r.chars().count() + 1;
        if used + len > MAP_CHARS {
            break;
        }
        used += len;
        out.push(r);
    }
    let left_out = decls.len() - out.len();
    let mut map = out.join("\n");
    if left_out > 0 {
        map.push_str(&format!("\n[+{left_out} more declarations, nested deeper or further down: read a range to see them.]"));
    }
    Some(map)
}

/// Whether a line (without its indentation) starts a declaration.
fn is_declaration(text: &str, indent: usize) -> bool {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        if token.starts_with("pub(") {
            continue; // pub(crate), pub(super)
        }
        let word: &str = &token[..token.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(token.len())];
        // `impl<T>`, `function*` and `macro_rules!` are keywords; `type:`
        // (an object key) and `module.exports` aren't. A keyword names
        // something next: `const mod = …` declares a variable called mod.
        let rest = &token[word.len()..];
        let named = if rest.is_empty() {
            tokens.get(i + 1).is_some_and(|next| !next.starts_with(['=', ':']))
        } else {
            rest.starts_with(['<', '(', '!', '*'])
        };
        if KEYWORDS.contains(&word) && named {
            // `type` is also an import specifier (`import { type A }`).
            return word != "type" || text.contains(['=', '{']);
        }
        if VARIABLES.contains(&word) && rest.is_empty() {
            if indent == 0 {
                return true;
            }
            continue;
        }
        if !MODIFIERS.contains(&word) || word.len() != token.len() {
            return false;
        }
    }
    false
}

/// `## Title` → 2.
fn heading_level(text: &str) -> Option<usize> {
    let level = text.chars().take_while(|&c| c == '#').count();
    let rest = &text[level..];
    ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' '))).then_some(level)
}

fn width(indent: &str) -> usize {
    indent.chars().map(|c| if c == '\t' { TAB_WIDTH } else { 1 }).sum()
}

fn extension(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// The first `n` characters of `s`.
fn cut(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn shorten(text: &str) -> String {
    if text.chars().count() > MAP_LINE_CHARS {
        format!("{}…", cut(text, MAP_LINE_CHARS))
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Rust file of `n` functions, each 30 lines long.
    fn big_rust(n: usize) -> String {
        let mut s = String::from("use std::fs;\n\n");
        for i in 0..n {
            s.push_str(&format!("pub fn f{i}(x: u32) -> u32 {{\n"));
            for _ in 0..28 {
                s.push_str("    let y = x + 1; // padding padding padding\n");
            }
            s.push_str("}\n");
        }
        s
    }

    #[test]
    fn a_small_file_comes_back_whole() {
        let content = "fn main() {}\n";
        assert_eq!(read("src/main.rs", content, None, None).unwrap(), content);
    }

    #[test]
    fn a_large_file_gets_its_start_and_a_map_of_all_of_it() {
        let content = big_rust(40);
        let lines = content.lines().count();
        let out = read("src/big.rs", &content, None, None).unwrap();
        assert!(out.starts_with(PARTIAL_PREFIX));
        assert!(out.contains(&format!("src/big.rs has {lines} lines")));
        assert!(out.contains("Shown: lines 1-60"));
        assert!(out.contains("\nuse std::fs;\n"), "the start of the file");
        assert!(out.contains("[Map of src/big.rs (line: declaration)]"));
        assert!(out.contains("\n3: pub fn f0(x: u32) -> u32 {"));
        // The last function, far past the first 20,000 characters.
        let last = 3 + 39 * 30;
        assert!(out.contains(&format!("\n{last}: pub fn f39(x: u32) -> u32 {{")), "{out}");
        assert!(out.chars().count() < MAX_CHARS);
    }

    #[test]
    fn the_map_leaves_out_the_deepest_nesting_first() {
        let mut content = String::new();
        for i in 0..300 {
            content.push_str(&format!("impl Thing{i} {{\n    pub fn method_with_a_long_name_{i}(&self) {{}}\n{}}}\n", "    // filler line\n".repeat(5)));
        }
        let out = read("src/t.rs", &content, None, None).unwrap();
        let map = out.split("(line: declaration)]\n").nth(1).unwrap();
        assert!(map.contains("1: impl Thing0 {"));
        assert!(map.contains(": impl Thing299 {"), "every top-level block fits");
        assert!(!map.contains("method_with_a_long_name_"), "nested methods are left out");
        assert!(map.contains("[+300 more declarations"));
    }

    #[test]
    fn a_map_too_big_even_at_the_top_level_stops_at_its_limit() {
        let content: String = (0..2_000).map(|i| format!("fn function_number_{i}() {{}}\n")).collect();
        let out = read("src/many.rs", &content, None, None).unwrap();
        let map = out.split("(line: declaration)]\n").nth(1).unwrap();
        assert!(map.chars().count() < MAP_CHARS + 200);
        assert!(map.contains("1: fn function_number_0() {}"));
        assert!(map.contains("more declarations"));
    }

    #[test]
    fn markdown_maps_headings_outside_code_blocks() {
        let mut content = String::from("# Title\n\n```sh\n# not a heading\n```\n\n## Part\n");
        content.push_str(&"text text text text text text\n".repeat(1_000));
        content.push_str("### Deep\n");
        let out = read("docs/guide.md", &content, None, None).unwrap();
        assert!(out.contains("\n1: # Title"));
        assert!(out.contains("\n7:   ## Part"));
        assert!(out.contains(&format!("\n{}:     ### Deep", 1_008)));
        assert!(!out.contains(": # not a heading"));
    }

    #[test]
    fn an_unknown_kind_of_file_has_no_map() {
        let content = "type this line\n".repeat(2_000);
        let out = read("logs/run.log", &content, None, None).unwrap();
        assert!(out.starts_with(PARTIAL_PREFIX));
        assert!(out.ends_with("[No map for this kind of file: read it in ranges.]"));
        assert!(!out.contains(", then a map"));
    }

    #[test]
    fn a_minified_file_shows_the_start_of_its_first_line() {
        let content = "x".repeat(50_000);
        let out = read("dist/app.min.js", &content, None, None).unwrap();
        assert!(out.contains("Shown: lines 1-1"));
        assert!(out.contains(&format!("{}…", "x".repeat(HEAD_CHARS))));
        assert!(out.chars().count() < HEAD_CHARS + 500);
    }

    #[test]
    fn ranges_are_unchanged_while_they_fit() {
        let content = "a\nb\nc\nd\n";
        assert_eq!(read("f.txt", content, Some(2), Some(3)).unwrap(), "b\nc");
        assert_eq!(read("f.txt", content, Some(3), None).unwrap(), "c\nd");
        assert_eq!(read("f.txt", content, None, Some(2)).unwrap(), "a\nb");
        assert_eq!(read("f.txt", content, Some(2), Some(99)).unwrap(), "b\nc\nd");
    }

    #[test]
    fn a_long_range_stops_and_says_where_to_continue() {
        let content = big_rust(40);
        let out = read("src/big.rs", &content, Some(1), Some(1_000)).unwrap();
        let (note, body) = out.split_once("]\n").unwrap();
        assert!(note.starts_with(PARTIAL_PREFIX));
        let shown = body.lines().count();
        assert!(note.contains(&format!("lines 1-{shown} of ")), "{note}");
        assert!(note.contains(&format!("Continue with start_line={}.", shown + 1)), "{note}");
        assert!(body.chars().count() <= MAX_CHARS);
    }

    #[test]
    fn a_line_longer_than_a_read_shows_its_start() {
        let content = format!("short\n{}\nafter\n", "y".repeat(30_000));
        let out = read("f.js", &content, Some(2), Some(3)).unwrap();
        assert!(out.starts_with("[Partial read: line 2 is 30000 characters long; shown: its first 20000. Continue with start_line=3.]\n"));
        assert_eq!(out.lines().nth(1).unwrap().len(), MAX_CHARS);
    }

    #[test]
    fn impossible_ranges_are_errors_not_panics() {
        let content = "a\nb\nc\n";
        assert_eq!(read("f.txt", content, Some(3), Some(2)).unwrap_err(), "start_line (3) is after end_line (2)");
        assert_eq!(read("f.txt", content, Some(9), None).unwrap_err(), "f.txt has 3 lines; start_line 9 is past the end");
    }

    #[test]
    fn declarations_after_modifiers_and_variables_only_at_the_top() {
        let yes = [
            (0, "pub(crate) async fn run() {"),
            (0, "export default function App() {"),
            (0, "export const routes = ["),
            (0, "pub const fn size() -> usize {"),
            (4, "pub const fn size() -> usize {"),
            (0, "impl<T: Clone> Stack<T> {"),
            (4, "async def handler(request):"),
            (0, "func (s *Server) Start() error {"),
            (0, "macro_rules! check {"),
            (0, "export type Props = {"),
        ];
        for (indent, line) in yes {
            assert!(is_declaration(line, indent), "{line}");
        }
        let no = [
            (4, "let x = compute();"),
            (4, "const total = a + b;"),
            (0, "// fn commented()"),
            (0, "return type;"),
            (0, "export { a, b };"),
            (0, "public void run() {"),
            (0, "default:"),
            (4, "type: 'function',"),
            (0, "module.exports = {"),
            (2, "static async load() {"), // a class method: no keyword to go by
            (2, "type Effort,"),
            (4, "const mod = IS_MAC ? e.metaKey : e.ctrlKey;"),
            (14, "const type = (e as InputEvent).inputType;"),
        ];
        for (indent, line) in no {
            assert!(!is_declaration(line, indent), "{line}");
        }
    }

    #[test]
    fn character_limits_respect_multibyte_text() {
        let content = "é".repeat(MAX_CHARS + 10);
        let out = read("notes.txt", &content, Some(1), Some(1)).unwrap();
        assert!(out.ends_with(&"é".repeat(MAX_CHARS)));
        assert_eq!(cut("añb", 2), "añ");
        assert_eq!(extension("src/a.TSX"), "tsx");
        assert_eq!(extension(".bashrc"), "");
    }
}
