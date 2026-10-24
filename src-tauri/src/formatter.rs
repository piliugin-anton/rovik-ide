//! Command-line formatters: which one formats a file, and running it.
//!
//! Most languages have a standard formatter, and a project that uses one
//! says so with a config file (`.prettierrc`, `rustfmt.toml`,
//! `[tool.ruff]` in pyproject.toml, …) or a dependency in package.json. The
//! project's own formatter, at the project's own version (node_modules/.bin,
//! .venv), comes first: formatting with any other tool than the one its CI
//! runs only makes noisy diffs. Without one, a language's standard formatter
//! found on PATH is used, if the tool is fine to run without a config.
//!
//! Every formatter here reads the text on stdin and writes the result to
//! stdout, so the buffer is formatted as it is in the editor, unsaved edits
//! included, and nothing is written to disk.

use serde::Serialize;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A formatter that runs longer than this is stuck (or waiting on a prompt).
const TIMEOUT: Duration = Duration::from_secs(10);

/// No source file is this big; a formatter that prints more is broken.
const MAX_OUTPUT: usize = 32 * 1024 * 1024;

/// Where a project keeps its own copy of a tool.
#[derive(Clone, Copy)]
enum Local {
    None,
    /// `node_modules/.bin`.
    Node,
    /// A virtualenv: `.venv`, `venv`.
    Python,
}

/// What shows a project uses a formatter, found in a folder from the file's
/// up to the project root.
#[derive(Clone, Copy)]
enum Signal {
    File(&'static str),
    /// A file containing this text, e.g. pyproject.toml with `[tool.ruff`.
    FileWith(&'static str, &'static str),
    /// package.json with this dependency, or this top-level key.
    Package(&'static str),
}

struct Spec {
    id: &'static str,
    name: &'static str,
    /// Monaco language ids.
    languages: &'static [&'static str],
    program: &'static str,
    /// `{path}`: the file's absolute path, `{edition}`: its Rust edition.
    args: &'static [&'static str],
    local: Local,
    signals: &'static [Signal],
    /// Fine to run when the project shows no sign of it.
    fallback: bool,
    /// Exit codes that still mean "formatted" (some tools exit 1 when they
    /// changed something or when lint issues remain).
    ok_codes: &'static [i32],
}

impl Spec {
    /// A formatter: `program args` reads the file on stdin and prints it
    /// formatted. Until said otherwise, it's a standard one, fine to run
    /// without a config, installed on PATH alone, and 0 is its only success.
    const fn new(
        id: &'static str,
        name: &'static str,
        languages: &'static [&'static str],
        program: &'static str,
        args: &'static [&'static str],
    ) -> Self {
        Spec {
            id,
            name,
            languages,
            program,
            args,
            local: Local::None,
            signals: &[],
            fallback: true,
            ok_codes: &[0],
        }
    }

    /// Where the project keeps its own copy.
    const fn local(mut self, local: Local) -> Self {
        self.local = local;
        self
    }

    /// What shows the project uses it.
    const fn signals(mut self, signals: &'static [Signal]) -> Self {
        self.signals = signals;
        self
    }

    /// Only with a sign the project uses it: it needs the project's config.
    const fn declared_only(mut self) -> Self {
        self.fallback = false;
        self
    }

    /// Exit codes that still mean "formatted".
    const fn ok_codes(mut self, codes: &'static [i32]) -> Self {
        self.ok_codes = codes;
        self
    }
}

const WEB: &[&str] = &[
    "javascript",
    "typescript",
    "css",
    "scss",
    "less",
    "html",
    "json",
    "markdown",
    "mdx",
    "yaml",
    "graphql",
    "handlebars",
];

const PRETTIER_CONFIG: &[Signal] = &[
    Signal::File(".prettierrc"),
    Signal::File(".prettierrc.json"),
    Signal::File(".prettierrc.json5"),
    Signal::File(".prettierrc.yaml"),
    Signal::File(".prettierrc.yml"),
    Signal::File(".prettierrc.toml"),
    Signal::File(".prettierrc.js"),
    Signal::File(".prettierrc.cjs"),
    Signal::File(".prettierrc.mjs"),
    Signal::File(".prettierrc.ts"),
    Signal::File("prettier.config.js"),
    Signal::File("prettier.config.cjs"),
    Signal::File("prettier.config.mjs"),
    Signal::File("prettier.config.ts"),
    Signal::Package("prettier"),
];

