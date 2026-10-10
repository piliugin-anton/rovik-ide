use super::languages::all;
use super::*;
use std::path::{Path, PathBuf};
use tree_sitter::Query;

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/outline/testdata")
}

/// An outline as text, one declaration per line: lines, nesting, kind,
/// name and signature.
fn render(symbols: &[Symbol]) -> String {
    symbols
        .iter()
        .map(|s| {
            format!(
                "{}-{} {}{} {} | {}\n",
                s.start_line,
                s.end_line,
                "  ".repeat(s.depth),
                s.kind,
                s.name,
                s.signature
            )
        })
        .collect()
}

/// Every sample in testdata/ against its `.outline` file. After a change
/// that's meant to alter them: UPDATE_OUTLINES=1 cargo test outline, then
/// review the diff.
#[test]
fn samples_match_their_expected_outlines() {
    let update = std::env::var_os("UPDATE_OUTLINES").is_some();
    let mut samples: Vec<PathBuf> = std::fs::read_dir(testdata())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_none_or(|e| e != "outline"))
        .collect();
    samples.sort();
    let mut failures = Vec::new();
    let mut languages = std::collections::BTreeSet::new();
    for sample in &samples {
        let name = sample.file_name().unwrap().to_string_lossy().to_string();
        let content = std::fs::read_to_string(sample).unwrap();
        let lang = language_for(&name, &content).unwrap_or_else(|| panic!("{name}: no language"));
        languages.insert(lang.name);
        let symbols = outline(&name, &content).unwrap_or_else(|| panic!("{name}: no outline"));
        assert!(
            !symbols.is_empty(),
            "{name} ({}): no declarations found",
            lang.name
        );
        let got = render(&symbols);
        let expected_path = sample.with_file_name(format!("{name}.outline"));
        if update {
            std::fs::write(&expected_path, &got).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expected_path).unwrap_or_default();
        if got != expected {
            failures.push(format!(
                "{name} ({}):\n--- expected\n{expected}--- got\n{got}",
                lang.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Every language has a sample.
    let missing: Vec<&str> = all()
        .iter()
        .map(|l| l.name)
        .filter(|n| !languages.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "languages without a sample in testdata/: {missing:?}"
    );
}

#[test]
fn every_kind_and_query_exists_in_its_grammar() {
    let mut problems = Vec::new();
    for lang in all() {
        let grammar = lang.grammar();
        for kind in lang.kinds() {
            if grammar.id_for_node_kind(kind, true) == 0 {
                problems.push(format!("{}: no node kind {kind}", lang.name));
            }
        }
        let source = lang.query_source();
        if !source.trim().is_empty() {
            if let Err(e) = Query::new(&grammar, &source) {
                problems.push(format!("{}: query: {e}", lang.name));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Prints a file's outline: OUTLINE_FILE=path cargo test outline::tests::dump -- --ignored --nocapture
#[test]
#[ignore]
fn dump() {
    let path = std::path::PathBuf::from(std::env::var("OUTLINE_FILE").unwrap());
    let mut paths: Vec<_> = if path.is_dir() {
        std::fs::read_dir(&path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    } else {
        vec![path]
    };
    paths.sort();
    for path in paths {
        let content = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let lang = language_for(&name, &content).map_or("-", |l| l.name);
        println!("=== {name} ({lang})");
        match outline(&name, &content) {
            None => println!("(no outline)"),
            Some(symbols) => {
                for s in symbols {
                    println!(
                        "{}-{} {}[{}] {} | {}",
                        s.start_line,
                        s.end_line,
                        "  ".repeat(s.depth),
                        s.kind,
                        s.signature,
                        s.name
                    );
                }
            }
        }
    }
}

/// Prints a file's syntax tree: OUTLINE_FILE=path cargo test outline::tests::tree -- --ignored --nocapture
#[test]
#[ignore]
fn tree() {
    let path = std::env::var("OUTLINE_FILE").unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let lang = language_for(&path, &content).expect("no language");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang.grammar()).unwrap();
    let tree = parser.parse(&content, None).unwrap();
    let mut cursor = tree.walk();
    let mut depth = 0;
    loop {
        let n = cursor.node();
        if n.is_named() {
            let field = cursor
                .field_name()
                .map(|f| format!("{f}: "))
                .unwrap_or_default();
            let text: String = content[n.start_byte()..n.end_byte()]
                .chars()
                .take(40)
                .collect::<String>()
                .replace('\n', "⏎");
            println!(
                "{}{field}{} [{}-{}] {text}",
                "  ".repeat(depth),
                n.kind(),
                n.start_position().row + 1,
                n.end_position().row + 1
            );
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
            depth -= 1;
        }
    }
}

// ---- which language ----

fn lang_name(path: &str, content: &str) -> Option<&'static str> {
    language_for(path, content).map(|l| l.name)
}

#[test]
fn languages_are_found_by_extension_file_name_and_path_shape() {
    assert_eq!(lang_name("src/main.rs", ""), Some("Rust"));
    assert_eq!(
        lang_name("SRC/MAIN.RS", ""),
        Some("Rust"),
        "extensions ignore case"
    );
    assert_eq!(
        lang_name("src\\lib\\mod.rs", ""),
        Some("Rust"),
        "Windows paths"
    );
    assert_eq!(lang_name("app.component.tsx", ""), Some("TSX"));
    assert_eq!(lang_name("types.d.ts", ""), Some("TypeScript"));
    assert_eq!(lang_name("Makefile", ""), Some("Make"));
    assert_eq!(lang_name("sub/dir/CMakeLists.txt", ""), Some("CMake"));
    assert_eq!(lang_name("BUILD", ""), Some("Starlark"));
    assert_eq!(lang_name("Gemfile", ""), Some("Ruby"));
    assert_eq!(lang_name("Jenkinsfile", ""), Some("Groovy"));
    assert_eq!(lang_name(".bashrc", ""), Some("Bash"));
    assert_eq!(lang_name("notes.txt", ""), None);
    assert_eq!(lang_name("README", ""), None);
    assert_eq!(
        lang_name(".gitignore", ""),
        None,
        "a dotfile isn't an extension"
    );
    assert_eq!(lang_name("archive.tar.gz", ""), None);
    assert_eq!(lang_name("makefile.bak", ""), None);
}

#[test]
fn shared_extensions_are_told_apart_by_content() {
    assert_eq!(lang_name("a.h", "int add(int a, int b);\n"), Some("C"));
    assert_eq!(
        lang_name("a.h", "namespace app {\nclass Box {};\n}\n"),
        Some("C++")
    );
    assert_eq!(
        lang_name("a.h", "#include <vector>\nstd::vector<int> v();\n"),
        Some("C++")
    );
    assert_eq!(
        lang_name(
            "a.h",
            "#import <Foundation/Foundation.h>\n@interface A : NSObject\n@end\n"
        ),
        Some("Objective-C")
    );
    assert_eq!(
        lang_name("a.m", "@implementation A\n@end\n"),
        Some("Objective-C")
    );
    assert_eq!(
        lang_name("a.m", "function y = f(x)\n  y = x;\nend\n"),
        Some("MATLAB")
    );
}

// ---- edge cases of the outline itself ----

fn names(path: &str, content: &str) -> Vec<String> {
    outline(path, content)
        .expect("an outline")
        .into_iter()
        .map(|s| s.name)
        .collect()
}

#[test]
fn markdown_text_before_the_first_heading_isnt_a_section() {
    assert_eq!(outline("a.md", "Just text.\n\nMore text.\n"), Some(vec![]));
    let o = outline("a.md", "Intro text.\n\n# Title\n\nBody.\n").unwrap();
    assert_eq!(o.len(), 1);
    assert_eq!((o[0].name.as_str(), o[0].start_line), ("# Title", 3));
}

#[test]
fn empty_and_declaration_free_files_have_empty_outlines() {
    assert_eq!(outline("a.rs", ""), Some(vec![]));
    assert_eq!(outline("a.rs", "\n\n   \n"), Some(vec![]));
    assert_eq!(
        outline("a.rs", "// just a comment\n/* and another */\n"),
        Some(vec![])
    );
    assert_eq!(outline("a.py", "print('hi')\n"), Some(vec![]));
    assert_eq!(
        outline("notes.txt", "fn not_code() {}"),
        None,
        "no grammar: no outline"
    );
}

#[test]
fn every_language_handles_empty_and_foreign_content_without_panicking() {
    let samples: Vec<(String, String)> = std::fs::read_dir(testdata())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_none_or(|e| e != "outline"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().to_string(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    let junk = [
        "",
        "\n",
        "{{{{",
        "}}}}",
        ")))(((",
        "\u{feff}",
        "\0\0\0",
        "é\u{1F600}\u{200B}",
        "/*",
        "\"unterminated",
    ];
    for lang in all() {
        let path = lang.sample_name();
        for text in junk {
            assert!(
                outline(&path, text).is_some(),
                "{}: no outline for {text:?}",
                lang.name
            );
        }
        // Every other language's sample, parsed as this one.
        for (name, content) in &samples {
            let symbols = outline(&path, content)
                .unwrap_or_else(|| panic!("{}: no outline for {name}", lang.name));
            for s in symbols {
                assert!(
                    s.start_line >= 1 && s.start_line <= s.end_line,
                    "{}: {name}: {s:?}",
                    lang.name
                );
            }
        }
    }
}

#[test]
fn line_ranges_cover_whole_declarations() {
    let src = "fn one() {}\n\nfn two(\n    a: u8,\n) -> u8 {\n    a\n}\n";
    let o = outline("a.rs", src).unwrap();
    assert_eq!((o[0].start_line, o[0].end_line), (1, 1));
    assert_eq!((o[1].start_line, o[1].end_line), (3, 7));
}

#[test]
fn a_block_ending_at_a_line_start_ends_on_the_line_before() {
    // Python's function body ends where the next line starts.
    let src = "def f():\n    return 1\n\ndef g():\n    pass\n";
    let o = outline("a.py", src).unwrap();
    assert_eq!((o[0].start_line, o[0].end_line), (1, 2));
    assert_eq!((o[1].start_line, o[1].end_line), (4, 5));
}

#[test]
fn crlf_line_endings_give_the_same_lines() {
    let lf = "struct A;\n\nimpl A {\n    fn m(&self) {}\n}\n";
    let crlf = lf.replace('\n', "\r\n");
    let a = outline("a.rs", lf).unwrap();
    let b = outline("a.rs", &crlf).unwrap();
    assert_eq!(a, b);
}

#[test]
fn a_byte_order_mark_doesnt_shift_anything() {
    let src = "\u{feff}def first():\n    pass\n";
    let o = outline("a.py", src).unwrap();
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].name, "first");
    assert_eq!(o[0].start_line, 1);
}

#[test]
fn unicode_names_and_text_are_kept_whole() {
    let src = "/// Ünïcödé\npub fn grüße_日本(x: &str) -> String { x.into() }\nstruct Ω;\n";
    let o = outline("a.rs", src).unwrap();
    assert_eq!(o[0].name, "grüße_日本");
    assert_eq!(o[0].signature, "pub fn grüße_日本(x: &str) -> String");
    assert_eq!(o[1].name, "Ω");
    // Cut by characters, never inside one.
    let long = format!("fn {}() {{}}\n", "日".repeat(400));
    let s = &outline("a.rs", &long).unwrap()[0];
    assert_eq!(s.signature.chars().count(), SIGNATURE_CHARS + 1);
    assert!(s.signature.ends_with('…'));
    assert_eq!(s.name.chars().count(), NAME_CHARS + 1);
}

#[test]
fn multi_line_signatures_become_one_line() {
    let src = "pub fn tool_edit_file(\n    root: String,\n    path: String,\n) -> Result<String, String> {\n    todo!()\n}\n";
    let o = outline("a.rs", src).unwrap();
    assert_eq!(
        o[0].signature,
        "pub fn tool_edit_file( root: String, path: String, ) -> Result<String, String>"
    );
}

#[test]
fn a_long_declarative_part_isnt_a_signature() {
    let mut src = String::from("program Big;\n");
    for i in 0..20 {
        src.push_str(&format!("var v{i}: Integer;\n"));
    }
    src.push_str("begin\nend.\n");
    let o = outline("a.pas", &src).unwrap();
    assert_eq!(o[0].signature, "program Big;");
}

#[test]
fn wrappers_are_part_of_the_range_and_decorators_not_of_the_signature() {
    let py = "@app.route('/x')\n@login_required\ndef handler():\n    pass\n";
    let o = outline("a.py", py).unwrap();
    assert_eq!(
        (o[0].start_line, o[0].end_line),
        (1, 4),
        "decorators belong to it"
    );
    assert_eq!(o[0].signature, "def handler():");

    let ts = "export default async function load(): Promise<void> {\n}\n";
    let o = outline("a.ts", ts).unwrap();
    assert_eq!(
        o[0].signature,
        "export default async function load(): Promise<void>"
    );

    let cpp = "template <typename T>\nT max(T a, T b) { return a > b ? a : b; }\n";
    let o = outline("a.cpp", cpp).unwrap();
    assert_eq!(
        (o[0].start_line, o[0].end_line),
        (1, 2),
        "the template line belongs to it"
    );
    assert_eq!(o[0].name, "max");
}

#[test]
fn the_same_declaration_is_listed_once() {
    // A typedef'd struct is one declaration, not a typedef and a struct.
    let o = outline("a.c", "typedef struct { int x; } Point;\n").unwrap();
    assert_eq!(o.len(), 1, "{o:?}");
    assert_eq!(o[0].name, "Point");
    // A function-valued `export const` matches two patterns: once.
    let o = outline("a.ts", "export const f = () => 1;\n").unwrap();
    assert_eq!(o.len(), 1, "{o:?}");
}

#[test]
fn nesting_follows_containment() {
    let src = "class A:\n    def m(self):\n        def inner():\n            pass\n    class B:\n        def n(self): pass\ndef top(): pass\n";
    let o = outline("a.py", src).unwrap();
    let shape: Vec<(String, usize)> = o.into_iter().map(|s| (s.name, s.depth)).collect();
    let expected = [
        ("A", 0),
        ("m", 1),
        ("inner", 2),
        ("B", 1),
        ("n", 2),
        ("top", 0),
    ];
    assert_eq!(shape, expected.map(|(n, d)| (n.to_string(), d)));
}

#[test]
fn deep_nesting_doesnt_overflow() {
    let depth = 300;
    let mut src = String::new();
    for i in 0..depth {
        src.push_str(&format!("{}function f{i}() {{\n", "  ".repeat(i)));
    }
    for i in (0..depth).rev() {
        src.push_str(&format!("{}}}\n", "  ".repeat(i)));
    }
    let o = outline("a.js", &src).unwrap();
    assert_eq!(o.len(), depth);
    assert_eq!(o.last().unwrap().depth, depth - 1);
}

#[test]
fn broken_code_still_outlines_what_parses() {
    let src =
        "fn before() {}\nfn broken( {\n    let x = ;\n\nstruct After { a: u8 }\nfn after() {}\n";
    let got = names("a.rs", src);
    assert_eq!(got.first().map(String::as_str), Some("before"), "{got:?}");
    let src = "def ok():\n    pass\n\ndef bad(:\n    pass\n\ndef also_ok():\n    pass\n";
    let got = names("a.py", src);
    assert!(
        got.contains(&"ok".to_string()) && got.contains(&"also_ok".to_string()),
        "{got:?}"
    );
}

#[test]
fn minified_code_on_one_line_is_outlined() {
    let src: String = (0..500)
        .map(|i| format!("function f{i}(a){{return a+{i}}}"))
        .collect();
    let o = outline("app.min.js", &src).unwrap();
    assert_eq!(o.len(), 500);
    assert!(o.iter().all(|s| s.start_line == 1 && s.end_line == 1));
    assert_eq!(o[499].signature, "function f499(a)");
}

#[test]
fn files_over_the_size_limit_arent_parsed() {
    let src = "x = 1\n".repeat(MAX_BYTES / 6 + 10);
    assert!(src.len() > MAX_BYTES);
    assert_eq!(outline("a.py", &src), None);
}

#[test]
fn the_number_of_declarations_is_capped() {
    let mut src = String::from("{\n");
    for i in 0..MAX_SYMBOLS + 100 {
        src.push_str(&format!("\"k{i}\": {i},\n"));
    }
    src.push_str("\"last\": 0\n}\n");
    let o = outline("big.json", &src).unwrap();
    assert_eq!(o.len(), MAX_SYMBOLS);
}

#[test]
fn outlines_are_deterministic() {
    for name in [
        "sample.ts",
        "sample.rs",
        "sample.cpp",
        "sample.ex",
        "sample.md",
    ] {
        let content = std::fs::read_to_string(testdata().join(name)).unwrap();
        assert_eq!(outline(name, &content), outline(name, &content), "{name}");
    }
}

#[test]
fn a_large_real_world_sized_file_is_quick() {
    let mut src = String::new();
    for i in 0..5_000 {
        src.push_str(&format!("export class C{i} {{\n  m{i}(a: number): number {{ return a; }}\n  p{i} = () => {i};\n}}\n"));
    }
    let start = std::time::Instant::now();
    let o = outline("big.ts", &src).unwrap();
    assert_eq!(o.len(), 15_000);
    // Generous: catches accidental quadratic work, not machine speed.
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
}

// ---- syntax errors ----

#[test]
fn clean_files_have_no_syntax_errors() {
    for (name, src) in [
        (
            "a.rs",
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        ),
        (
            "a.ts",
            "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
        ),
        ("a.py", "def add(a, b):\n    return a + b\n"),
    ] {
        assert_eq!(syntax_errors(name, src), Some(Vec::new()), "{name}");
    }
}

#[test]
fn broken_files_report_lines() {
    let errors =
        syntax_errors("a.rs", "pub fn add(a: i32, b: i32) -> i32 {\n    a + \n}\n").unwrap();
    assert!(!errors.is_empty(), "an unfinished expression is an error");
    assert!(
        errors.iter().all(|e| e.start_line >= 1 && e.start_col >= 1),
        "{errors:?}"
    );
    assert!(errors.iter().all(|e| !e.message.is_empty()), "{errors:?}");

    let errors = syntax_errors("a.ts", "export function add( {\n  return 1;\n}\n").unwrap();
    assert!(!errors.is_empty(), "{errors:?}");

    // One problem per broken region, not per nested node.
    let errors = syntax_errors("a.py", "def f(:\n    pass\n").unwrap();
    assert!(errors.len() <= 3, "{errors:?}");
}

#[test]
fn syntax_errors_need_a_known_language() {
    assert_eq!(syntax_errors("notes.txt", "hello ((("), None);
}
