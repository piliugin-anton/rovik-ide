//! What the agent may do, and what needs approval first. Two layers:
//!
//!   1. Permission: may this run at all? Only `bash` is restricted,
//!      by the allowed-commands list (or "any command"). A refused call
//!      never reaches approval.
//!   2. Approval: commands, file edits and the other tool calls each have
//!      their own rule: whether approval is required, and who gives it (the
//!      user, or the AI reviewer model).
//!
//! The settings are edited and stored by the frontend (src/lib/permissions.ts,
//! which also keeps a copy of `check_command` for the settings UI's hints);
//! this is where they're enforced. They travel to the daemon too, so their
//! type is the protocol's (rovik-protocol).

use regex::Regex;
use std::sync::LazyLock;

pub use rovik_protocol::permissions::{
    ApprovalRule, Approver, ChecklistMode, Permissions, DEFAULT_ALLOWED_COMMANDS,
    DEFAULT_TOOL_CALL_CHECKPOINT,
};

/*
 * ---------------------------------------------------------------------------
 * Shell command analysis
 * ---------------------------------------------------------------------------
 *
 * Deliberately simple and conservative: a command is split into simple
 * commands at && || ; | and newlines (quotes respected); each must match the
 * allowed list. Anything that can't be checked that way (command
 * substitution, subshells) is refused unless any command is allowed. A
 * command counts as read-only only when every part is on the known
 * read-only list below; anything unknown is treated as modifying files.
 */

#[derive(Debug, PartialEq)]
pub struct ParsedCommand {
    /// Simple commands, each as words (leading VAR=value assignments removed).
    pub segments: Vec<Vec<String>>,
    /// Output redirected into a file (> or >>), other than /dev/null.
    pub writes_via_redirect: bool,
    /// Command substitution or subshells: $( ), backticks, <( ), ( ).
    pub opaque: bool,
    /// For each segment: words the shell expands (globs `*?[`, `~`, `$VAR`,
    /// braces), so its arguments aren't what's written (P2.5).
    pub expands: Vec<bool>,
    /// The files input is redirected from (`< src/lib.rs`), as written.
    pub reads_from: Vec<String>,
}

#[derive(Default)]
struct Splitter {
    segments: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    in_word: bool,
    expands: Vec<bool>,
    expanding: bool,
}

impl Splitter {
    fn end_word(&mut self) {
        if self.in_word {
            self.words.push(std::mem::take(&mut self.word));
        }
        self.word.clear();
        self.in_word = false;
    }

    fn end_segment(&mut self) {
        self.end_word();
        let body = drop_assignments(std::mem::take(&mut self.words));
        let expanding = std::mem::take(&mut self.expanding);
        if !body.is_empty() {
            self.segments.push(body);
            self.expands.push(expanding);
        }
    }
}

fn is_target_end(c: char) -> bool {
    c.is_whitespace() || ";&|<>()".contains(c)
}

pub fn parse_command(command: &str) -> ParsedCommand {
    let chars: Vec<char> = command.chars().collect();
    let mut s = Splitter::default();
    let mut quote: Option<char> = None;
    let mut writes_via_redirect = false;
    let mut opaque = false;
    let mut reads_from = Vec::new();

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' && next.is_some() {
                i += 1;
                s.word.push(chars[i]);
            } else {
                if q == '"' && (c == '`' || (c == '$' && next == Some('('))) {
                    opaque = true;
                }
                if q == '"' && c == '$' {
                    s.expanding = true;
                }
                s.word.push(c);
            }
            i += 1;
            continue;
        }

        if c == '\'' || c == '"' {
            quote = Some(c);
            s.in_word = true;
            i += 1;
            continue;
        }
        if c == '\\' {
            if let Some(n) = next {
                s.word.push(n);
                s.in_word = true;
                i += 2;
                continue;
            }
        }
        if c == '`'
            || (c == '$' && next == Some('('))
            || (c == '<' && next == Some('('))
            || c == '('
            || c == ')'
        {
            opaque = true;
            s.in_word = true;
            s.word.push(c);
            i += 1;
            continue;
        }
        if c == '&' && next == Some('>') {
            // "&>file": redirects both streams; the ">" is handled next.
            i += 1;
            continue;
        }
        if matches!(c, '&' | '|' | ';' | '\n') {
            s.end_segment();
            if (c == '&' || c == '|') && next == Some(c) {
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == '>' {
            // A descriptor number right before ">" (the 2 in "2>") isn't a word.
            if !s.word.is_empty() && s.word.chars().all(|d| d.is_ascii_digit()) {
                s.word.clear();
                s.in_word = false;
            } else {
                s.end_word();
            }
            let mut j = i + 1;
            if chars.get(j) == Some(&'>') {
                j += 1;
            }
            if chars.get(j) == Some(&'&') {
                // ">&2" duplicates a descriptor; no file is written.
                j += 1;
                while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '-') {
                    j += 1;
                }
                i = j;
                continue;
            }
            while matches!(chars.get(j), Some(' ' | '\t')) {
                j += 1;
            }
            let mut target = String::new();
            while j < chars.len() && !is_target_end(chars[j]) {
                target.push(chars[j]);
                j += 1;
            }
            if target != "/dev/null" && target.to_lowercase() != "nul" {
                writes_via_redirect = true;
            }
            i = j;
            continue;
        }
        if c == '<' {
            // Input redirection: not a word of the command; the file is
            // kept apart (it's read: round 2, P2.5). `<<` and `<<<` are
            // text, not a file.
            s.end_word();
            let text = next == Some('<');
            let mut j = i + 1;
            while chars.get(j) == Some(&'<') {
                j += 1;
            }
            while matches!(chars.get(j), Some(' ' | '\t')) {
                j += 1;
            }
            let mut target = String::new();
            while j < chars.len() && !is_target_end(chars[j]) {
                target.push(chars[j]);
                j += 1;
            }
            if !text && !target.is_empty() {
                reads_from.push(target.trim_matches(['"', '\'']).to_string());
            }
            i = j;
            continue;
        }
        if c == ' ' || c == '\t' {
            s.end_word();
            i += 1;
            continue;
        }
        if matches!(c, '*' | '?' | '[' | '~' | '{' | '$') {
            s.expanding = true;
        }
        s.word.push(c);
        s.in_word = true;
        i += 1;
    }
    s.end_segment();

    ParsedCommand {
        segments: s.segments,
        writes_via_redirect,
        opaque,
        expands: s.expands,
        reads_from,
    }
}

static ASSIGNMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=").unwrap());

fn drop_assignments(words: Vec<String>) -> Vec<String> {
    let skip = words.iter().take_while(|w| ASSIGNMENT.is_match(w)).count();
    words.into_iter().skip(skip).collect()
}

/// "git status" permits "git status -s", not "git stash".
fn matches_allowed(segment: &[String], allowed: &[Vec<String>]) -> bool {
    allowed.iter().any(|entry| {
        !entry.is_empty()
            && entry.len() <= segment.len()
            && entry.iter().zip(segment).all(|(w, s)| w == s)
    })
}