/// In order of preference where more than one applies.
const SPECS: &[Spec] = &[
    Spec::new(
        "biome",
        "Biome",
        &["javascript", "typescript", "json", "css", "graphql"],
        "biome",
        &["format", "--stdin-file-path={path}"],
    )
    .local(Local::Node)
    .signals(&[
        Signal::File("biome.json"),
        Signal::File("biome.jsonc"),
        Signal::Package("@biomejs/biome"),
    ])
    .declared_only(),
    Spec::new(
        "dprint",
        "dprint",
        &[
            "javascript",
            "typescript",
            "json",
            "markdown",
            "ini",
            "dockerfile",
            "css",
            "scss",
            "less",
            "html",
            "yaml",
            "graphql",
        ],
        "dprint",
        &["fmt", "--stdin", "{path}"],
    )
    .local(Local::Node)
    .signals(&[
        Signal::File("dprint.json"),
        Signal::File(".dprint.json"),
        Signal::File("dprint.jsonc"),
        Signal::File(".dprint.jsonc"),
    ])
    .declared_only(),
    Spec::new(
        "prettier",
        "Prettier",
        WEB,
        "prettier",
        &["--stdin-filepath", "{path}"],
    )
    .local(Local::Node)
    .signals(PRETTIER_CONFIG),
    Spec::new(
        "rustfmt",
        "rustfmt",
        &["rust"],
        "rustfmt",
        &["--edition", "{edition}", "--emit", "stdout"],
    )
    .signals(&[Signal::File("rustfmt.toml"), Signal::File(".rustfmt.toml")]),
    Spec::new("gofmt", "gofmt", &["go"], "gofmt", &[]),
    Spec::new(
        "ruff",
        "Ruff",
        &["python"],
        "ruff",
        &["format", "--stdin-filename", "{path}", "-"],
    )
    .local(Local::Python)
    .signals(&[
        Signal::File("ruff.toml"),
        Signal::File(".ruff.toml"),
        Signal::FileWith("pyproject.toml", "[tool.ruff"),
    ]),
    Spec::new(
        "black",
        "Black",
        &["python"],
        "black",
        &["--quiet", "--stdin-filename", "{path}", "-"],
    )
    .local(Local::Python)
    .signals(&[Signal::FileWith("pyproject.toml", "[tool.black]")]),
    Spec::new(
        "clang-format",
        "clang-format",
        &["cpp", "c", "objective-c", "proto"],
        "clang-format",
        &["--assume-filename={path}"],
    )
    .signals(&[Signal::File(".clang-format"), Signal::File("_clang-format")]),
    Spec::new(
        "shfmt",
        "shfmt",
        &["shell"],
        "shfmt",
        &["--filename", "{path}"],
    ),
    Spec::new(
        "stylua",
        "StyLua",
        &["lua"],
        "stylua",
        &["--stdin-filepath", "{path}", "-"],
    )
    .signals(&[Signal::File("stylua.toml"), Signal::File(".stylua.toml")]),
    Spec::new("dart", "dart format", &["dart"], "dart", &["format"]),
    // Needs the project: plugins and settings come from .formatter.exs.
    Spec::new(
        "mix",
        "mix format",
        &["elixir"],
        "mix",
        &["format", "--stdin-filename", "{path}", "-"],
    )
    .signals(&[Signal::File(".formatter.exs")])
    .declared_only(),
    Spec::new(
        "terraform",
        "terraform fmt",
        &["hcl"],
        "terraform",
        &["fmt", "-"],
    ),
    Spec::new("tofu", "tofu fmt", &["hcl"], "tofu", &["fmt", "-"]),
    Spec::new("ktfmt", "ktfmt", &["kotlin"], "ktfmt", &["-"]),
    // 1: lint issues it couldn't fix; the text is still formatted.
    Spec::new(
        "ktlint",
        "ktlint",
        &["kotlin"],
        "ktlint",
        &["--format", "--stdin", "--log-level=none"],
    )
    .ok_codes(&[0, 1]),
    Spec::new(
        "google-java-format",
        "google-java-format",
        &["java"],
        "google-java-format",
        &["-"],
    ),
    // Refuses to run without a config naming its version.
    Spec::new(
        "scalafmt",
        "scalafmt",
        &["scala"],
        "scalafmt",
        &["--stdin", "--assume-filename", "{path}", "--quiet"],
    )
    .signals(&[Signal::File(".scalafmt.conf")])
    .declared_only(),
    Spec::new(
        "swift-format",
        "swift-format",
        &["swift"],
        "swift-format",
        &["format", "--assume-filename", "{path}"],
    )
    .signals(&[Signal::File(".swift-format")]),
    // 1: offenses it couldn't correct; the text is still corrected.
    Spec::new(
        "rubocop",
        "RuboCop",
        &["ruby"],
        "rubocop",
        &[
            "--autocorrect",
            "--stdin",
            "{path}",
            "--stderr",
            "--format",
            "quiet",
        ],
    )
    .signals(&[Signal::File(".rubocop.yml")])
    .declared_only()
    .ok_codes(&[0, 1]),
    Spec::new(
        "perltidy",
        "Perltidy",
        &["perl"],
        "perltidy",
        &["--standard-output", "--standard-error-output", "--quiet"],
    )
    .signals(&[Signal::File(".perltidyrc")]),
    // The dialect comes from the config. 1: violations it couldn't fix;
    // the fixable ones are fixed.
    Spec::new(
        "sqlfluff",
        "SQLFluff",
        &["sql", "mysql", "pgsql", "redshift"],
        "sqlfluff",
        &["format", "-"],
    )
    .local(Local::Python)
    .signals(&[
        Signal::File(".sqlfluff"),
        Signal::FileWith("pyproject.toml", "[tool.sqlfluff"),
    ])
    .declared_only()
    .ok_codes(&[0, 1]),
    Spec::new("pg_format", "pgFormatter", &["pgsql"], "pg_format", &["-"])
        .signals(&[Signal::File(".pg_format")]),
    // TOML opens as ini (lib/languageMap.ts); only .toml files qualify.
    Spec::new(
        "taplo",
        "Taplo",
        &["ini"],
        "taplo",
        &["format", "--stdin-filepath", "{path}", "-"],
    )
    .signals(&[Signal::File("taplo.toml"), Signal::File(".taplo.toml")]),
    Spec::new("yamlfmt", "yamlfmt", &["yaml"], "yamlfmt", &["-"])
        .signals(&[Signal::File(".yamlfmt"), Signal::File("yamlfmt.yml")]),
    Spec::new(
        "forge",
        "forge fmt",
        &["sol"],
        "forge",
        &["fmt", "--raw", "-"],
    )
    .signals(&[Signal::File("foundry.toml")])
    .declared_only(),
    Spec::new(
        "xmllint",
        "xmllint",
        &["xml"],
        "xmllint",
        &["--format", "-"],
    ),
];

