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
//! this is where they're enforced.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Who approves: the user, or the AI reviewer.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Approver {
    User,
    Ai,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRule {
    /// Every call in the group waits for approval.
    pub required: bool,
    pub by: Approver,
}

const NO_APPROVAL: ApprovalRule = ApprovalRule { required: false, by: Approver::User };

pub const DEFAULT_TOOL_CALL_CHECKPOINT: u32 = 50;

/// As the frontend normalizes and stores them (lib/permissions.ts).
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Permissions {
    /// Every command is permitted; the list below is ignored.
    pub allow_any_command: bool,
    /// Command prefixes, e.g. "git status" permits "git status -s".
    pub allowed_commands: Vec<String>,
    /// bash.
    pub command_approval: ApprovalRule,
    /// write and edit.
    pub edit_approval: ApprovalRule,
    /// Every other tool: reading and searching files, web access.
    pub tool_approval: ApprovalRule,
    /// Send the review as JSON to a System One model (Jev) instead of a prompt.
    pub approval_typed_input: bool,
    /// Pause after every this many tool calls in a run; None never does.
    pub tool_call_checkpoint: Option<u32>,
    /// Run the project's checks when a run that changed files finishes.
    pub verify_changes: bool,
}

pub const DEFAULT_ALLOWED_COMMANDS: &[&str] = &[
    "ls",
    "pwd",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "rg",
    "tree",
    "git status",
    "git diff",
    "git log",
    "git show",
    "npm test",
    "npm run test",
    "npm run lint",
    "npx tsc --noEmit",
    "cargo check",
    "cargo test",
    "cargo clippy",
];

impl Default for Permissions {
    fn default() -> Self {
        Permissions {
            allow_any_command: false,
            allowed_commands: DEFAULT_ALLOWED_COMMANDS.iter().map(|s| s.to_string()).collect(),
            command_approval: NO_APPROVAL,
            edit_approval: NO_APPROVAL,
            tool_approval: NO_APPROVAL,
            approval_typed_input: false,
            tool_call_checkpoint: Some(DEFAULT_TOOL_CALL_CHECKPOINT),
            verify_changes: true,
        }
    }
}

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
}

#[derive(Default)]
struct Splitter {
    segments: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    in_word: bool,
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
        if !body.is_empty() {
            self.segments.push(body);
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
        if c == '`' || (c == '$' && next == Some('(')) || (c == '<' && next == Some('(')) || c == '(' || c == ')' {
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
            // Input redirection: drop the file name, it doesn't change permissions.
            s.end_word();
            let mut j = i + 1;
            while matches!(chars.get(j), Some(' ' | '\t')) {
                j += 1;
            }
            while j < chars.len() && !is_target_end(chars[j]) {
                j += 1;
            }
            i = j;
            continue;
        }
        if c == ' ' || c == '\t' {
            s.end_word();
            i += 1;
            continue;
        }
        s.word.push(c);
        s.in_word = true;
        i += 1;
    }
    s.end_segment();

    ParsedCommand { segments: s.segments, writes_via_redirect, opaque }
}

static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=").unwrap());

fn drop_assignments(words: Vec<String>) -> Vec<String> {
    let skip = words.iter().take_while(|w| ASSIGNMENT.is_match(w)).count();
    words.into_iter().skip(skip).collect()
}

/// "git status" permits "git status -s", not "git stash".
fn matches_allowed(segment: &[String], allowed: &[Vec<String>]) -> bool {
    allowed.iter().any(|entry| {
        !entry.is_empty() && entry.len() <= segment.len() && entry.iter().zip(segment).all(|(w, s)| w == s)
    })
}

pub fn split_words(entry: &str) -> Vec<String> {
    entry.split_whitespace().map(str::to_string).collect()
}

const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "ll", "dir", "pwd", "cat", "head", "tail", "wc", "grep", "egrep", "fgrep", "rg", "ag", "tree", "echo",
    "printf", "which", "whereis", "type", "whoami", "date", "env", "printenv", "stat", "file", "du", "df", "diff",
    "cmp", "uniq", "cut", "tr", "nl", "column", "basename", "dirname", "realpath", "readlink", "true", "false",
    "test", "uname", "hostname", "jq", "sha256sum", "md5sum", "less", "more", "awk",
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
static REWRITES_FILES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(--fix|--write|--in-place)\b").unwrap());