/// A wrapper's or program's name: the file name, lower case, without
/// `.exe`/`.cmd`/`.bat`.
pub(super) fn program_name(word: &str) -> String {
    let name = word
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_lowercase();
    for ext in [".exe", ".cmd", ".bat"] {
        if let Some(n) = name.strip_suffix(ext) {
            return n.to_string();
        }
    }
    name
}

/// How many words a wrapper takes before the command it runs: its name,
/// its options (with their values) and, for `timeout`, the duration.
/// None: `word` isn't a wrapper.
fn wrapper_words(name: &str, rest: &[String]) -> Option<usize> {
    // Options that take the next word as their value.
    let takes: &[&str] = match name {
        "env" => &["-u", "--unset", "-C", "--chdir", "-S", "--split-string"],
        "timeout" | "gtimeout" => &["-s", "--signal", "-k", "--kill-after"],
        "nice" => &["-n", "--adjustment"],
        "sudo" | "doas" => &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U"],
        "xargs" => &[
            "-I",
            "-n",
            "-P",
            "-L",
            "-d",
            "-E",
            "-s",
            "-a",
            "--max-args",
            "--max-procs",
            "--delimiter",
            "--arg-file",
            "--max-lines",
        ],
        // Round 3: the other wrappers that only run the rest.
        "stdbuf" => &["-i", "-o", "-e", "--input", "--output", "--error"],
        "ionice" => &["-c", "-n", "-p", "--class", "--classdata"],
        "chrt" => &["-p"],
        "taskset" => &["-c", "--cpu-list", "-p"],
        "exec" => &["-a"],
        "time" | "nohup" | "command" | "caffeinate" | "setsid" | "unbuffer" | "chronic" => &[],
        _ => return None,
    };
    let mut at = 0;
    while let Some(w) = rest.get(at) {
        if name == "env" && w.contains('=') && !w.starts_with('-') {
            at += 1;
        } else if takes.contains(&w.as_str()) {
            at += 2;
        } else if w.starts_with('-') && w.len() > 1 {
            at += 1;
        } else {
            break;
        }
    }
    if matches!(name, "timeout" | "gtimeout") {
        // The duration.
        at += 1;
    }
    Some(1 + at)
}

/// The words of a segment past the wrappers that only run the rest (P0.2):
/// `env X=1 -u Y`, `timeout 5`, `nice -n 10`, `time`, `nohup`, `exec`,
/// `command`, `xargs -n 1`, `sudo -u x`. Empty when the segment is only
/// wrappers (a bare `env` prints the environment). Every matcher of a
/// command (read-only, checks, light commands) starts from here.
pub fn unwrap_wrappers(seg: &[String]) -> &[String] {
    let mut rest = seg;
    while let Some(first) = rest.first() {
        let name = program_name(first);
        // `env -S "rm -rf x"`: the command is inside a string, so the
        // segment stays `env …`, which no list matches (round 2).
        if split_string(&name, &rest[1..]).is_some() {
            break;
        }
        match wrapper_words(&name, &rest[1..]) {
            Some(n) => rest = &rest[n.min(rest.len())..],
            None => break,
        }
    }
    rest
}

/// The command `env -S`/`--split-string` runs: its string, then the words
/// after it. None: not `env`, or no split string.
fn split_string(name: &str, rest: &[String]) -> Option<String> {
    if name != "env" {
        return None;
    }
    let mut at = 0;
    while let Some(w) = rest.get(at) {
        let found = if let Some(v) = w.strip_prefix("--split-string=") {
            Some((v.to_string(), at + 1))
        } else if w == "--split-string" {
            Some((rest.get(at + 1).cloned().unwrap_or_default(), at + 2))
        } else if let Some(cluster) = w.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            // `-S`, `-S'…'`, `-iS …`.
            // Round 3: `-0S` too (a digit option before it).
            cluster
                .find('S')
                .filter(|&i| cluster[..i].chars().all(|c| c.is_ascii_alphanumeric()))
                .map(|i| match &cluster[i + 1..] {
                    "" => (rest.get(at + 1).cloned().unwrap_or_default(), at + 2),
                    v => (v.to_string(), at + 1),
                })
        } else {
            None
        };
        if let Some((value, after)) = found {
            let tail = rest.get(after..).unwrap_or_default();
            let words = std::iter::once(value).chain(tail.iter().cloned());
            return Some(words.collect::<Vec<_>>().join(" "));
        }
        if matches!(w.as_str(), "-u" | "--unset" | "-C" | "--chdir") {
            // The option's value goes with it.
            at += 2;
        } else if (w.starts_with('-') && w.len() > 1) || w.contains('=') {
            at += 1;
        } else {
            return None;
        }
    }
    None
}

/// Whether a segment runs its command as another user (`sudo`, `doas`),
/// which is never read-only nor a check.
pub fn elevated(seg: &[String]) -> bool {
    let inner = unwrap_wrappers(seg).len();
    seg[..seg.len() - inner]
        .iter()
        .any(|w| matches!(program_name(w).as_str(), "sudo" | "doas"))
}

/// The program's name in a segment, past wrappers that run the rest
/// (`sudo`, `env X=1`, `time`, `nice`, `nohup`, `exec`, `command`): each
/// wrapper's own name comes first, so `sudo` itself can be denied.
/// Round 2: the same unwrapping as `unwrap_wrappers` (`timeout 5`,
/// `nice -n 5`, `xargs -I {}`, `sudo -u bob`).
fn programs(segment: &[String]) -> Vec<(String, &[String])> {
    let mut out = Vec::new();
    let mut rest = segment;
    while let Some(first) = rest.first() {
        let name = program_name(first);
        out.push((name.clone(), &rest[1..]));
        if split_string(&name, &rest[1..]).is_some() {
            break;
        }
        match wrapper_words(&name, &rest[1..]) {
            Some(n) => rest = &rest[n.min(rest.len())..],
            None => break,
        }
    }
    out
}

/// Whether a segment runs a denied command: its program, then the entry's
/// other words in order anywhere after it; a shell's `-c` script is
/// checked as a command of its own.
fn segment_denied(segment: &[String], denied: &[Vec<String>], depth: usize) -> bool {
    programs(segment).iter().any(|(program, args)| {
        // `env -S 'git push'`: its string is a command of its own.
        if let Some(script) = split_string(program, args) {
            if depth < 3 && command_denied(&script, denied, depth + 1) {
                return true;
            }
        }
        let shell = matches!(program.as_str(), "bash" | "sh" | "zsh" | "dash" | "fish");
        if shell && depth < 3 {
            // `-c`, `-lc`, `-ec`, `-euc` (round 3): the script is the next word.
            if let Some(at) = args
                .iter()
                .position(|a| short_cluster(a).is_some_and(|letters| letters.contains(&'c')))
            {
                if let Some(script) = args.get(at + 1) {
                    if command_denied(script, denied, depth + 1) {
                        return true;
                    }
                }
            }
        }
        denied.iter().any(|entry| {
            let Some((head, words)) = entry.split_first() else {
                return false;
            };
            if head.to_lowercase() != *program {
                return false;
            }
            // P6.10: a word of short options (`-rf`) is denied however
            // they're written (`-r -f`, `-fr`, `--recursive --force`).
            let given = short_options(program, args);
            let mut it = args.iter();
            words.iter().all(|w| match short_cluster(w) {
                Some(letters) => letters.iter().all(|l| given.contains(l)),
                None => it.any(|a| a == w),
            })
        })
    })
}

