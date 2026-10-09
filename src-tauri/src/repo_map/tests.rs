use super::*;
use std::fs;

struct Project(PathBuf);

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn project(files: &[(&str, &str)]) -> Project {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!("repo-map-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    for (path, content) in files {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    // Canonical, as the agent's root is.
    Project(fs::canonicalize(&root).unwrap())
}

fn map(p: &Project, focus: Focus) -> Option<String> {
    repo_map(&p.0, &focus, &Options::default())
}

fn map_with(p: &Project, focus: Focus, max_chars: usize) -> Option<String> {
    repo_map(&p.0, &focus, &Options { max_chars, ..Options::default() })
}

fn focus(files: &[&str], words: &[&str]) -> Focus {
    Focus { files: files.iter().map(|s| s.to_string()).collect(), words: words.iter().map(|s| s.to_string()).collect() }
}

/// The files in a map, in order.
fn files_in(map: &str) -> Vec<&str> {
    map.lines().filter(|l| l.ends_with(':') && !l.starts_with(' ')).map(|l| l.trim_end_matches(':')).collect()
}

// ---- what a map shows ----

#[test]
fn a_map_lists_files_and_their_declarations_with_line_ranges() {
    let p = project(&[
        ("src/config.rs", "pub struct Config {\n    pub port: u16,\n}\n\npub fn parse_config(text: &str) -> Config {\n    Config { port: 1 }\n}\n"),
        ("src/main.rs", "mod config;\nfn main() {\n    let c = config::parse_config(\"\");\n    let d: config::Config = c;\n}\n"),
    ]);
    let m = map(&p, Focus::default()).unwrap();
    assert!(m.starts_with("## Repository map\n"), "{m}");
    assert!(m.contains("src/config.rs:\n"), "{m}");
    assert!(m.contains("  5-7 pub fn parse_config(text: &str) -> Config"), "{m}");
    assert!(m.contains("  1-3 pub struct Config"), "{m}");
}

#[test]
fn nested_declarations_show_under_what_they_are_in() {
    let mut store = String::from("export class ChatStore {\n");
    for i in 0..30 {
        store.push_str(&format!("  method{i}() {{}}\n"));
    }
    store.push_str("  appendMessage(text: string) {}\n}\n");
    let p = project(&[
        ("store.ts", &store),
        ("panel.ts", "import { ChatStore } from './store';\nconst s = new ChatStore();\ns.appendMessage('x');\ns.appendMessage('y');\n"),
    ]);
    let m = map_with(&p, focus(&["panel.ts"], &[]), HEADER.len() + 200).unwrap();
    // The method the panel uses, under its class, even when not all fit.
    assert!(m.contains("store.ts:\n  1-33 export class ChatStore\n"), "{m}");
    assert!(m.contains("    32 appendMessage(text: string)"), "{m}");
    assert!(!m.contains("method29"), "the unused methods don't make the cut: {m}");
}

// ---- ranking ----

/// A project of two separate parts: billing (invoice.rs, tax.rs) and chat
/// (message.rs, room.rs), each file using its neighbour.
fn two_parts() -> Project {
    project(&[
        ("billing/invoice.rs", "pub fn create_invoice() { compute_tax_total(); }\n"),
        ("billing/tax.rs", "pub fn compute_tax_total() {}\npub fn tax_rate_for() {}\n"),
        ("chat/message.rs", "pub fn send_chat_message() { join_chat_room(); }\n"),
        ("chat/room.rs", "pub fn join_chat_room() {}\npub fn leave_chat_room() {}\n"),
    ])
}

#[test]
fn the_focus_file_and_what_it_uses_come_first() {
    let p = two_parts();
    // The focus file itself first, then what it uses, then the rest.
    let m = map(&p, focus(&["billing/invoice.rs"], &[])).unwrap();
    let files = files_in(&m);
    assert_eq!(&files[..2], ["billing/invoice.rs", "billing/tax.rs"], "{m}");

    let m = map(&p, focus(&["chat/message.rs"], &[])).unwrap();
    let files = files_in(&m);
    assert_eq!(&files[..2], ["chat/message.rs", "chat/room.rs"], "{m}");
}

#[test]
fn a_name_the_request_mentions_pulls_its_file_up() {
    let p = project(&[
        ("hub.rs", "fn main() { alpha_handler_one(); beta_handler_two(); }\n"),
        ("alpha.rs", "pub fn alpha_handler_one() {}\n"),
        ("beta.rs", "pub fn beta_handler_two() {}\n"),
    ]);
    let m = map(&p, focus(&[], &["beta_handler_two"])).unwrap();
    assert_eq!(files_in(&m)[0], "beta.rs", "{m}");
    let m = map(&p, focus(&[], &["alpha_handler_one"])).unwrap();
    assert_eq!(files_in(&m)[0], "alpha.rs", "{m}");
}

#[test]
fn a_file_named_like_a_mentioned_word_is_a_focus() {
    let p = two_parts();
    let m = map(&p, focus(&[], &["message"])).unwrap();
    assert_eq!(&files_in(&m)[..2], ["chat/room.rs", "chat/message.rs"], "{m}");
}

#[test]
fn name_weights() {
    let none = HashSet::new();
    assert_eq!(name_weight("run", 1, &none), 1.0);
    assert_eq!(name_weight("parse_config", 1, &none), 10.0, "long and specific");
    assert_eq!(name_weight("parseConfig", 1, &none), 10.0, "camelCase too");
    assert_eq!(name_weight("PARSECONFIG", 1, &none), 1.0, "not mixed case: not specific");
    assert_eq!(name_weight("_private", 1, &none), 0.1);
    assert_eq!(name_weight("__private_helper", 1, &none), 1.0, "private, but long and specific");
    assert!((name_weight("render", 6, &none) - 0.1).abs() < 1e-12, "declared in many files");
    let mentioned: HashSet<&str> = ["run"].into();
    assert_eq!(name_weight("run", 1, &mentioned), 10.0);
}

#[test]
fn a_name_declared_everywhere_doesnt_tie_files_together() {
    // Every model file declares `new`; the controller uses `new` a lot and
    // `charge_customer` once.
    let mut files: Vec<(String, String)> =
        (0..8).map(|i| (format!("models/m{i}.rs"), "pub fn new() {}\n".to_string())).collect();
    files.push(("pay.rs".into(), "pub fn charge_customer() {}\n".into()));
    files.push(("controller.rs".into(), format!("fn handle() {{ {} charge_customer(); }}\n", "new(); ".repeat(30))));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let p = project(&refs);
    let m = map(&p, focus(&["controller.rs"], &[])).unwrap();
    assert_eq!(&files_in(&m)[..2], ["controller.rs", "pay.rs"], "{m}");
}

#[test]
fn without_a_focus_the_most_used_declarations_lead() {
    let p = project(&[
        ("util.rs", "pub fn shared_helper_fn() {}\npub fn rarely_used_thing() {}\n"),
        ("a.rs", "fn a() { shared_helper_fn(); }\n"),
        ("b.rs", "fn b() { shared_helper_fn(); }\n"),
        ("c.rs", "fn c() { shared_helper_fn(); rarely_used_thing(); }\n"),
    ]);
    let m = map(&p, Focus::default()).unwrap();
    assert_eq!(files_in(&m)[0], "util.rs", "{m}");
    // With room for one declaration only, it's the one used everywhere.
    let tiny = map_with(&p, Focus::default(), HEADER.len() + 60).unwrap();
    assert!(tiny.contains("shared_helper_fn") && !tiny.contains("rarely_used_thing"), "{tiny}");
}

#[test]
fn a_file_using_its_own_name_doesnt_link_to_others_with_it() {
    // render_page is declared in both; page.rs calls its own.
    let p = project(&[
        ("page.rs", "pub fn render_page() {}\nfn main() { render_page(); render_page(); }\n"),
        ("other.rs", "pub fn render_page() {}\npub fn unrelated_thing() {}\n"),
        ("used.rs", "pub fn really_used_helper() {}\n"),
        ("caller.rs", "fn go() { really_used_helper(); }\n"),
    ]);
    // page.rs uses nothing from elsewhere: other.rs isn't pulled in by
    // the name page.rs declares itself (nor are the unrelated files).
    let m = map(&p, focus(&["page.rs"], &[])).unwrap();
    assert_eq!(files_in(&m), ["page.rs"], "{m}");
}

#[test]
fn locals_dont_link_files() {
    // `helper_value` is a local inside a function in a.ts; b.ts using the
    // same word doesn't make a.ts relevant.
    let p = project(&[
        ("a.ts", "export function outer() {\n  const helper_value = () => 1;\n  return helper_value;\n}\n"),
        ("b.ts", "const x = helper_value + helper_value + helper_value;\nexport function shared_entry_point() {}\n"),
        ("c.ts", "shared_entry_point(); shared_entry_point();\nexport function c_main() {}\n"),
    ]);
    let m = map(&p, focus(&["b.ts"], &[])).unwrap();
    assert!(!files_in(&m).contains(&"a.ts"), "{m}");
    assert!(files_in(&m).contains(&"c.ts") || files_in(&m) == ["b.ts"], "{m}");
    assert_eq!(scopes(&outline::outline("a.ts", "export function outer() {\n  const helper_value = () => 1;\n}\n").unwrap()), [
        Scope { local: false, test: false },
        Scope { local: true, test: false },
    ]);
}

#[test]
fn test_code_counts_less_unless_its_the_focus() {
    let p = project(&[
        ("src/lib.rs", "pub fn compute_total_price() {}\n"),
        ("src/app.rs", "fn run() { compute_total_price(); }\npub fn app_entry() {}\n"),
        ("tests/price_test.rs", "fn checks_price() { compute_total_price(); compute_total_price(); }\npub fn test_helper_fn() {}\n"),
    ]);
    let m = map(&p, Focus::default()).unwrap();
    let files = files_in(&m);
    let test = files.iter().position(|f| *f == "tests/price_test.rs");
    assert!(test.is_none_or(|t| t == files.len() - 1), "test code last, if at all: {m}");
    // Asked about the test: it's first.
    let m = map(&p, focus(&["tests/price_test.rs"], &[])).unwrap();
    assert_eq!(files_in(&m)[0], "tests/price_test.rs", "{m}");
}

#[test]
fn test_paths() {
    for yes in [
        "tests/a.rs", "src/__tests__/x.ts", "pkg/testdata/sample.go", "a/fixtures/b.json", "x_test.go", "x_tests.rs",
        "app.test.tsx", "app.spec.js", "test_models.py", "conftest.py", "e2e/login.ts", "src/agent_run/e2e_tests.rs",
    ] {
        assert!(is_test_path(yes), "{yes}");
    }
    for no in ["src/testing_utils.rs", "src/attest.rs", "contest/main.rs", "latest.ts", "src/lib.rs", "spectrum.rs"] {
        assert!(!is_test_path(no), "{no}");
    }
}

#[test]
fn rarity_of_names() {
    assert_eq!(rarity(1, 1), 1.0);
    assert!((rarity(1, 100) - 1.0).abs() < 1e-9, "one file in a hundred: full weight");
    assert!(rarity(10, 100) < 0.6 && rarity(10, 100) > 0.4);
    assert_eq!(rarity(100, 100), MIN_RARITY, "every file: almost nothing");
    assert_eq!(rarity(0, 100), 1.0, "unused counts as one");
    assert_eq!(rarity(500, 100), MIN_RARITY, "more than all is all");
}

// ---- size ----

#[test]
fn the_map_fits_its_budget_and_grows_with_it() {
    let mut files: Vec<(String, String)> = Vec::new();
    for i in 0..120 {
        let body: String = (0..10).map(|j| format!("pub fn module{i}_function_{j}() {{ module{}_function_{j}(); }}\n", (i + 1) % 120)).collect();
        files.push((format!("src/m{i:03}.rs"), body));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let p = project(&refs);
    let mut sizes = Vec::new();
    for budget in [1_000, 3_000, 9_000] {
        let m = map_with(&p, Focus::default(), budget).unwrap();
        assert!(m.chars().count() <= budget, "{} > {budget}", m.chars().count());
        sizes.push(m.lines().count());
    }
    assert!(sizes[0] < sizes[1] && sizes[1] < sizes[2], "{sizes:?}");
}

#[test]
fn a_budget_too_small_for_anything_gives_no_map() {
    let p = two_parts();
    assert_eq!(map_with(&p, Focus::default(), 10), None);
    assert_eq!(map_with(&p, Focus::default(), 0), None);
}

// ---- which files ----

#[test]
fn ignored_hidden_denylisted_large_and_unknown_files_are_left_out() {
    let big = format!("pub fn huge_file_fn() {{}}\n{}", "// padding\n".repeat(120_000));
    let p = project(&[
        ("src/app.rs", "pub fn real_function() {}\n"),
        (".gitignore", "generated/\n"),
        ("generated/out.rs", "pub fn generated_fn() {}\n"),
        ("node_modules/lib/index.js", "export function vendored_fn() {}\n"),
        (".hidden/secret.rs", "pub fn hidden_fn() {}\n"),
        ("target/debug/build.rs", "pub fn build_output_fn() {}\n"),
        ("notes.txt", "fn not_code() {}\n"),
        ("big.rs", &big),
    ]);
    let m = map(&p, Focus::default()).unwrap();
    assert_eq!(files_in(&m), ["src/app.rs"], "{m}");
}

#[test]
fn a_project_without_declarations_has_no_map() {
    let empty = project(&[]);
    assert_eq!(map(&empty, Focus::default()), None);
    let text_only = project(&[("README.txt", "hello"), ("data.csv", "a,b\n")]);
    assert_eq!(map(&text_only, Focus::default()), None);
    let no_decls = project(&[("script.py", "print('hi')\n")]);
    assert_eq!(map(&no_decls, Focus::default()), None);
}

#[test]
fn a_focus_on_files_that_dont_exist_is_ignored() {
    let p = two_parts();
    let a = map(&p, focus(&["nope.rs", "../outside.rs", ""], &[])).unwrap();
    let b = map(&p, Focus::default()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn many_languages_in_one_map() {
    let p = project(&[
        ("api.py", "def fetch_user_profile(user_id):\n    return None\n"),
        ("web/view.ts", "export function renderUserProfile() { fetch_user_profile(1); }\n"),
        ("core/lib.go", "package core\nfunc LoadUserProfile() {}\n"),
        ("README.md", "# Project\n\n## Setup\n"),
        ("package.json", "{ \"name\": \"fetch_user_profile\" }\n"),
        ("styles.css", ".fetch_user_profile { color: red; }\n"),
    ]);
    let m = map(&p, focus(&["web/view.ts"], &[])).unwrap();
    assert_eq!(&files_in(&m)[..2], ["web/view.ts", "api.py"], "then the file view.ts uses: {m}");
    assert!(m.contains("def fetch_user_profile(user_id):"), "{m}");
    for data in ["README.md", "package.json", "styles.css"] {
        assert!(!files_in(&m).contains(&data), "data and markup aren't in the map: {m}");
    }
}

// ---- the cache, limits, determinism ----

#[test]
fn the_same_project_gives_the_same_map() {
    let p = two_parts();
    let first = map(&p, focus(&["billing/invoice.rs"], &["tax"])).unwrap();
    for _ in 0..5 {
        assert_eq!(map(&p, focus(&["billing/invoice.rs"], &["tax"])).unwrap(), first);
    }
    clear_cache();
    assert_eq!(map(&p, focus(&["billing/invoice.rs"], &["tax"])).unwrap(), first, "cached or not");
}

#[test]
fn edited_and_deleted_files_are_seen_at_once() {
    let p = project(&[("a.rs", "pub fn original_name() {}\n"), ("b.rs", "pub fn other_function() {}\n")]);
    let m = map(&p, Focus::default()).unwrap();
    assert!(m.contains("original_name"));
    // A different size: the cache can tell even within the same second.
    fs::write(p.0.join("a.rs"), "pub fn renamed_function_now() {}\n").unwrap();
    let m = map(&p, Focus::default()).unwrap();
    assert!(m.contains("renamed_function_now") && !m.contains("original_name"), "{m}");
    fs::remove_file(p.0.join("b.rs")).unwrap();
    let m = map(&p, Focus::default()).unwrap();
    assert!(!m.contains("b.rs"), "{m}");
}

#[test]
fn limits_give_a_partial_map_not_a_wait() {
    let files: Vec<(String, String)> = (0..50).map(|i| (format!("f{i:02}.rs"), format!("pub fn function_{i}() {{}}\n"))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let p = project(&refs);
    let one = repo_map(&p.0, &Focus::default(), &Options { max_files: 1, ..Options::default() }).unwrap();
    assert_eq!(files_in(&one).len(), 1, "{one}");
    clear_cache();
    let start = Instant::now();
    let none = repo_map(&p.0, &Focus::default(), &Options { time_limit: Duration::ZERO, ..Options::default() });
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(none.is_none() || files_in(none.as_deref().unwrap()).len() < 50);
}

#[test]
fn unicode_paths_and_names_work() {
    let p = project(&[
        ("módulo/datos.py", "def calcular_año_fiscal():\n    pass\n"),
        ("main.py", "from módulo.datos import calcular_año_fiscal\ncalcular_año_fiscal()\n"),
    ]);
    let m = map(&p, focus(&["main.py"], &[])).unwrap();
    assert!(m.contains("módulo/datos.py:\n  1-2 def calcular_año_fiscal():"), "{m}");
}

// ---- PageRank ----

fn link(links: &mut BTreeMap<(usize, usize, &str), f64>, from: usize, to: usize, w: f64) {
    links.insert((from, to, "x"), w);
}

#[test]
fn pagerank_without_links_is_the_start() {
    let start = [0.5, 0.25, 0.25];
    let r = pagerank(3, &BTreeMap::new(), &start);
    for (a, b) in r.iter().zip(start) {
        assert!((a - b).abs() < 1e-9);
    }
}

#[test]
fn pagerank_flows_along_links_and_sums_to_one() {
    let mut links = BTreeMap::new();
    link(&mut links, 0, 1, 1.0);
    link(&mut links, 2, 1, 1.0);
    let r = pagerank(3, &links, &[1.0 / 3.0; 3]);
    assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-6, "{r:?}");
    assert!(r[1] > r[0] && r[1] > r[2], "the linked-to file ranks highest: {r:?}");

    // Heavier links carry more.
    let mut links = BTreeMap::new();
    link(&mut links, 0, 1, 9.0);
    link(&mut links, 0, 2, 1.0);
    let r = pagerank(3, &links, &[1.0, 0.0, 0.0]);
    assert!(r[1] > 5.0 * r[2], "{r:?}");
}

#[test]
fn pagerank_handles_cycles_and_dead_ends() {
    let mut links = BTreeMap::new();
    link(&mut links, 0, 1, 1.0);
    link(&mut links, 1, 0, 1.0);
    // 2 links nowhere; 3 only to 2.
    link(&mut links, 3, 2, 1.0);
    let r = pagerank(4, &links, &[0.25; 4]);
    assert!(r.iter().all(|x| x.is_finite() && *x >= 0.0));
    assert!((r.iter().sum::<f64>() - 1.0).abs() < 1e-6, "{r:?}");
}

// ---- the focus of a request ----

#[test]
fn focus_finds_the_files_a_request_names() {
    let p = project(&[("src/lib/chatStore.ts", "x"), ("README.md", "x"), ("src/a b.rs", "x")]);
    let root = p.0.to_string_lossy().to_string();
    let f = focus_from(
        &p.0,
        "Fix `src/lib/chatStore.ts:42` and ./README.md, not src/missing.ts or ../etc/passwd. Also see src/lib/chatStore.ts again.",
        &[format!("{root}/README.md"), "src/lib/chatStore.ts".into(), "gone.rs".into()],
    );
    assert_eq!(f.files, ["README.md", "src/lib/chatStore.ts"], "touched first, each once, only existing files");
}

#[test]
fn focus_words_are_identifier_like_and_unique() {
    let p = project(&[]);
    let f = focus_from(&p.0, "Why does parseConfig() call load_file twice? parseConfig! a an 42x _private", &[]);
    for w in ["parseConfig", "load_file", "Why", "does", "call", "twice", "_private"] {
        assert!(f.words.contains(&w.to_string()), "{w}: {:?}", f.words);
    }
    assert!(!f.words.contains(&"an".to_string()) && !f.words.contains(&"42x".to_string()));
    assert_eq!(f.words.iter().filter(|w| *w == "parseConfig").count(), 1);
}

#[test]
fn focus_of_an_empty_request() {
    let p = project(&[]);
    let f = focus_from(&p.0, "", &[]);
    assert!(f.files.is_empty() && f.words.is_empty());
}

// ---- graph queries (neighbours / callers) ----

#[test]
fn neighbours_show_what_a_file_uses_and_what_uses_it() {
    let p = two_parts();
    let n = neighbours(&p.0, "billing/invoice.rs", &Options::default()).unwrap();
    assert!(n.contains("## Neighbours of billing/invoice.rs"), "{n}");
    assert!(n.contains("billing/tax.rs"), "{n}");
    assert!(n.contains("compute_tax_total"), "{n}");
    assert!(!n.contains("chat/"), "{n}");
    // A suffix finds the file.
    let n = neighbours(&p.0, "invoice.rs", &Options::default()).unwrap();
    assert!(n.contains("## Neighbours of billing/invoice.rs"), "{n}");
}

#[test]
fn neighbours_of_unknown_or_ambiguous_paths_suggest() {
    let p = two_parts();
    let err = neighbours(&p.0, "billing/missing.rs", &Options::default()).unwrap_err();
    assert!(err.contains("not a source file"), "{err}");
    // A unique suffix resolves.
    let n = neighbours(&p.0, "message.rs", &Options::default()).unwrap();
    assert!(n.contains("## Neighbours of chat/message.rs"), "{n}");
    // Two files with the same name: ambiguous.
    let q = project(&[
        ("a/store.ts", "export function loadStore() {}\n"),
        ("b/store.ts", "export function saveStore() {}\n"),
        ("c/app.ts", "import { loadStore } from '../a/store';\nloadStore();\n"),
    ]);
    let err = neighbours(&q.0, "store.ts", &Options::default()).unwrap_err();
    assert!(err.contains("several files") && err.contains("a/store.ts") && err.contains("b/store.ts"), "{err}");
}

#[test]
fn callers_show_declarers_and_users() {
    let p = two_parts();
    let c = callers(&p.0, "compute_tax_total", &Options::default()).unwrap();
    assert!(c.contains("## Callers of `compute_tax_total`"), "{c}");
    assert!(c.contains("billing/tax.rs"), "{c}");
    assert!(c.contains("billing/invoice.rs"), "{c}");
    assert!(!c.contains("chat/"), "{c}");
}

#[test]
fn callers_of_unknown_names_suggest() {
    let p = two_parts();
    let err = callers(&p.0, "no_such_thing_xyz", &Options::default()).unwrap_err();
    assert!(err.contains("no declaration"), "{err}");
    let err = callers(&p.0, "", &Options::default()).unwrap_err();
    assert!(err.contains("empty"), "{err}");
}

// ---- mid-run refresh: cache invalidation ----

#[test]
fn invalidate_path_forgets_one_cached_file() {
    clear_cache();
    let p = project(&[("a.rs", "pub fn alpha_one() {}\n"), ("b.rs", "pub fn bee_two() {}\n")]);
    let _ = map(&p, Focus::default()).unwrap();
    let abs = p.0.join("a.rs");
    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let (_, files) = cache.iter().find(|(r, _)| r == &p.0).expect("this root is cached");
        assert!(files.contains_key(&abs), "a.rs is cached");
    }
    invalidate_path(&p.0, &abs);
    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let Some((_, files)) = cache.iter().find(|(r, _)| r == &p.0) else { return };
        assert!(!files.contains_key(&abs), "a.rs is forgotten");
        assert!(files.contains_key(&p.0.join("b.rs")), "b.rs is kept");
    }
    clear_cache();
}

#[test]
fn a_rewritten_file_reparses_on_the_next_map() {
    clear_cache();
    let p = project(&[("a.rs", "pub fn alpha_one() {}\n")]);
    let before = map(&p, Focus::default()).unwrap();
    assert!(before.contains("alpha_one"), "{before}");
    // A rewrite the caches would otherwise key as unchanged: invalidate as
    // the run's record() does after every write, then the new name shows.
    fs::write(p.0.join("a.rs"), "pub fn beta_two() {}\n").unwrap();
    let abs = p.0.join("a.rs");
    crate::outline::invalidate_path(&abs);
    invalidate_path(&p.0, &abs);
    let after = map(&p, Focus::default()).unwrap();
    assert!(after.contains("beta_two"), "{after}");
    assert!(!after.contains("alpha_one"), "{after}");
    clear_cache();
}

// ---- on a real project ----

#[test]
fn this_repository_maps_quickly_within_budget() {
    let root = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("src")).unwrap();
    let start = Instant::now();
    let focus = focus_from(&root, "Fix how read_file maps large files", &["read_file.rs".to_string()]);
    let m = repo_map(&root, &focus, &Options::default()).unwrap();
    assert!(start.elapsed() < Duration::from_secs(20), "{:?}", start.elapsed());
    assert!(m.chars().count() <= Options::default().max_chars);
    assert!(files_in(&m).len() >= 3, "{m}");
}

/// Prints a project's map: REPO_MAP_ROOT=dir REPO_MAP_REQUEST="…" cargo test repo_map::tests::show -- --ignored --nocapture
#[test]
#[ignore]
fn show() {
    let root = fs::canonicalize(std::env::var("REPO_MAP_ROOT").unwrap()).unwrap();
    let request = std::env::var("REPO_MAP_REQUEST").unwrap_or_default();
    let start = Instant::now();
    let focus = focus_from(&root, &request, &[]);
    let first = repo_map(&root, &focus, &Options::default());
    let cold = start.elapsed();
    let start = Instant::now();
    let _ = repo_map(&root, &focus, &Options::default());
    println!("{}", first.unwrap_or_else(|| "(no map)".into()));
    eprintln!("cold {cold:?}, cached {:?}", start.elapsed());
}

// ---- callers: qualified queries ----

fn two_parsers() -> Project {
    project(&[
        ("src/parser.rs", "pub struct Parser;\nimpl Parser {\n    pub fn parse_input(&self) {}\n}\n"),
        ("src/lexer.rs", "pub struct Lexer;\nimpl Lexer {\n    pub fn parse_input(&self) {}\n}\n"),
        ("src/main.rs", "fn main() {\n    let p = Parser;\n    p.parse_input();\n}\n"),
    ])
}

#[test]
fn qualified_callers_show_the_matching_declaration_only() {
    let p = two_parsers();
    for query in ["Parser::parse_input", "Parser > parse_input", "Parser.parse_input"] {
        let c = callers(&p.0, query, &Options::default()).unwrap();
        let declared = &c[c.find("Declared in:").unwrap()..c.find("Used by").unwrap()];
        assert!(declared.contains("src/parser.rs"), "{query}: {c}");
        assert!(!declared.contains("- src/lexer.rs"), "{query}: {c}");
        // The other `parse_input` didn't match the qualifier: said as such,
        // not as declarations cut from the list.
        assert!(declared.contains("also declared in 1 other file"), "{query}: {c}");
        assert!(!declared.contains("not shown"), "{query}: {c}");
        assert!(c.contains("src/main.rs"), "{query}: {c}");
    }
    // Unqualified: both declarations, nothing reported as left out.
    let c = callers(&p.0, "parse_input", &Options::default()).unwrap();
    assert!(c.contains("src/parser.rs") && c.contains("src/lexer.rs"), "{c}");
    assert!(!c.contains("not shown") && !c.contains("also declared"), "{c}");
}