/// TOML files open as ini (lib/languageMap.ts), but .ini and .cfg files
/// aren't TOML: formatters for ini take .toml files only.
fn handles(language: &str, path: &Path) -> bool {
    language != "ini"
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
}

/// Whether two paths name the same folder. Windows and macOS file systems
/// ignore case by default, and a drive letter can come either way (`c:` in
/// a URI, `C:` from a dialog), so case doesn't count there.
fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        let parts = |p: &Path| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
                .collect::<Vec<_>>()
        };
        parts(a) == parts(b)
    } else {
        a.components().eq(b.components())
    }
}

/// The folders to look in, from the file's up to (and including) the
/// project root, or the repository root; the file's own folder alone when
/// it's in neither. Never further up: a `node_modules/.bin` or `.venv` in
/// `/tmp`, or in a parent folder the project doesn't own, isn't the
/// project's tool, and it would run.
fn search_dirs(path: &Path, root: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let Some(parent) = path.parent() else {
        return dirs;
    };
    let root = root.filter(|r| parent.ancestors().any(|d| same_path(d, r)));
    for dir in parent.ancestors() {
        dirs.push(dir.to_path_buf());
        let at_root = match root {
            Some(r) => same_path(dir, r),
            None => dir.join(".git").exists(),
        };
        if at_root {
            return dirs;
        }
    }
    // No project or repository root on the way up.
    dirs.truncate(1);
    dirs
}

fn package_mentions(dir: &Path, name: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    json.get(name).is_some()
        || ["dependencies", "devDependencies", "peerDependencies"]
            .iter()
            .any(|k| json.get(k).and_then(|d| d.get(name)).is_some())
}

fn signal_in(dir: &Path, signal: Signal) -> bool {
    match signal {
        Signal::File(name) => dir.join(name).is_file(),
        Signal::FileWith(name, text) => std::fs::read_to_string(dir.join(name))
            .map(|c| c.contains(text))
            .unwrap_or(false),
        Signal::Package(name) => package_mentions(dir, name),
    }
}

/// The nearest folder showing the project uses `spec`.
fn declared_in(spec: &Spec, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .find(|d| spec.signals.iter().any(|s| signal_in(d, *s)))
        .cloned()
}