/// The letters of a word of short options (`-rf` → r, f).
fn short_cluster(word: &str) -> Option<Vec<char>> {
    let letters = word.strip_prefix('-').filter(|l| !l.starts_with('-'))?;
    (!letters.is_empty() && letters.chars().all(|c| c.is_ascii_alphabetic()))
        .then(|| letters.chars().collect())
}

/// Every short option a command was given, its long names (for `rm`) as
/// their letters.
fn short_options(program: &str, args: &[String]) -> Vec<char> {
    let mut out = Vec::new();
    for a in args {
        if let Some(letters) = short_cluster(a) {
            out.extend(letters);
        } else if program == "rm" {
            match a.as_str() {
                "--recursive" => out.extend(['r', 'R']),
                "--force" => out.push('f'),
                _ => {}
            }
        }
    }
    // `rm -R` is `rm -r`.
    if program == "rm" && out.contains(&'R') {
        out.push('r');
    }
    out
}

fn command_denied(command: &str, denied: &[Vec<String>], depth: usize) -> bool {
    let parsed = parse_command(command);
    if parsed
        .segments
        .iter()
        .any(|seg| segment_denied(seg, denied, depth))
    {
        return true;
    }
    // Inside $( ), backticks or ( ): the words in order in the text.
    parsed.opaque && {
        let text = split_words(&command.replace(['(', ')', '`', '$', ';', '&', '|'], " "));
        denied.iter().any(|entry| {
            !entry.is_empty() && text.windows(entry.len()).any(|w| w == entry.as_slice())
        })
    }
}

/// The denied entry a command runs, if any (`Permissions::denied_commands`).
pub fn denied_entry(command: &str, permissions: &Permissions) -> Option<String> {
    permissions.denied_commands.iter().find_map(|entry| {
        let words = split_words(entry);
        (!words.is_empty() && command_denied(command, std::slice::from_ref(&words), 0))
            .then(|| entry.trim().to_string())
    })
}

pub fn split_words(entry: &str) -> Vec<String> {
    entry.split_whitespace().map(str::to_string).collect()
}

const READ_ONLY_COMMANDS: &[&str] = &[
    "ls",
    "ll",
    "dir",
    "pwd",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "tree",
    "echo",
    "printf",
    "which",
    "whereis",
    "type",
    "whoami",
    "date",
    "printenv",
    "stat",
    "file",
    "du",
    "df",
    "diff",
    "cmp",
    "uniq",
    "cut",
    "tr",
    "nl",
    "column",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "true",
    "false",
    "test",
    "uname",
    "hostname",
    "jq",
    "sha256sum",
    "md5sum",
    "less",
    "more",
    "awk",
];

const READ_ONLY_GIT: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "blame",
    "describe",
    "shortlog",
    "grep",
    "reflog",
    "cat-file",
    "whatchanged",
    "name-rev",
    "merge-base",
    "count-objects",
];

/// Checks, tests, linters: they produce build output but don't edit the project's files.
const CHECK_COMMANDS: &[&[&str]] = &[
    &["cargo", "check"],
    &["cargo", "test"],
    &["cargo", "clippy"],
    &["cargo", "build"],
    &["cargo", "tree"],
    &["cargo", "metadata"],
    &["cargo", "doc"],
    &["npm", "test"],
    &["npm", "t"],
    &["npm", "run", "test"],
    &["npm", "run", "lint"],
    &["npm", "run", "build"],
    &["npm", "run", "typecheck"],
    &["npm", "ls"],
    &["npm", "outdated"],
    &["pnpm", "test"],
    &["pnpm", "run", "test"],
    &["pnpm", "run", "lint"],
    &["pnpm", "run", "build"],
    &["yarn", "test"],
    &["yarn", "lint"],
    &["yarn", "build"],
    &["npx", "tsc", "--noEmit"],
    &["tsc", "--noEmit"],
    &["npx", "vitest", "run"],
    &["npx", "jest"],
    &["npx", "eslint"],
    &["npx", "prettier", "--check"],
    &["pytest"],
    &["python", "-m", "pytest"],
    &["go", "test"],
    &["go", "vet"],
    &["go", "build"],
    &["make", "test"],
    &["make", "check"],
];

static AWK_WRITES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"system\(|>\s*""#).unwrap());
static BRANCH_CHANGES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-(d|D|m|M|c|C|f)$|^--(delete|move|copy|force)").unwrap());
static REWRITES_FILES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(--fix|--write|--in-place)\b").unwrap());

pub(super) fn segment_is_read_only(seg: &[String]) -> bool {
    // `env rm -rf x` runs `rm`; `sudo` anything changes who runs it.
    if elevated(seg) {
        return false;
    }
    let seg = unwrap_wrappers(seg);
    let Some((cmd, rest)) = seg.split_first() else {
        return true;
    };
    let cmd = cmd.as_str();
    // Version and help queries never change anything.
    if rest.len() == 1
        && ["--version", "-v", "-V", "--help", "-h", "version"].contains(&rest[0].as_str())
    {
        return true;
    }
    if cmd == "find" {
        return !rest.iter().any(|a| {
            [
                "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls",
            ]
            .contains(&a.as_str())
        });
    }
    if cmd == "sort" {
        // `-o f`, `-of`, `-ro f`, `--output=f`.
        return !rest.iter().any(|a| {
            a.starts_with("--output")
                || a.strip_prefix('-')
                    .is_some_and(|c| !c.starts_with('-') && c.contains('o'))
        });
    }
    if cmd == "uniq" {
        // `uniq in out` writes `out` (round 2, P2.5).
        return rest
            .iter()
            .filter(|a| !a.starts_with('-') || *a == "-")
            .count()
            < 2;
    }
    if cmd == "awk" {
        return !rest.iter().any(|a| AWK_WRITES.is_match(a));
    }
    if READ_ONLY_COMMANDS.contains(&cmd) {
        return true;
    }

    if cmd == "git" {
        let Some(at) = rest.iter().position(|a| !a.starts_with('-')) else {
            return true;
        };
        let sub = rest[at].as_str();
        let args = &rest[at + 1..];
        if READ_ONLY_GIT.contains(&sub) {
            return true;
        }
        let flags_only = args.iter().all(|a| a.starts_with('-'));
        return match sub {
            "branch" | "tag" => flags_only && !args.iter().any(|a| BRANCH_CHANGES.is_match(a)),
            "remote" => args.is_empty() || (args.len() == 1 && args[0] == "-v"),
            "stash" => args.first().is_some_and(|a| a == "list" || a == "show"),
            "config" => args
                .iter()
                .any(|a| ["--get", "--list", "-l", "--get-all"].contains(&a.as_str())),
            _ => false,
        };
    }

    // Checkers that rewrite files when asked to (eslint --fix, prettier --write).
    if rest.iter().any(|a| REWRITES_FILES.is_match(a)) {
        return false;
    }
    CHECK_COMMANDS.iter().any(|pattern| {
        pattern
            .iter()
            .enumerate()
            .all(|(i, w)| seg.get(i).is_some_and(|s| s == w))
    })
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Permitted { modifies_files: bool },
    Refused(String),
}

