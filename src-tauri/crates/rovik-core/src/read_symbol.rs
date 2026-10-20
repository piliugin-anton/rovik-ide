//! `read` by symbol: one declaration (a function, a class, a method, a
//! section…) found by name in the file's outline, with the comments and
//! attributes just above it.
//!
//! - `parse` finds a declaration of that name; `Parser::parse`,
//!   `ChatStore.append`, `outer/inner` or `Parser > parse` (as search_code
//!   and this module's headers write it) names it inside another (any of
//!   `::`, `.`, `/`, `#`, ` > ` separates). Names are compared without generic
//!   arguments, quotes and Markdown's `#`s: `Point` finds `impl<T> Point<T>`.
//! - Several matches: the model gets them, with lines, to pick one.
//! - No match: names that come close, and the file's top-level
//!   declarations.
//! - Too long for one read: its start, the declarations nested in it, and
//!   where to continue.
//!
//! The result starts with [`SYMBOL_PREFIX`] and the lines it covers, which
//! the history (chat_db/history.rs) reads to know what this copy of the
//! file holds. Pure string work over the outline, tested below.

use crate::outline::{self, Symbol};
use crate::read_file::{MAX_CHARS, PARTIAL_PREFIX};

/// A whole declaration read by symbol starts with this.
pub const SYMBOL_PREFIX: &str = "[Symbol read: ";
/// Candidates listed when a name is ambiguous, names suggested when there's
/// no match, top-level declarations listed then.
const MAX_CANDIDATES: usize = 12;
const MAX_SUGGESTIONS: usize = 5;
const MAX_LISTED: usize = 40;
/// Comment and attribute lines above a declaration taken with it, at most.
const MAX_LEADING_LINES: usize = 60;

/// The declaration named `symbol` in `content`, the file at `path`.
pub fn read(path: &str, content: &str, symbol: &str) -> Result<String, String> {
    let query = symbol.trim();
    if query.is_empty() {
        return Err(
            "\"symbol\" is empty: give a declaration's name, e.g. \"parse\" or \"Parser::parse\"."
                .into(),
        );
    }
    let Some(symbols) = outline::outline(path, content) else {
        return Err(match outline::language_for(path, content) {
            None => format!(
                "{path}: reading by symbol needs a language the outline knows; read it by start_line/end_line instead."
            ),
            // Too large or too slow to parse.
            Some(_) => format!("{path} couldn't be outlined (too large); read it by start_line/end_line instead."),
        });
    };
    let matches = find(&symbols, query);
    match matches.as_slice() {
        [] => Err(not_found(path, &symbols, query)),
        [one] => Ok(show(path, content, &symbols, *one)),
        many => Err(ambiguous(path, &symbols, query, many)),
    }
}