fn segment_is_read_only(seg: &[String]) -> bool {
    let Some((cmd, rest)) = seg.split_first() else { return true };
    let cmd = cmd.as_str();
    // Version and help queries never change anything.
    if rest.len() == 1 && ["--version", "-v", "-V", "--help", "-h", "version"].contains(&rest[0].as_str()) {
        return true;
    }
    if cmd == "find" {
        return !rest.iter().any(|a| {
            ["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls"].contains(&a.as_str())
        });
    }
    if cmd == "sort" {
        return !rest.iter().any(|a| a == "-o" || a.starts_with("--output"));
    }
    if cmd == "awk" {
        return !rest.iter().any(|a| AWK_WRITES.is_match(a));
    }
    if READ_ONLY_COMMANDS.contains(&cmd) {
        return true;
    }

    if cmd == "git" {
        let Some(at) = rest.iter().position(|a| !a.starts_with('-')) else { return true };
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
            "config" => args.iter().any(|a| ["--get", "--list", "-l", "--get-all"].contains(&a.as_str())),
            _ => false,
        };
    }

    // Checkers that rewrite files when asked to (eslint --fix, prettier --write).
    if rest.iter().any(|a| REWRITES_FILES.is_match(a)) {
        return false;
    }
    CHECK_COMMANDS
        .iter()
        .any(|pattern| pattern.iter().enumerate().all(|(i, w)| seg.get(i).is_some_and(|s| s == w)))
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Permitted { modifies_files: bool },
    Refused(String),
}