/// Whether a command may change files (the same analysis as
/// `check_command`, without the allowed-list check).
pub fn command_modifies_files(command: &str) -> bool {
    let parsed = parse_command(command);
    parsed.writes_via_redirect
        || parsed.opaque
        || !parsed.segments.iter().all(|s| segment_is_read_only(s))
}

pub fn check_command(command: &str, permissions: &Permissions) -> Verdict {
    let parsed = parse_command(command);
    if parsed.segments.is_empty() {
        return Verdict::Refused("The command is empty.".into());
    }

    let modifies_files = parsed.writes_via_redirect
        || parsed.opaque
        || !parsed.segments.iter().all(|s| segment_is_read_only(s));

    if let Some(entry) = denied_entry(command, permissions) {
        return Verdict::Refused(format!(
            "`{entry}` isn't allowed in this run. Do without it, or finish and say what's left."
        ));
    }

    if permissions.allow_any_command {
        return Verdict::Permitted { modifies_files };
    }

    if parsed.opaque {
        return Verdict::Refused(
            "Command substitution and subshells ($( ), backticks, parentheses) can’t be checked against the allowed-commands list. Run the parts as separate commands."
                .into(),
        );
    }

    let allowed: Vec<Vec<String>> = permissions
        .allowed_commands
        .iter()
        .map(|c| split_words(c))
        .filter(|w| !w.is_empty())
        .collect();
    let refused: Vec<&Vec<String>> = parsed
        .segments
        .iter()
        .filter(|seg| !matches_allowed(seg, &allowed))
        .collect();
    if !refused.is_empty() {
        let names = refused
            .iter()
            .map(|seg| format!("\"{}\"", seg.join(" ")))
            .collect::<Vec<_>>()
            .join(", ");
        let list = if permissions.allowed_commands.is_empty() {
            "(none)".to_string()
        } else {
            permissions.allowed_commands.join(", ")
        };
        return Verdict::Refused(format!(
            "Not in the user's allowed commands: {names}. Allowed: {list}."
        ));
    }

    Verdict::Permitted { modifies_files }
}

/*
 * ---------------------------------------------------------------------------
 * Tool calls
 * ---------------------------------------------------------------------------
 */

/// Tools that always change files; everything else (reads, web) doesn't.
pub fn modifies_files(tool: &str) -> bool {
    tool == "write"
}

#[derive(Debug, PartialEq)]
pub enum ToolVerdict {
    /// May run; `approval` says who has to approve first, if anyone.
    Permitted {
        approval: Option<Approver>,
    },
    Refused(String),
}

/// A tool call's arguments as an object (empty when they don't parse).
pub fn parse_args(args_json: &str) -> serde_json::Map<String, serde_json::Value> {
    if args_json.trim().is_empty() {
        return Default::default();
    }
    match serde_json::from_str::<serde_json::Value>(args_json) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => Default::default(),
    }
}

/// One string argument of a tool call.
pub fn str_arg(args_json: &str, key: &str) -> Option<String> {
    parse_args(args_json)
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// A string value, trimmed, when there's something left of it.
pub fn trimmed_str(v: Option<&serde_json::Value>) -> Option<&str> {
    v.and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// A count argument: a whole number of at least 1 (a float is floored).
pub fn count_arg(v: Option<&serde_json::Value>) -> Option<u32> {
    v.and_then(|n| n.as_f64())
        .filter(|n| n.is_finite() && *n >= 1.0)
        .map(|n| n.floor() as u32)
}

/// Permission first, then who (if anyone) has to approve.
pub fn check_tool_call(name: &str, args_json: &str, permissions: &Permissions) -> ToolVerdict {
    if name == "bash" {
        let command = str_arg(args_json, "command").unwrap_or_default();
        if command.trim().is_empty() {
            // The executor reports the missing argument.
            return ToolVerdict::Permitted { approval: None };
        }
        if let Verdict::Refused(reason) = check_command(&command, permissions) {
            return ToolVerdict::Refused(reason);
        }
    }

    let rule = if name == "bash" {
        permissions.command_approval
    } else if modifies_files(name) {
        permissions.edit_approval
    } else {
        permissions.tool_approval
    };
    ToolVerdict::Permitted {
        approval: rule.required.then_some(rule.by),
    }
}

/// Whether the model should be offered bash at all.
pub fn commands_available(p: &Permissions) -> bool {
    p.allow_any_command || p.allowed_commands.iter().any(|c| !c.trim().is_empty())
}

/// One line describing a tool call, for approval prompts and reviews.
pub fn describe_tool_call(name: &str, args_json: &str) -> String {
    let args = parse_args(args_json);
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let raw = |k: &str| {
        args.get(k).map(|v| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        })
    };

    match name {
        "bash" => format!("Run `{}`", s("command").unwrap_or_else(|| "…".into())),
        "read" => {
            let path = s("path").unwrap_or_else(|| "a file".into());
            match s("symbol").filter(|x| !x.trim().is_empty()) {
                Some(symbol) => format!("Read {} in {path}", symbol.trim()),
                None => format!("Read {path}"),
            }
        }
        "list_dir" => format!(
            "List {}",
            s("path")
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "the project root".into())
        ),
        "search_code" => format!(
            "Search the project for “{}”",
            s("query").unwrap_or_else(|| "…".into())
        ),
        "neighbours" => format!(
            "Graph neighbours of {}",
            s("path").unwrap_or_else(|| "a file".into())
        ),
        "callers" => format!("Callers of {}", s("symbol").unwrap_or_else(|| "…".into())),
        "outline" => format!("Outline {}", s("path").unwrap_or_else(|| "a file".into())),
        "diagnostics" => match s("path").filter(|p| !p.trim().is_empty()) {
            Some(p) => format!("Diagnostics for {p}"),
            None => "Diagnostics summary".to_string(),
        },
        "write" if args.get("old_text").is_some_and(|v| !v.is_null()) => {
            let range = match raw("start_line") {
                Some(start) => format!(
                    " (only lines {start}-{})",
                    raw("end_line").unwrap_or_else(|| start.clone())
                ),
                None => String::new(),
            };
            format!(
                "Edit {}{range}",
                s("path").unwrap_or_else(|| "a file".into())
            )
        }
        "write" => {
            let size = s("content")
                .map(|c| format!(" ({} lines)", c.split('\n').count()))
                .unwrap_or_default();
            format!(
                "Write {}{size}",
                s("path").unwrap_or_else(|| "a file".into())
            )
        }
        "web_search" => format!(
            "Search the web for “{}”",
            s("query").unwrap_or_else(|| "…".into())
        ),
        "browse_url" => format!("Open {}", s("url").unwrap_or_else(|| "a web page".into())),
        "browser_screenshot" => format!(
            "Take a screenshot of {}",
            s("url").unwrap_or_else(|| "a web page".into())
        ),
        _ => format!(
            "{name}({})",
            crate::chat_db::store::char_prefix(args_json, 200)
        ),
    }
}