fn executable_names(program: &str) -> Vec<String> {
    if cfg!(windows) {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        exts.split(';')
            .filter(|e| !e.is_empty())
            .map(|e| format!("{program}{}", e.to_ascii_lowercase()))
            .collect()
    } else {
        vec![program.to_string()]
    }
}

fn find_in(dir: &Path, program: &str) -> Option<PathBuf> {
    executable_names(program)
        .into_iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// The project's own copy of the tool, else the one on PATH.
fn find_program(spec: &Spec, dirs: &[PathBuf], search_path: &OsStr) -> Option<PathBuf> {
    let bin = if cfg!(windows) { "Scripts" } else { "bin" };
    let local: Vec<PathBuf> = match spec.local {
        Local::None => Vec::new(),
        Local::Node => dirs.iter().map(|d| d.join("node_modules/.bin")).collect(),
        Local::Python => dirs
            .iter()
            .flat_map(|d| [d.join(".venv").join(bin), d.join("venv").join(bin)])
            .collect(),
    };
    local
        .iter()
        .find_map(|d| find_in(d, spec.program))
        .or_else(|| std::env::split_paths(search_path).find_map(|d| find_in(&d, spec.program)))
}

/// The edition in the nearest Cargo.toml (rustfmt formats 2015 without it).
fn rust_edition(dirs: &[PathBuf]) -> String {
    let re = regex::Regex::new(r#"(?m)^\s*edition\s*=\s*"(\d{4})""#).expect("valid regex");
    dirs.iter()
        .find_map(|d| {
            let text = std::fs::read_to_string(d.join("Cargo.toml")).ok()?;
            Some(re.captures(&text)?[1].to_string())
        })
        .unwrap_or_else(|| "2021".to_string())
}

struct Resolved {
    spec: &'static Spec,
    program: PathBuf,
    /// Where it runs: the folder that declared it, else the file's own.
    cwd: PathBuf,
    declared: bool,
}

struct Resolution {
    found: Option<Resolved>,
    /// The project's formatter, when it isn't installed.
    missing: Option<&'static Spec>,
    /// The folders looked in (search_dirs).
    dirs: Vec<PathBuf>,
}

/// `search_path`: the PATH to look for installed tools on.
fn resolve(path: &Path, language: &str, root: Option<&Path>, search_path: &OsStr) -> Resolution {
    let dirs = search_dirs(path, root);
    let file_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let candidates: Vec<&'static Spec> = SPECS
        .iter()
        .filter(|s| s.languages.contains(&language) && handles(language, path))
        .collect();

    let mut missing = None;
    for spec in &candidates {
        if let Some(dir) = declared_in(spec, &dirs) {
            match find_program(spec, &dirs, search_path) {
                Some(program) => {
                    return Resolution {
                        found: Some(Resolved {
                            spec,
                            program,
                            cwd: dir,
                            declared: true,
                        }),
                        missing: None,
                        dirs,
                    }
                }
                None => missing = missing.or(Some(*spec)),
            }
        }
    }
    // The project names a formatter that isn't installed: running another
    // one would format the file the wrong way.
    let found = if missing.is_some() {
        None
    } else {
        candidates.iter().filter(|s| s.fallback).find_map(|spec| {
            Some(Resolved {
                spec,
                program: find_program(spec, &dirs, search_path)?,
                cwd: file_dir.clone(),
                declared: false,
            })
        })
    };
    Resolution {
        found,
        missing,
        dirs,
    }
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FormatterInfo {
    id: String,
    name: String,
    /// The project's own formatter (it has a config or a dependency for it).
    declared: bool,
    /// The program that runs.
    program: String,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FormatterLookup {
    formatter: Option<FormatterInfo>,
    /// The project's formatter, by name, when it isn't installed.
    missing: Option<String>,
}

fn lookup(
    path: &Path,
    language: &str,
    root: Option<&Path>,
    search_path: &OsStr,
) -> FormatterLookup {
    let r = resolve(path, language, root, search_path);
    FormatterLookup {
        formatter: r.found.map(|f| FormatterInfo {
            id: f.spec.id.to_string(),
            name: f.spec.name.to_string(),
            declared: f.declared,
            program: f.program.to_string_lossy().into_owned(),
        }),
        missing: r.missing.map(|s| s.name.to_string()),
    }
}

/// Runs `program` with `input` on stdin; its stdout, or why it failed.
///
/// Many formatters are wrappers: `prettier.cmd` runs node through cmd.exe
/// on Windows, `mix`, `dart` and `ktlint` are shell scripts starting a VM.
/// So the tool gets its own process group (a job object on Windows), a
/// timeout kills the whole tree (command_tool::ProcessTree), and it is only
/// reaped once its output is done, so the group can't be a stranger's by
/// then (shell_exited).
fn run(
    mut cmd: Command,
    input: String,
    ok_codes: &[i32],
    timeout: Duration,
) -> Result<String, String> {
    use rovik_core::command_tool::{shell_exited, ProcessTree};

    rovik_core::process::no_window(&mut cmd);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let tree = ProcessTree::of(&child);

    let mut stdin = child.stdin.take().expect("piped stdin");
    std::thread::spawn(move || {
        // A formatter that exits early (bad config) closes the pipe: the
        // error is reported from its exit status instead.
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut out);
        out
    });
    let mut stderr = child.stderr.take().expect("piped stderr");
    let err_reader = std::thread::spawn(move || {
        let mut err = Vec::new();
        let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut err);
        err
    });

    // Done when it has exited and its output has ended: something it
    // started that still holds the pipes counts as running.
    let start = Instant::now();
    let mut exited = false;
    loop {
        if !exited {
            exited = shell_exited(&mut child).map_err(|e| e.to_string())?;
        }
        if exited && reader.is_finished() && err_reader.is_finished() {
            break;
        }
        if start.elapsed() > timeout {
            tree.kill(&mut child);
            let _ = child.wait();
            return Err(format!("timed out after {} s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let out = reader.join().unwrap_or_default();
    let err = String::from_utf8_lossy(&err_reader.join().unwrap_or_default())
        .trim()
        .to_string();

    let code = status.code().unwrap_or(-1);
    if !ok_codes.contains(&code) {
        return Err(if err.is_empty() {
            format!("exited with status {code}")
        } else {
            err
        });
    }
    if out.len() > MAX_OUTPUT {
        return Err("printed more than a source file can hold".to_string());
    }
    let out = String::from_utf8(out).map_err(|_| "printed text that isn't UTF-8".to_string())?;
    // A tool that "succeeds" with nothing on stdout didn't format anything
    // (the input is never blank); saving that would empty the file.
    if out.trim().is_empty() {
        return Err(if err.is_empty() {
            "printed nothing".to_string()
        } else {
            err
        });
    }
    Ok(out)
}

fn format_text(
    path: &Path,
    language: &str,
    root: Option<&Path>,
    content: String,
    search_path: &OsStr,
) -> Result<String, String> {
    let r = resolve(path, language, root, search_path);
    let Some(f) = r.found else {
        return Err(match r.missing {
            Some(spec) => format!(
                "the project uses {}, but `{}` isn't installed",
                spec.name, spec.program
            ),
            None => "no formatter for this file".to_string(),
        });
    };
    let path_str = path.to_string_lossy();
    let args: Vec<String> = f
        .spec
        .args
        .iter()
        .map(|a| {
            let a = a.replace("{path}", &path_str);
            if a.contains("{edition}") {
                a.replace("{edition}", &rust_edition(&r.dirs))
            } else {
                a
            }
        })
        .collect();
    let mut cmd = rovik_core::process::command(&f.program);
    cmd.args(&args).current_dir(&f.cwd).env("PATH", search_path);
    // An empty file has nothing to format, and some tools reject it.
    if content.trim().is_empty() {
        return Ok(content);
    }
    run(cmd, content, f.spec.ok_codes, TIMEOUT).map_err(|e| format!("{}: {e}", f.spec.name))
}

/// The formatter for a file: the project's own, else the language's
/// standard one if installed.
#[tauri::command]
pub async fn formatter_for_file(
    path: String,
    language: String,
    root: Option<String>,
) -> Result<FormatterLookup, String> {
    // Off the main thread: the lookup reads config files up the folders
    // and probes programs on the PATH.
    crate::util::blocking(move || {
        Ok(lookup(
            Path::new(&path),
            &language,
            root.as_deref().map(Path::new),
            &crate::lsp::tool_path(),
        ))
    })
    .await
}

/// Formats `content` (the editor's text of `path`) with the file's
/// formatter. The result replaces the text; the file on disk is untouched.
#[tauri::command]
pub async fn format_file(
    path: String,
    language: String,
    root: Option<String>,
    content: String,
) -> Result<String, String> {
    crate::util::blocking(move || {
        format_text(
            Path::new(&path),
            &language,
            root.as_deref().map(Path::new),
            content,
            &crate::lsp::tool_path(),
        )
    })
    .await
}

#[cfg(test)]
mod tests;