pub fn check_command(command: &str, permissions: &Permissions) -> Verdict {
    let parsed = parse_command(command);
    if parsed.segments.is_empty() {
        return Verdict::Refused("The command is empty.".into());
    }

    let modifies_files = parsed.writes_via_redirect
        || parsed.opaque
        || !parsed.segments.iter().all(|s| segment_is_read_only(s));

    if permissions.allow_any_command {
        return Verdict::Permitted { modifies_files };
    }

    if parsed.opaque {
        return Verdict::Refused(
            "Command substitution and subshells ($( ), backticks, parentheses) can’t be checked against the allowed-commands list. Run the parts as separate commands."
                .into(),
        );
    }

    let allowed: Vec<Vec<String>> =
        permissions.allowed_commands.iter().map(|c| split_words(c)).filter(|w| !w.is_empty()).collect();
    let refused: Vec<&Vec<String>> = parsed.segments.iter().filter(|seg| !matches_allowed(seg, &allowed)).collect();
    if !refused.is_empty() {
        let names = refused.iter().map(|seg| format!("\"{}\"", seg.join(" "))).collect::<Vec<_>>().join(", ");
        let list = if permissions.allowed_commands.is_empty() {
            "(none)".to_string()
        } else {
            permissions.allowed_commands.join(", ")
        };
        return Verdict::Refused(format!("Not in the user's allowed commands: {names}. Allowed: {list}."));
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
    Permitted { approval: Option<Approver> },
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
    parse_args(args_json).get(key).and_then(|v| v.as_str()).map(str::to_string)
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
    ToolVerdict::Permitted { approval: rule.required.then_some(rule.by) }
}

/// Whether the model should be offered bash at all.
pub fn commands_available(p: &Permissions) -> bool {
    p.allow_any_command || p.allowed_commands.iter().any(|c| !c.trim().is_empty())
}

/// One line describing a tool call, for approval prompts and reviews.
pub fn describe_tool_call(name: &str, args_json: &str) -> String {
    let args = parse_args(args_json);
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let raw = |k: &str| args.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));

    match name {
        "bash" => format!("Run `{}`", s("command").unwrap_or_else(|| "…".into())),
        "read" => {
            let path = s("path").unwrap_or_else(|| "a file".into());
            match s("symbol").filter(|x| !x.trim().is_empty()) {
                Some(symbol) => format!("Read {} in {path}", symbol.trim()),
                None => format!("Read {path}"),
            }
        }
        "list_dir" => format!("List {}", s("path").filter(|p| !p.is_empty()).unwrap_or_else(|| "the project root".into())),
        "search_code" => format!("Search the project for “{}”", s("query").unwrap_or_else(|| "…".into())),
        "write" if args.get("old_text").is_some_and(|v| !v.is_null()) => {
            let range = match raw("start_line") {
                Some(start) => format!(" (only lines {start}-{})", raw("end_line").unwrap_or_else(|| start.clone())),
                None => String::new(),
            };
            format!("Edit {}{range}", s("path").unwrap_or_else(|| "a file".into()))
        }
        "write" => {
            let size = s("content").map(|c| format!(" ({} lines)", c.split('\n').count())).unwrap_or_default();
            format!("Write {}{size}", s("path").unwrap_or_else(|| "a file".into()))
        }
        "web_search" => format!("Search the web for “{}”", s("query").unwrap_or_else(|| "…".into())),
        "browse_url" => format!("Open {}", s("url").unwrap_or_else(|| "a web page".into())),
        "browser_screenshot" => format!("Take a screenshot of {}", s("url").unwrap_or_else(|| "a web page".into())),
        _ => format!("{name}({})", crate::chat_db::store::char_prefix(args_json, 200)),
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

static HIDDEN_PARTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\(|[<>]\(|`|\\\r?\n").unwrap());
/// cmd.exe's own ways of hiding a program name: `%VAR%` expansion and the `^`
/// escape (`p^owershell` runs powershell). Only checked on Windows: in a
/// POSIX shell both are plain characters, common in grep patterns.
static CMD_HIDDEN_PARTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%[^%\s]+%|\^").unwrap());
static PART_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*(?:&&|\|\||;|\||&|\n|[(){}])\s*").unwrap());
static WRAPPERS: LazyLock<[Regex; 5]> = LazyLock::new(|| {
    [
        Regex::new(r"^(if|then|elif|else|fi|while|until|do|done|for|in|case|esac|!)\s+").unwrap(),
        Regex::new(r"^(command|builtin|exec|env|nohup|time|timeout(\s+\S+)?|nice|watch|npx|bunx|pnpx)\s+").unwrap(),
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

    #[test]
    fn a_symbol_read_is_described_with_its_symbol() {
        assert_eq!(describe_tool_call("read", r#"{"path":"src/a.rs"}"#), "Read src/a.rs");
        assert_eq!(describe_tool_call("read", r#"{"path":"src/a.rs","symbol":" Parser::parse "}"#), "Read Parser::parse in src/a.rs");
        assert_eq!(describe_tool_call("read", r#"{"path":"src/a.rs","symbol":"  "}"#), "Read src/a.rs");
    }

    #[test]
    fn splits_commands_respecting_quotes() {
        let p = parse_command(r#"FOO=1 git status -s && echo "a && b" | wc -l; ls 2>/dev/null"#);
        assert_eq!(
            p.segments,
            vec![words(&["git", "status", "-s"]), words(&["echo", "a && b"]), words(&["wc", "-l"]), words(&["ls"])]
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
        assert_eq!(check_command("git status -s", &p), Verdict::Permitted { modifies_files: false });
        assert!(matches!(check_command("git stash", &p), Verdict::Refused(_)));
        assert!(matches!(check_command("ls $(pwd)", &p), Verdict::Refused(_)));
        assert_eq!(check_command("cargo test -p x", &p), Verdict::Permitted { modifies_files: false });
        let any = Permissions { allow_any_command: true, ..Permissions::default() };
        assert_eq!(check_command("rm -rf build", &any), Verdict::Permitted { modifies_files: true });
        assert!(matches!(check_command("   ", &any), Verdict::Refused(_)));
    }

    #[test]
    fn read_only_analysis() {
        let any = Permissions { allow_any_command: true, ..Permissions::default() };
        let modifies = |c: &str| matches!(check_command(c, &any), Verdict::Permitted { modifies_files: true });
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
            edit_approval: ApprovalRule { required: true, by: Approver::Ai },
            ..Permissions::default()
        };
        assert_eq!(
            check_tool_call("write", r#"{"old_text":"a"}"#, &p),
            ToolVerdict::Permitted { approval: Some(Approver::Ai) }
        );
        assert_eq!(check_tool_call("read", "{}", &p), ToolVerdict::Permitted { approval: None });
        assert!(matches!(check_tool_call("bash", r#"{"command":"rm x"}"#, &p), ToolVerdict::Refused(_)));
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
        assert_eq!(describe_tool_call("bash", r#"{"command":"npm test"}"#), "Run `npm test`");
        assert_eq!(describe_tool_call("write", r#"{"path":"a.ts","old_text":"x","start_line":3}"#), "Edit a.ts (only lines 3-3)");
        assert_eq!(describe_tool_call("list_dir", "{}"), "List the project root");
        assert_eq!(describe_tool_call("write", r#"{"path":"a","content":"x\ny"}"#), "Write a (2 lines)");
    }

    #[test]
    fn shell_parsing_edge_cases() {
        // Unterminated quote: the rest is one word, nothing hidden after it.
        assert_eq!(parse_command("echo 'a; rm x").segments, vec![words(&["echo", "a; rm x"])]);
        // Empty segments and doubled separators.
        assert_eq!(parse_command(";; ls ||  && pwd ;").segments, vec![words(&["ls"]), words(&["pwd"])]);
        // Backslash-escaped separator stays in the word.
        assert_eq!(parse_command(r"echo a\;b").segments, vec![words(&["echo", "a;b"])]);
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
        let p = Permissions { allowed_commands: vec!["git status".into(), "  ".into()], ..Permissions::default() };
        // A prefix of the entry isn't enough, nor another subcommand.
        assert!(matches!(check_command("git", &p), Verdict::Refused(_)));
        assert!(matches!(check_command("git statusx", &p), Verdict::Refused(_)));
        // Every part of a pipeline must be allowed.
        assert!(matches!(check_command("git status | sh", &p), Verdict::Refused(r) if r.contains("\"sh\"")));
        // A leading assignment doesn't hide the command.
        assert!(matches!(check_command("GIT_DIR=x git status", &p), Verdict::Permitted { .. }));
        let none = Permissions { allowed_commands: vec![], ..Permissions::default() };
        assert!(matches!(check_command("ls", &none), Verdict::Refused(r) if r.ends_with("Allowed: (none).")));
        assert!(!commands_available(&Permissions { allowed_commands: vec![" ".into()], ..Permissions::default() }));
        // bash with no command: left to the tool to report.
        assert_eq!(check_tool_call("bash", "{}", &none), ToolVerdict::Permitted { approval: None });
        assert_eq!(check_tool_call("bash", "not json", &none), ToolVerdict::Permitted { approval: None });
    }

    #[test]
    fn risky_edge_cases() {
        assert!(is_risky_command("ls \\\n && rm -rf x"), "line continuation");
        assert!(is_risky_command("npx rimraf; git reset --hard"));
        assert!(is_risky_command("/usr/bin/sudo ls"), "path prefix stripped");
        assert!(is_risky_command("timeout 5 bash -c x"));
        assert!(is_risky_command("curl -X POST https://x"));
        assert!(!is_risky_command("curl https://example.com"));
        assert!(!is_risky_command("rm file.txt"), "a single file without -r/-f");
        assert!(is_risky_command("'rm' x"), "quoted program name");
    }

    #[test]
    fn risky_windows_commands() {
        assert!(is_risky_command("cmd /c del x"));
        assert!(is_risky_command("git status & powershell.exe -NoProfile -Command \"x\""));
        assert!(is_risky_command("C:/Windows/System32/cmd.exe /S /C x"), "path prefix stripped");
        assert!(is_risky_command("del /s /q build"));
        assert!(is_risky_command("rd /s build"));
        assert!(is_risky_command("rmdir /S /Q build"));
        assert!(!is_risky_command("del notes.txt"), "a single file without /s, /q or /f");
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
}