/*
 * ---------------------------------------------------------------------------
 * Never left to the AI reviewer
 * ---------------------------------------------------------------------------
 *
 * A reviewer model only sees the text it's given: it can't tell that a
 * command reads a credential file or that a remote is production. These
 * shapes always go to the user, whatever the reviewer would say. Adapted
 * from OpenRouter's "Auto-Approve Coding Agent Permission Prompts with Jev"
 * recipe.
 */

static RISKY_COMMANDS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Hidden program names: quotes, escapes, variables, leading dashes/digits.
        r#"^(\S*['"\\=$]|[-0-9])"#,
        r"^(sudo|doas|su)\b",
        // Running a string through another shell or interpreter hides its content.
        r"^(bash|sh|zsh|fish|eval|xargs)\b",
        r"^(node|bun|deno|python3?)\s+(-\S+\s+)*(-c|-e|-p|--eval)\b",
        r"^rm\s+(-\S*[rRf]\S*\s+)+",
        r"^git\b.*\b(push|reset\s+--hard|clean\s+-\S*[fd]|branch\s+-D|checkout\s+--\s|restore\b)",
        r"^(npm|pnpm|yarn|bun|cargo)\s+publish\b",
        r"^(wrangler|vercel|fly|flyctl|netlify)\s+deploy\b|^terraform\s+(apply|destroy)\b|^kubectl\s+(delete|apply)\b",
        r"^(curl|wget)\b.*\s(-d|--data\S*|-F|--form|-T|--upload-file|-X\s*(POST|PUT|PATCH|DELETE))\b",
        r"^(chmod|chown)\s+-\S*R",
        r"(?i)\.env\b|\.ssh\b|\.aws\b|\.npmrc\b|\.netrc\b|id_rsa|credentials",
        // Windows: bash goes through cmd.exe there (command_tool.rs),
        // where these are the same shapes. Program names may carry .exe.
        r"(?i)^(cmd|powershell|pwsh|wsl|cscript|wscript|mshta|rundll32|regsvr32|start|call|invoke-expression|iex|start-process)\b",
        r"(?i)^(runas|gsudo)\b",
        r"(?i)^(del|erase)\b.*\s/[sqf]\b|^(rd|rmdir)\b.*\s/s\b",
        r"(?i)^(remove-item|ri|rm|del|rmdir|rd)\b.*\s-(r|recurse|force)\b",
        r"(?i)^(format|diskpart|bcdedit|vssadmin|takeown|icacls|set-executionpolicy)\b|^reg\s+(add|delete|import)\b",
        r"(?i)^(invoke-webrequest|iwr|invoke-restmethod|irm)\b.*\s-(method\s+(post|put|patch|delete)|body|infile)\b",
    ]
    .iter()
    // Windows ignores case in program names: `RM -rf`, `Git push`.
    .map(|p| Regex::new(&if cfg!(windows) { format!("(?i){p}") } else { p.to_string() }).unwrap())
    .collect()
});

/// Paths whose contents are secrets: edits to them always go to the user.
static SECRET_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(^|[\\/])(\.env(\.[^\\/]*)?|\.npmrc|\.netrc|id_[a-z0-9]+|[^\\/]*\.(pem|key|pfx|p12))$|(^|[\\/])(\.ssh|\.aws|\.gnupg)([\\/]|$)|credentials",
    )
    .unwrap()
});