/// The lines a symbol read covers, from its first line (for the history).
pub fn covered_lines(result: &str) -> Option<(u64, u64)> {
    let rest = result.strip_prefix(SYMBOL_PREFIX)?;
    let (_, after) = rest.split_once(", lines ")?;
    let (range, _) = after.split_once(" of ")?;
    let (a, b) = range.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// A name as compared: no generic arguments, parameters, quotes or
/// Markdown heading marks; `impl<T> Point<T>` is `Point`.
fn normalize(name: &str) -> String {
    let mut s = name.trim();
    s = s.trim_start_matches('#').trim();
    s = s.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    let s = match s.find(['<', '(', '[']) {
        Some(0) | None => s,
        Some(i) => &s[..i],
    };
    s.trim().trim_end_matches(':').trim().to_string()
}

/// Indexes of the symbols `query` names: by whole name first (`M.add` in
/// Lua is one name), else as a path whose last part is the name and whose
/// earlier parts are, in order, among its ancestors.
fn find(symbols: &[Symbol], query: &str) -> Vec<usize> {
    // The whole query as one name: a plain name, or one with separators in
    // it as written (`M.add`), never cut short (`Point<T>::new`).
    let has_separator =
        query.contains("::") || query.contains(" > ") || query.contains(['.', '/', '#']);
    let whole: Vec<usize> = (0..symbols.len())
        .filter(|&i| {
            let name = &symbols[i].name;
            if has_separator {
                name.trim() == query
            } else {
                normalize(name) == normalize(query)
            }
        })
        .collect();
    if !whole.is_empty() {
        return whole;
    }
    let parts: Vec<String> = query
        .split(" > ")
        .flat_map(|p| p.split("::"))
        .flat_map(|p| p.split(['.', '/', '#']))
        .map(normalize)
        .filter(|p| !p.is_empty())
        .collect();
    let Some((last, outer)) = parts.split_last() else {
        return Vec::new();
    };
    (0..symbols.len())
        .filter(|&i| normalize(&symbols[i].name) == *last)
        .filter(|&i| {
            // The outer parts, innermost last, among the ancestors (outermost first).
            let chain: Vec<String> = ancestors(symbols, i)
                .iter()
                .rev()
                .map(|&a| normalize(&symbols[a].name))
                .collect();
            let mut want = outer.iter();
            let mut next = want.next();
            for name in &chain {
                if Some(name) == next {
                    next = want.next();
                }
            }
            next.is_none()
        })
        .collect()
}

/// The declarations symbol `i` is in, innermost first (the outline lists
/// parents before children, with their depth).
pub(crate) fn ancestors(symbols: &[Symbol], i: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut depth = symbols[i].depth;
    for j in (0..i).rev() {
        if depth == 0 {
            break;
        }
        if symbols[j].depth < depth {
            out.push(j);
            depth = symbols[j].depth;
        }
    }
    out
}

/// The innermost declaration line `line` is in, if any.
pub fn enclosing(symbols: &[Symbol], line: usize) -> Option<usize> {
    // Parents come before their children: the last one containing it is
    // the innermost.
    (0..symbols.len())
        .filter(|&i| symbols[i].start_line <= line && line <= symbols[i].end_line)
        .max_by_key(|&i| symbols[i].depth)
}

/// `Parser > parse`: a symbol and what it's in (read takes it as a
/// symbol).
pub fn qualified(symbols: &[Symbol], i: usize) -> String {
    let mut names: Vec<String> = ancestors(symbols, i)
        .iter()
        .rev()
        .map(|&a| normalize(&symbols[a].name))
        .collect();
    names.push(normalize(&symbols[i].name));
    names.join(" > ")
}

fn lines_label(start: usize, end: usize) -> String {
    format!("{start}-{end}")
}

/// Whether a line (trimmed) is a comment or an attribute that belongs to
/// the declaration below it.
fn is_leading(line: &str) -> bool {
    let t = line.trim_start();
    ["///", "//!", "//", "/*", "*", "#[", "#![", "@", "--", ";;", "(*", "{-", "%"].iter().any(|m| t.starts_with(m))
        // `# comment` (Python, Ruby, shell); not C's `#include`.
        || t == "#"
        || t.starts_with("# ")
}

/// Where a declaration's text starts: its first line, or the comments and
/// attributes right above it (up to a blank line, and never into the
/// declaration before it).
fn leading_start(lines: &[&str], symbols: &[Symbol], i: usize) -> usize {
    let start = symbols[i].start_line;
    // The previous declaration's last line ends what can be taken.
    let floor = symbols[..i]
        .iter()
        .filter(|s| s.end_line < start)
        .map(|s| s.end_line)
        .max()
        .unwrap_or(0);
    let mut first = start;
    while first > 1 && first - 1 > floor && start - (first - 1) <= MAX_LEADING_LINES {
        let line = lines[first - 2];
        if line.trim().is_empty() || !is_leading(line) {
            break;
        }
        first -= 1;
    }
    first
}

/// The declaration's text with a header, or its start when it's too long.
fn show(path: &str, content: &str, symbols: &[Symbol], i: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let s = &symbols[i];
    let end = s.end_line.min(lines.len());
    let start = leading_start(&lines, symbols, i).min(end.max(1));
    let name = qualified(symbols, i);
    let text = lines[start - 1..end].join("\n");
    if text.chars().count() <= MAX_CHARS {
        return format!(
            "{SYMBOL_PREFIX}{name}, lines {} of {path}]\n{text}",
            lines_label(start, end)
        );
    }

    // Too long: as much as fits, and what's nested in it.
    let nested: Vec<String> = symbols
        .iter()
        .enumerate()
        .skip(i + 1)
        .take_while(|(_, n)| n.start_line <= s.end_line && n.depth > s.depth)
        .filter(|(_, n)| n.depth == s.depth + 1)
        .take(MAX_LISTED)
        .map(|(_, n)| {
            format!(
                "  {} {}",
                lines_label(n.start_line, n.end_line),
                n.signature
            )
        })
        .collect();
    let budget = MAX_CHARS.saturating_sub(nested.iter().map(|l| l.len() + 1).sum::<usize>());
    let mut shown = 0;
    let mut used = 0;
    for line in &lines[start - 1..end] {
        let len = line.chars().count() + 1;
        if used + len > budget && shown > 0 {
            break;
        }
        used += len;
        shown += 1;
    }
    let last = start + shown - 1;
    let body: String = lines[start - 1..last].join("\n");
    let body = if body.chars().count() > budget {
        body.chars().take(budget).collect()
    } else {
        body
    };
    let mut out = format!(
        "{PARTIAL_PREFIX}{name} is lines {} of {path}, more than one read holds: shown lines {}. \
         Continue with start_line={}, end_line={end}, or read one of its declarations by symbol.]\n",
        lines_label(start, end),
        lines_label(start, last),
        last + 1,
    );
    if !nested.is_empty() {
        out.push_str(&format!("[Declared in it]\n{}\n\n", nested.join("\n")));
    }
    out.push_str(&body);
    out
}

fn ambiguous(path: &str, symbols: &[Symbol], query: &str, matches: &[usize]) -> String {
    let listed: Vec<String> = matches
        .iter()
        .take(MAX_CANDIDATES)
        .map(|&i| {
            format!(
                "- {} (lines {}): {}",
                qualified(symbols, i),
                lines_label(symbols[i].start_line, symbols[i].end_line),
                symbols[i].signature
            )
        })
        .collect();
    let more = matches.len().saturating_sub(MAX_CANDIDATES);
    let more = if more > 0 {
        format!("\n…and {more} more")
    } else {
        String::new()
    };
    format!(
        "{} declarations in {path} are called \"{query}\". Name the one you want with what it's in (e.g. \"Outer::name\"), or read its lines with start_line/end_line:\n{}{more}",
        matches.len(),
        listed.join("\n")
    )
}

fn not_found(path: &str, symbols: &[Symbol], query: &str) -> String {
    if symbols.is_empty() {
        return format!(
            "{path} has no declarations to read by symbol; read it by start_line/end_line instead."
        );
    }
    // Close: the same name in another case, or containing / contained in it.
    let wanted =
        normalize(query.rsplit(['.', '/', '#', ':']).next().unwrap_or(query)).to_lowercase();
    let mut close: Vec<String> = Vec::new();
    for (i, s) in symbols.iter().enumerate() {
        let name = normalize(&s.name).to_lowercase();
        if !wanted.is_empty()
            && !name.is_empty()
            && (name == wanted || name.contains(&wanted) || wanted.contains(&name))
        {
            let q = qualified(symbols, i);
            if !close.contains(&q) {
                close.push(q);
            }
        }
        if close.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    let mut out = format!("No declaration \"{query}\" in {path}.");
    if !close.is_empty() {
        out.push_str(&format!(" Did you mean: {}?", close.join(", ")));
    }
    let top: Vec<String> = symbols
        .iter()
        .filter(|s| s.depth == 0)
        .take(MAX_LISTED)
        .map(|s| {
            format!(
                "- {} (lines {})",
                normalize(&s.name),
                lines_label(s.start_line, s.end_line)
            )
        })
        .collect();
    let count = symbols.iter().filter(|s| s.depth == 0).count();
    out.push_str(&format!(
        "\nIts top-level declarations{}:\n{}",
        if count > MAX_LISTED {
            format!(" (first {MAX_LISTED} of {count})")
        } else {
            String::new()
        },
        top.join("\n")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = "use std::fmt;

/// A point.
/// Two lines of docs.
#[derive(Debug, Clone)]
pub struct Point<T> {
    x: T,
}

impl<T: Clone> Point<T> {
    pub fn new(x: T) -> Self {
        Point { x }
    }

    // Plain comment.
    pub fn get(&self) -> T {
        self.x.clone()
    }
}

impl fmt::Display for Point<u8> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, \"{}\", self.x)
    }
}

pub struct Circle;

impl Circle {
    pub fn new() -> Self {
        Circle
    }
}
";

    fn body(out: &str) -> &str {
        out.split_once("]\n").unwrap().1
    }

    #[test]
    fn a_declaration_comes_with_its_docs_and_attributes() {
        let out = read("src/point.rs", RUST, "Point").unwrap_err();
        // Point is the struct and two impls: ambiguous.
        assert!(
            out.starts_with("3 declarations in src/point.rs are called \"Point\""),
            "{out}"
        );

        let out = read("src/point.rs", RUST, "get").unwrap();
        assert_eq!(
            out.lines().next().unwrap(),
            "[Symbol read: Point > get, lines 15-18 of src/point.rs]"
        );
        assert_eq!(
            body(&out),
            "    // Plain comment.\n    pub fn get(&self) -> T {\n        self.x.clone()\n    }"
        );
    }

    #[test]
    fn a_name_inside_another_picks_one_of_several() {
        let err = read("src/point.rs", RUST, "new").unwrap_err();
        assert!(
            err.starts_with("2 declarations in src/point.rs are called \"new\""),
            "{err}"
        );
        assert!(
            err.contains("- Point > new (lines 11-13): pub fn new(x: T) -> Self"),
            "{err}"
        );
        assert!(
            err.contains("- Circle > new (lines 30-32): pub fn new() -> Self"),
            "{err}"
        );

        for query in [
            "Point::new",
            "Point.new",
            "Point/new",
            "  Point :: new  ",
            "Point<T>::new",
            "Point > new",
        ] {
            let out = read("src/point.rs", RUST, query).unwrap_or_else(|e| panic!("{query}: {e}"));
            assert!(
                out.starts_with("[Symbol read: Point > new, lines 11-13 of src/point.rs]"),
                "{query}: {out}"
            );
        }
        let out = read("src/point.rs", RUST, "Circle::new").unwrap();
        assert!(body(&out).contains("Circle\n"), "{out}");
    }

    #[test]
    fn docs_stop_at_a_blank_line_and_take_attributes() {
        let src = "// Unrelated note.\n\n/// Docs.\n#[inline]\nfn f() {}\n";
        let out = read("a.rs", src, "f").unwrap();
        assert_eq!(body(&out), "/// Docs.\n#[inline]\nfn f() {}");
        assert!(out.starts_with("[Symbol read: f, lines 3-5 of a.rs]"));
    }

    #[test]
    fn leading_lines_never_reach_into_the_declaration_before() {
        let src = "fn a() {\n}\n// about b\nfn b() {}\n";
        let out = read("a.rs", src, "b").unwrap();
        assert_eq!(body(&out), "// about b\nfn b() {}");
        // A one-line previous declaration ending right above: nothing taken.
        let src = "#[test] fn a() {}\n#[test]\nfn b() {}\n";
        let out = read("a.rs", src, "b").unwrap();
        assert_eq!(body(&out), "#[test]\nfn b() {}");
    }

    #[test]
    fn c_preprocessor_lines_are_not_docs() {
        let src = "#include <stdio.h>\n#define X 1\nint main(void) { return 0; }\n";
        let out = read("a.c", src, "main").unwrap();
        assert_eq!(body(&out), "int main(void) { return 0; }");
    }

    #[test]
    fn python_decorators_and_comments_and_nested_classes() {
        let src = "class Outer:\n    class Inner:\n        # Explains it.\n        @staticmethod\n        def run():\n            pass\n\n    def run(self):\n        pass\n";
        let out = read("a.py", src, "Inner.run").unwrap();
        assert!(
            out.starts_with("[Symbol read: Outer > Inner > run, lines 3-6 of a.py]"),
            "{out}"
        );
        assert!(body(&out).starts_with("        # Explains it.\n        @staticmethod\n"));
        // The outer parts may skip levels: Outer.run is the method, not Inner's.
        let out = read("a.py", src, "Outer.run").unwrap_err();
        assert!(
            out.starts_with("2 declarations"),
            "both are inside Outer: {out}"
        );
        let out = read("a.py", src, "Outer::Inner::run").unwrap();
        assert!(out.contains("lines 3-6"));
    }

    #[test]
    fn typescript_class_members_and_exports() {
        let src = "export class ChatStore {\n  append(text: string) {\n    return text;\n  }\n}\nexport const helper = () => 1;\n";
        let out = read("store.ts", src, "ChatStore.append").unwrap();
        assert!(
            out.starts_with("[Symbol read: ChatStore > append, lines 2-4 of store.ts]"),
            "{out}"
        );
        let out = read("store.ts", src, "helper").unwrap();
        assert_eq!(body(&out), "export const helper = () => 1;");
    }

    #[test]
    fn names_with_dots_and_quotes_and_headings() {
        let lua = "local M = {}\nfunction M.add(a, b) return a + b end\nreturn M\n";
        assert!(read("m.lua", lua, "M.add")
            .unwrap()
            .contains("function M.add(a, b)"));
        let json = "{\n  \"scripts\": {\n    \"build\": \"tsc\"\n  }\n}\n";
        let out = read("package.json", json, "scripts.build").unwrap();
        assert_eq!(body(&out), "    \"build\": \"tsc\"");
        let md = "# Guide\n\n## Install\n\nRun it.\n\n## Usage\n\nUse it.\n";
        let out = read("README.md", md, "Install").unwrap();
        assert!(
            out.starts_with("[Symbol read: Guide > Install, lines 3-6 of README.md]"),
            "{out}"
        );
        assert!(read("README.md", md, "## Usage")
            .unwrap()
            .contains("Use it."));
    }

    #[test]
    fn no_match_suggests_close_names_and_lists_the_top_level() {
        let err = read("src/point.rs", RUST, "Getter").unwrap_err();
        assert!(
            err.starts_with(
                "No declaration \"Getter\" in src/point.rs. Did you mean: Point > get?"
            ),
            "{err}"
        );
        assert!(
            err.contains("Its top-level declarations:\n- Point (lines 6-8)\n- Point (lines 10-19)"),
            "{err}"
        );

        let err = read("src/point.rs", RUST, "POINT::NEW").unwrap_err();
        assert!(
            err.contains("Did you mean: Point > new, Circle > new?"),
            "a different case is suggested: {err}"
        );

        let err = read("src/point.rs", RUST, "zzz").unwrap_err();
        assert!(!err.contains("Did you mean"), "{err}");
    }

    #[test]
    fn files_that_cant_be_read_by_symbol_say_how_to_read_them() {
        let err = read("notes.txt", "fn x() {}", "x").unwrap_err();
        assert!(err.contains("needs a language the outline knows"), "{err}");
        let err = read("a.py", "print(1)\n", "x").unwrap_err();
        assert!(err.contains("has no declarations"), "{err}");
        let err = read("a.rs", RUST, "   ").unwrap_err();
        assert!(err.starts_with("\"symbol\" is empty"), "{err}");
        let err = read("a.rs", RUST, "::").unwrap_err();
        assert!(err.starts_with("No declaration"), "{err}");
    }

    #[test]
    fn a_declaration_too_long_for_one_read_gives_its_start_and_what_it_holds() {
        let mut src = String::from("impl Big {\n");
        for i in 0..400 {
            src.push_str(&format!("    pub fn method_number_{i}(&self) -> u64 {{\n        {i} + 1 + 2 + 3 + 4 + 5 + 6\n    }}\n"));
        }
        src.push_str("}\n");
        let out = read("big.rs", &src, "Big").unwrap();
        let (note, rest) = out.split_once("]\n").unwrap();
        assert!(note.starts_with(PARTIAL_PREFIX), "{note}");
        assert!(note.contains("Big is lines 1-1202 of big.rs"), "{note}");
        let shown_to: usize = note
            .split("shown lines 1-")
            .nth(1)
            .unwrap()
            .split('.')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            note.contains(&format!(
                "Continue with start_line={}, end_line=1202",
                shown_to + 1
            )),
            "{note}"
        );
        assert!(
            rest.starts_with("[Declared in it]\n  2-4 pub fn method_number_0(&self) -> u64\n"),
            "{rest}"
        );
        assert!(out.chars().count() <= MAX_CHARS + 500);
        // Its parts can be read one by one.
        assert!(read("big.rs", &src, "Big::method_number_399")
            .unwrap()
            .contains("399 + 1"));
    }

    #[test]
    fn covered_lines_are_read_back_from_the_header() {
        let out = read("src/point.rs", RUST, "Point::get").unwrap();
        assert_eq!(covered_lines(&out), Some((15, 18)));
        assert_eq!(
            covered_lines("[Symbol read: a > b, lines 3-9 of x/y.rs]\ncode"),
            Some((3, 9))
        );
        assert_eq!(
            covered_lines("[Symbol read: odd, lines name, lines 3-9 of y.rs]\n"),
            None
        );
        assert_eq!(covered_lines("plain text"), None);
        assert_eq!(
            covered_lines("[Partial read: Big is lines 1-1202 of big.rs …]"),
            None
        );
    }

    #[test]
    fn crlf_files_read_by_symbol() {
        let src = RUST.replace('\n', "\r\n");
        let out = read("src/point.rs", &src, "Circle::new").unwrap();
        assert!(
            out.starts_with("[Symbol read: Circle > new, lines 30-32 of src/point.rs]"),
            "{out}"
        );
        assert!(!body(&out).contains('\r'));
    }
}