static HIDDEN_PARTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\(|[<>]\(|`|\\\r?\n").unwrap());
/// cmd.exe's own ways of hiding a program name: `%VAR%` expansion and the `^`
/// escape (`p^owershell` runs powershell). Only checked on Windows: in a
/// POSIX shell both are plain characters, common in grep patterns.
static CMD_HIDDEN_PARTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%[^%\s]+%|\^").unwrap());
static PART_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*(?:&&|\|\||;|\||&|\n|[(){}])\s*").unwrap());
static WRAPPERS: LazyLock<[Regex; 5]> = LazyLock::new(|| {
    [
        Regex::new(r"^(if|then|elif|else|fi|while|until|do|done|for|in|case|esac|!)\s+").unwrap(),
        Regex::new(
            r"^(command|builtin|exec|env|nohup|time|timeout(\s+\S+)?|nice|watch|npx|bunx|pnpx)\s+",
        )
        .unwrap(),
        Regex::new(r#"^[A-Za-z_][A-Za-z0-9_]*=[^\s'"\\$]*\s+"#).unwrap(),
        Regex::new(r"^\\").unwrap(),
        Regex::new(r#"^[^\s'"\\$]*/"#).unwrap(),
    ]
});

/// Strips wrappers and prefixes until the program name is bare.
fn bare_command(part: &str) -> String {
    let mut current = part.to_string();
    loop {
        let mut next = current.trim().to_string();
        for re in WRAPPERS.iter() {
            next = re.replace(&next, "").into_owned();
        }
        if next == current {
            return current;
        }
        current = next;
    }
}

/// A command the AI reviewer must never approve on its own.
pub fn is_risky_command(command: &str) -> bool {
    // Substitutions and line continuations can hide anything.
    if HIDDEN_PARTS.is_match(command) || (cfg!(windows) && CMD_HIDDEN_PARTS.is_match(command)) {
        return true;
    }
    // Splitting ignores quotes on purpose: a separator inside a quoted
    // argument can only add a prompt, never hide a command.
    PART_SPLIT
        .split(command)
        .map(bare_command)
        .any(|part| RISKY_COMMANDS.iter().any(|p| p.is_match(&part)))
}

/// A file whose edits the AI reviewer must never approve on its own.
pub fn is_secret_path(path: &str) -> bool {
    SECRET_PATH.is_match(path.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(seg: &[&str]) -> Vec<String> {
        seg.iter().map(|s| s.to_string()).collect()
    }

    /// P0.2: every matcher reads a command past the same wrappers.
    #[test]
    fn wrappers_are_seen_through_by_every_matcher() {
        use crate::workload::classify::classify;
        for (command, modifies) in [
            ("env rm x", true),
            ("env sed -i s/a/b/ f", true),
            ("timeout 5 rm x", true),
            ("timeout -s KILL 5m rm x", true),
            ("nice -n 10 rm x", true),
            ("xargs -n 1 rm", true),
            ("sudo cat f", true),
            ("env -u HOME rm x", true),
            ("env FOO=1 cat f", false),
            ("time cat f", false),
            ("xargs -I {} cat", false),
            ("env", false),
        ] {
            assert_eq!(command_modifies_files(command), modifies, "{command}");
            let seg = split_words(command);
            assert_eq!(
                segment_is_read_only(&seg),
                !modifies,
                "segment_is_read_only({command})"
            );
        }
        for (command, check) in [
            ("env CI=1 timeout 60 cargo test", true),
            ("nice -n 10 cargo test", true),
            ("time npm test", true),
            ("sudo cargo test", false),
            ("env rm x", false),
        ] {
            assert_eq!(
                super::super::worker::is_check_tool(&split_words(command)),
                check,
                "is_check_tool({command})"
            );
        }
        for (wrapped, bare) in [
            ("env CI=1 cargo test", "cargo test"),
            ("timeout -s KILL 5m cargo build", "cargo build"),
            ("nice -n 10 cargo check", "cargo check"),
            ("xargs -n 1 ls", "ls"),
            ("env -u X ls", "ls"),
        ] {
            assert_eq!(classify(wrapped, None), classify(bare, None), "{wrapped}");
        }
        assert_eq!(
            unwrap_wrappers(&split_words("env -i A=1 -u B time nice -5 x y")),
            ["x", "y"]
        );
    }

    /// The cases the frontend's hints are tested with too
    /// (src/lib/permissions.test.ts): both must judge every one alike.
    #[test]
    fn shared_command_cases_agree_with_the_frontend() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../test-fixtures/command-permissions.json"
        ))
        .unwrap();
        let strings = |v: &serde_json::Value| -> Vec<String> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        };
        let default_allowed = strings(&fixture["defaultAllowed"]);
        for case in fixture["cases"].as_array().unwrap() {
            let command = case["command"].as_str().unwrap();
            let expect = &case["expect"];
            let parsed = parse_command(command);
            if let Some(segments) = expect.get("segments") {
                let want: Vec<Vec<String>> =
                    segments.as_array().unwrap().iter().map(strings).collect();
                assert_eq!(parsed.segments, want, "segments of {command:?}");
            }
            if let Some(w) = expect.get("writesViaRedirect") {
                assert_eq!(
                    parsed.writes_via_redirect,
                    w.as_bool().unwrap(),
                    "redirect in {command:?}"
                );
            }
            if let Some(o) = expect.get("opaque") {
                assert_eq!(parsed.opaque, o.as_bool().unwrap(), "opaque {command:?}");
            }
            let permissions = Permissions {
                allow_any_command: case["allowAny"].as_bool().unwrap_or(false),
                allowed_commands: case
                    .get("allowed")
                    .map(strings)
                    .unwrap_or_else(|| default_allowed.clone()),
                ..Permissions::default()
            };
            let verdict = check_command(command, &permissions);
            if let Some(p) = expect.get("permitted") {
                assert_eq!(
                    matches!(verdict, Verdict::Permitted { .. }),
                    p.as_bool().unwrap(),
                    "permitted {command:?}"
                );
            }
            if let Some(m) = expect.get("modifiesFiles") {
                match verdict {
                    Verdict::Permitted { modifies_files } => {
                        assert_eq!(
                            modifies_files,
                            m.as_bool().unwrap(),
                            "modifies files {command:?}"
                        )
                    }
                    Verdict::Refused(reason) => panic!("{command:?} refused: {reason}"),
                }
            }
        }
    }

    #[test]
    fn a_symbol_read_is_described_with_its_symbol() {
        assert_eq!(
            describe_tool_call("read", r#"{"path":"src/a.rs"}"#),
            "Read src/a.rs"
        );
        assert_eq!(
            describe_tool_call("read", r#"{"path":"src/a.rs","symbol":" Parser::parse "}"#),
            "Read Parser::parse in src/a.rs"
        );
        assert_eq!(
            describe_tool_call("read", r#"{"path":"src/a.rs","symbol":"  "}"#),
            "Read src/a.rs"
        );
    }

    #[test]
    fn splits_commands_respecting_quotes() {
        let p = parse_command(r#"FOO=1 git status -s && echo "a && b" | wc -l; ls 2>/dev/null"#);
        assert_eq!(
            p.segments,
            vec![
                words(&["git", "status", "-s"]),
                words(&["echo", "a && b"]),
                words(&["wc", "-l"]),
                words(&["ls"])
            ]
        );
        assert!(!p.writes_via_redirect && !p.opaque);
        assert!(parse_command("echo hi > out.txt").writes_via_redirect);
        assert!(!parse_command("echo hi >&2").writes_via_redirect);
        assert!(parse_command("echo $(whoami)").opaque);
        assert!(parse_command(r#"echo "$(whoami)""#).opaque);
        assert!(!parse_command("echo '$(whoami)'").opaque);
    }

    #[test]
    fn allowed_commands_are_prefixes_of_words() {
        let p = Permissions::default();
        assert_eq!(
            check_command("git status -s", &p),
            Verdict::Permitted {
                modifies_files: false
            }
        );
        assert!(matches!(
            check_command("git stash", &p),
            Verdict::Refused(_)
        ));
        assert!(matches!(
            check_command("ls $(pwd)", &p),
            Verdict::Refused(_)
        ));
        assert_eq!(
            check_command("cargo test -p x", &p),
            Verdict::Permitted {
                modifies_files: false
            }
        );
        let any = Permissions {
            allow_any_command: true,
            ..Permissions::default()
        };
        assert_eq!(
            check_command("rm -rf build", &any),
            Verdict::Permitted {
                modifies_files: true
            }
        );
        assert!(matches!(check_command("   ", &any), Verdict::Refused(_)));
    }

    #[test]
    fn read_only_analysis() {
        let any = Permissions {
            allow_any_command: true,
            ..Permissions::default()
        };
        let modifies = |c: &str| {
            matches!(
                check_command(c, &any),
                Verdict::Permitted {
                    modifies_files: true
                }
            )
        };
        assert!(!modifies("git log --oneline | head -5"));
        assert!(modifies("git commit -m x"));
        assert!(!modifies("git branch -a"));
        assert!(modifies("git branch -D old"));
        assert!(modifies("npx eslint --fix ."));
        assert!(!modifies("npx eslint ."));
        assert!(modifies("find . -delete"));
        assert!(!modifies("node --version"));
    }

    #[test]
    fn tool_calls_go_by_their_group_rule() {
        let p = Permissions {
            edit_approval: ApprovalRule {
                required: true,
                by: Approver::Ai,
            },
            ..Permissions::default()
        };
        assert_eq!(
            check_tool_call("write", r#"{"old_text":"a"}"#, &p),
            ToolVerdict::Permitted {
                approval: Some(Approver::Ai)
            }
        );
        assert_eq!(
            check_tool_call("read", "{}", &p),
            ToolVerdict::Permitted { approval: None }
        );
        assert!(matches!(
            check_tool_call("bash", r#"{"command":"rm x"}"#, &p),
            ToolVerdict::Refused(_)
        ));
    }

    #[test]
    fn risky_shapes_and_secret_paths() {
        assert!(is_risky_command("git push origin main"));
        assert!(is_risky_command("npm test && rm -rf dist"));
        assert!(is_risky_command("cat .env"));
        assert!(is_risky_command("sudo ls"));
        assert!(is_risky_command("env FOO=1 bash -c 'x'"));
        assert!(is_risky_command("python -c 'print(1)'"));
        assert!(!is_risky_command("npm test"));
        assert!(!is_risky_command("git status"));
        assert!(is_secret_path(".env.local"));
        assert!(is_secret_path("config/server.pem"));
        assert!(is_secret_path("home/.ssh/config"));
        assert!(!is_secret_path("src/environment.ts"));
    }

    #[test]
    fn describes_calls() {
        assert_eq!(
            describe_tool_call("bash", r#"{"command":"npm test"}"#),
            "Run `npm test`"
        );
        assert_eq!(
            describe_tool_call("write", r#"{"path":"a.ts","old_text":"x","start_line":3}"#),
            "Edit a.ts (only lines 3-3)"
        );
        assert_eq!(
            describe_tool_call("list_dir", "{}"),
            "List the project root"
        );
        assert_eq!(
            describe_tool_call("write", r#"{"path":"a","content":"x\ny"}"#),
            "Write a (2 lines)"
        );
    }

    #[test]
    fn shell_parsing_edge_cases() {
        // Unterminated quote: the rest is one word, nothing hidden after it.
        assert_eq!(
            parse_command("echo 'a; rm x").segments,
            vec![words(&["echo", "a; rm x"])]
        );
        // Empty segments and doubled separators.
        assert_eq!(
            parse_command(";; ls ||  && pwd ;").segments,
            vec![words(&["ls"]), words(&["pwd"])]
        );
        // Backslash-escaped separator stays in the word.
        assert_eq!(
            parse_command(r"echo a\;b").segments,
            vec![words(&["echo", "a;b"])]
        );
        // Redirects: to a file writes, from a file or to a descriptor doesn't.
        assert!(parse_command("ls >> log.txt").writes_via_redirect);
        assert!(parse_command("ls &> all.txt").writes_via_redirect);
        assert!(!parse_command("wc -l < input.txt 2>&1").writes_via_redirect);
        assert!(!parse_command("ls > NUL").writes_via_redirect);
        // Only assignments: no command.
        assert!(parse_command("FOO=1 BAR=2").segments.is_empty());
        assert!(parse_command("cat <(ls)").opaque);
        assert!(parse_command("(cd x && ls)").opaque);
    }

    #[test]
    fn allowed_list_edge_cases() {
        let p = Permissions {
            allowed_commands: vec!["git status".into(), "  ".into()],
            ..Permissions::default()
        };
        // A prefix of the entry isn't enough, nor another subcommand.
        assert!(matches!(check_command("git", &p), Verdict::Refused(_)));
        assert!(matches!(
            check_command("git statusx", &p),
            Verdict::Refused(_)
        ));
        // Every part of a pipeline must be allowed.
        assert!(
            matches!(check_command("git status | sh", &p), Verdict::Refused(r) if r.contains("\"sh\""))
        );
        // A leading assignment doesn't hide the command.
        assert!(matches!(
            check_command("GIT_DIR=x git status", &p),
            Verdict::Permitted { .. }
        ));
        let none = Permissions {
            allowed_commands: vec![],
            ..Permissions::default()
        };
        assert!(
            matches!(check_command("ls", &none), Verdict::Refused(r) if r.ends_with("Allowed: (none)."))
        );
        assert!(!commands_available(&Permissions {
            allowed_commands: vec![" ".into()],
            ..Permissions::default()
        }));
        // bash with no command: left to the tool to report.
        assert_eq!(
            check_tool_call("bash", "{}", &none),
            ToolVerdict::Permitted { approval: None }
        );
        assert_eq!(
            check_tool_call("bash", "not json", &none),
            ToolVerdict::Permitted { approval: None }
        );
    }

    #[test]
    fn risky_edge_cases() {
        assert!(is_risky_command("ls \\\n && rm -rf x"), "line continuation");
        assert!(is_risky_command("npx rimraf; git reset --hard"));
        assert!(is_risky_command("/usr/bin/sudo ls"), "path prefix stripped");
        assert!(is_risky_command("timeout 5 bash -c x"));
        assert!(is_risky_command("curl -X POST https://x"));
        assert!(!is_risky_command("curl https://example.com"));
        assert!(
            !is_risky_command("rm file.txt"),
            "a single file without -r/-f"
        );
        assert!(is_risky_command("'rm' x"), "quoted program name");
    }

    #[test]
    fn risky_windows_commands() {
        assert!(is_risky_command("cmd /c del x"));
        assert!(is_risky_command(
            "git status & powershell.exe -NoProfile -Command \"x\""
        ));
        assert!(
            is_risky_command("C:/Windows/System32/cmd.exe /S /C x"),
            "path prefix stripped"
        );
        assert!(is_risky_command("del /s /q build"));
        assert!(is_risky_command("rd /s build"));
        assert!(is_risky_command("rmdir /S /Q build"));
        assert!(
            !is_risky_command("del notes.txt"),
            "a single file without /s, /q or /f"
        );
        assert!(is_risky_command("Remove-Item -Recurse build"));
        assert!(is_risky_command("ri build -Force"));
        assert!(is_risky_command("reg delete HKCU\\Software\\x /f"));
        assert!(is_risky_command("iwr https://x -Method Post"));
        assert!(!is_risky_command("iwr https://example.com"));
        assert!(CMD_HIDDEN_PARTS.is_match("%COMSPEC% /c x"));
        assert!(CMD_HIDDEN_PARTS.is_match("p^owershell -c x"));
        assert!(!CMD_HIDDEN_PARTS.is_match("cargo test 100%"));
        assert!(is_secret_path("C:\\Users\\me\\.aws\\config"));
        assert!(is_secret_path("deploy/KEY.PEM"));
        assert!(!is_secret_path("src/keyboard.ts"));
    }

    /// Round 2 (§4.2): what the shell expands is read from the quoting,
    /// segment by segment.
    #[test]
    fn quoting_decides_expansion() {
        let expands = |c: &str| parse_command(c).expands;
        for (command, want) in [
            ("grep \"fn *\"", false),
            ("ls fn*", true),
            ("echo '$X'", false),
            ("echo \"$X\"", true),
            ("ls ~", true),
            ("ls '~'", false),
            ("ls {a,b}", true),
            ("grep -E \"a{2}\"", false),
            ("echo \\$HOME", false),
        ] {
            assert_eq!(expands(command), [want], "{command}");
        }
        assert_eq!(expands("a | b*"), [false, true]);
        assert_eq!(expands("ls * ; ls"), [true, false]);
        assert_eq!(expands("ls * && ls"), [true, false]);
    }

    /// Round 2 (§4.2): one entry per segment, empty ones skipped.
    #[test]
    fn expands_vector_aligns_with_segments() {
        for command in ["; ; ls * ; ", "| | |", "", "ls &&", "&& ls *"] {
            let p = parse_command(command);
            assert_eq!(p.expands.len(), p.segments.len(), "{command}");
        }
        assert_eq!(parse_command("; ; ls * ; ").expands, [true]);
    }

    /// Round 2 (§4.2): a wrapper with its option's value missing is
    /// nothing to run, not a panic.
    #[test]
    fn dangling_wrapper_options_dont_panic() {
        for seg in [
            vec!["env", "-u"],
            vec!["timeout"],
            vec!["timeout", "-s"],
            vec!["nice", "-n"],
            vec!["xargs", "-I"],
            vec!["sudo", "-u"],
            vec![],
        ] {
            let seg = words(&seg);
            assert!(unwrap_wrappers(&seg).is_empty(), "{seg:?}");
            let _ = programs(&seg);
            let _ = elevated(&seg);
        }
        assert!(segment_is_read_only(&[]));
    }

    /// Round 2 (§4.2): wrappers in all their forms.
    #[test]
    fn wrapper_edge_cases() {
        for (command, modifies) in [
            ("env -i A=1 cat f", false),
            ("env -- rm x", true),
            ("env -C dir rm x", true),
            ("/usr/bin/env rm x", true),
            ("timeout --signal=KILL 5 rm x", true),
            ("exec -a name rm x", true),
            ("nohup rm x &", true),
            ("xargs -P4 -I{} sh -c 'rm {}'", true),
            ("timeout 5", false),
            ("time -p cat f", false),
            ("command -v ls", false),
            ("stdbuf -oL cat f", false),
            ("stdbuf -oL rm x", true),
            ("xargs", false),
            ("find . | xargs grep foo", false),
        ] {
            assert_eq!(command_modifies_files(command), modifies, "{command}");
        }
        assert!(elevated(&split_words("doas cat f")));
        assert!(!segment_is_read_only(&split_words("doas cat f")));
    }

    /// Round 2 (§4.2): the denied list, however `rm -rf` is written or
    /// wrapped; long options are read only for rm (as the README says);
    /// an option's value is read as options too (pinned: refusing more is
    /// the safe side for a denied list).
    #[test]
    fn denied_rm_forms_and_wrappers() {
        let p = Permissions {
            allow_any_command: true,
            denied_commands: vec!["rm -rf".into(), "curl -f".into(), "grep -r".into()],
            ..Permissions::default()
        };
        let refused = |c: &str| matches!(check_command(c, &p), Verdict::Refused(_));
        for bad in [
            "rm -r -f x",
            "rm -fr x",
            "rm -Rf x",
            "rm --recursive --force x",
            "rm -r build -f",
            "env rm -rf x",
            "timeout 5 rm -rf x",
            "bash -c 'rm -rf x'",
            "xargs rm -rf",
            "grep -e -r f",
        ] {
            assert!(refused(bad), "{bad}");
        }
        for ok in ["rm -r x", "rm -f x", "curl --fail x"] {
            assert!(!refused(ok), "{ok}");
        }
    }

    /// Round 2, P0.2: `env -S` hides its command in a string, so the
    /// segment matches no list: it may change files, isn't read-only, a
    /// check nor light.
    #[test]
    fn env_split_string_is_opaque() {
        use crate::workload::classify::{classify, Class};
        for command in [
            "env -S \"rm -rf x\"",
            "env --split-string=\"rm -rf x\"",
            "env -S 'cat f'",
            "env -iS 'cat f'",
            "env -u X -S 'cat f'",
            "env -S 'cargo test'",
        ] {
            assert!(command_modifies_files(command), "{command}");
            let seg = split_words(command);
            let parsed = parse_command(command);
            assert!(!segment_is_read_only(&parsed.segments[0]), "{command}");
            assert!(
                !super::super::worker::is_check_tool(&parsed.segments[0]),
                "{command}"
            );
            assert_ne!(classify(command, None), Class::Light, "{command}");
            assert_eq!(
                unwrap_wrappers(&seg).first().map(String::as_str),
                Some("env")
            );
        }
        // Without -S, env still runs the rest.
        assert!(!command_modifies_files("env -i A=1 cat f"));
    }

    /// Round 2, P0.2: the denied list unwraps the same wrappers as every
    /// other matcher, and reads `env -S`'s string as a command.
    #[test]
    fn the_denied_list_sees_through_every_wrapper() {
        let p = Permissions {
            allow_any_command: true,
            denied_commands: vec!["git push".into(), "sudo".into()],
            ..Permissions::default()
        };
        let refused = |c: &str| matches!(check_command(c, &p), Verdict::Refused(_));
        for bad in [
            "timeout 5 git push",
            "timeout -s KILL 5m git push",
            "nice -n 5 git push",
            "xargs -I {} git push",
            "sudo -u bob git push",
            "env -u X git push",
            "env -S 'git push'",
            "env --split-string='git push origin'",
            "env -S git push",
            "/usr/bin/env -S 'sudo ls'",
            "git.exe push",
            // Round 3: a `-c` in a cluster, the other wrappers, `exec -a`.
            "bash -ec 'git push'",
            "sh -euc 'git push origin'",
            "bash -xc 'git push'",
            "stdbuf -oL git push",
            "ionice -c3 git push",
            "taskset -c 0 git push",
            "setsid git push",
            "exec -a x git push",
            "env -0S 'git push'",
        ] {
            assert!(refused(bad), "{bad}");
        }
        for ok in [
            "timeout 5 git pull",
            "env -S 'git status'",
            "nice -n 5 ls",
            "stdbuf -oL git status",
        ] {
            assert!(!refused(ok), "{ok}");
        }
    }

    #[test]
    fn denied_commands_are_refused_however_they_are_written() {
        let p = Permissions {
            allow_any_command: true,
            denied_commands: vec!["git push".into(), "sudo".into(), "rm -rf".into()],
            ..Permissions::default()
        };
        let refused = |c: &str| matches!(check_command(c, &p), Verdict::Refused(_));
        for bad in [
            "git push",
            "git  push origin main",
            "git -C . push",
            "/usr/bin/git push --force",
            "cd x && git push",
            "env GIT_TRACE=1 git push",
            "sudo ls",
            "sudo -u root cat x",
            "bash -c 'git push'",
            "sh -c \"sudo ls\"",
            "echo $(git push)",
            "rm -rf build",
            "time git push",
            // P6.10: however the options are written.
            "rm -r -f build",
            "rm -fr build",
            "rm -Rf build",
            "rm --recursive --force build",
            "rm -r build -f",
        ] {
            assert!(refused(bad), "{bad}");
        }
        for ok in [
            "git status",
            "git pull",
            "echo git push",
            "ls sudo",
            "rm build",
            "rm -r build",
            "rm -f build",
        ] {
            assert!(!refused(ok), "{ok}");
        }
        assert_eq!(
            denied_entry("git -C . push", &p).as_deref(),
            Some("git push")
        );
    }
}
