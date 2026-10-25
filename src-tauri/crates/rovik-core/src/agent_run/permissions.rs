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
use std::ops::Range;
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
    /// Simple commands, each as words (leading harmless VAR=value assignments
    /// removed: see [`drop_assignments`]).
    pub segments: Vec<Vec<String>>,
    /// The files output is redirected into (`>`, `>>`, `&>`), as written,
    /// other than /dev/null.
    pub writes_to: Vec<String>,
    /// Command substitution or subshells: $( ), backticks, <( ), ( ).
    pub opaque: bool,
    /// For each segment: words the shell expands (globs `*?[`, `~`, `$VAR`,
    /// braces), so its arguments aren't what's written (P2.5).
    pub expands: Vec<bool>,
    /// The files input is redirected from (`< src/lib.rs`), as written.
    pub reads_from: Vec<String>,
    /// For each of `writes_to`, then of `reads_from`: the segment it's
    /// part of, its index in `segments` (the length, for one after the
    /// last command), so it's read from the folder that segment runs in.
    pub writes_in: Vec<usize>,
    pub reads_in: Vec<usize>,
    /// For each segment: its output goes into the next one (`|`, `|&`; not
    /// `||`), in one pipeline.
    pub piped: Vec<bool>,
    /// Text the command gives its segments as their input: the lines of
    /// here-documents no shell reads (`cat <<'EOF'`; a shell's are its
    /// segments, [`HereDoc::script`]), and here-strings (`<<< "…"`).
    pub inputs: Vec<Input>,
    /// A POSIX shell leaves a part of it running without waiting for it: a
    /// lone `&` (not `&&`, `|&`, `&>` or `>&`) ends that part.
    pub backgrounds: bool,
}

/// Text a command gives one of its segments as its input.
#[derive(Debug, PartialEq)]
pub struct Input {
    /// The segment it's the input of: its index in `segments`.
    pub segment: usize,
    pub text: String,
}

impl ParsedCommand {
    /// Whether its output is redirected into a file.
    pub fn writes_via_redirect(&self) -> bool {
        !self.writes_to.is_empty()
    }
}

#[derive(Default)]
struct Splitter {
    segments: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    in_word: bool,
    expands: Vec<bool>,
    expanding: bool,
    piped: Vec<bool>,
}

impl Splitter {
    fn end_word(&mut self) {
        if self.in_word {
            self.words.push(std::mem::take(&mut self.word));
        }
        self.word.clear();
        self.in_word = false;
    }

    /// `piped`: what ended it pipes its output into the next segment.
    fn end_segment(&mut self, piped: bool) {
        self.end_word();
        let body = drop_assignments(std::mem::take(&mut self.words));
        let expanding = std::mem::take(&mut self.expanding);
        if !body.is_empty() {
            self.segments.push(body);
            self.expands.push(expanding);
            self.piped.push(piped);
        }
    }
}

fn is_target_end(c: char) -> bool {
    c.is_whitespace() || ";&|<>()".contains(c)
}

/// Which shell the command is read as. The agent's `bash` runs through
/// cmd.exe on Windows and a POSIX shell everywhere else (command_tool.rs),
/// and the two split a line very differently: cmd.exe knows only `"`
/// quotes (not `'`), escapes the next character with `^` (not `\`, which
/// is just a path separator), doesn't treat `;` as a separator, and
/// expands `%VAR%`. Reading a Windows command with POSIX rules let a
/// second command hide behind a `'`-quoted or `\`-escaped `&`, so the
/// allowed-commands check below never saw it; reading it only as cmd.exe
/// does let one hide from the checks behind a `;` or in `bash -c '…'`,
/// which the bash or sh cmd.exe starts reads the POSIX way. So the checks
/// read a command every way this machine may ([`Shell::readings`]), and
/// what any reading refuses is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shell {
    Posix,
    Cmd,
}

impl Shell {
    /// Every way a command is read on this machine: as cmd.exe splits it
    /// on Windows, which runs the agent's commands, and as a POSIX shell
    /// does everywhere, as the programs cmd.exe starts may (bash, sh).
    pub(crate) fn readings() -> &'static [Shell] {
        if cfg!(windows) {
            &[Shell::Posix, Shell::Cmd]
        } else {
            &[Shell::Posix]
        }
    }

    /// Whether `c` opens a quoted span. cmd.exe has only `"`.
    fn is_quote(self, c: char) -> bool {
        c == '"' || (self == Shell::Posix && c == '\'')
    }

    /// The character that takes the next one literally outside quotes:
    /// `\` in a POSIX shell, `^` in cmd.exe.
    fn escape(self) -> char {
        match self {
            Shell::Posix => '\\',
            Shell::Cmd => '^',
        }
    }

    /// Whether `;` separates commands (it doesn't in cmd.exe).
    fn semicolon_separates(self) -> bool {
        self == Shell::Posix
    }
}

/// Reads a command as a POSIX shell splits it: what it means to the tools
/// that describe it (its workload, its files, its server). The checks read
/// it every way this machine may ([`Shell::readings`]).
pub fn parse_command(command: &str) -> ParsedCommand {
    parse_command_in(command, Shell::Posix)
}

/// Reads a command as `shell` splits it.
pub(crate) fn parse_command_in(command: &str, shell: Shell) -> ParsedCommand {
    let chars: Vec<char> = command.chars().collect();
    let mut s = Splitter::default();
    let mut quote: Option<char> = None;
    let mut writes_to = Vec::new();
    let mut writes_in = Vec::new();
    let mut reads_in = Vec::new();
    let mut opaque = false;
    let mut reads_from = Vec::new();
    let mut inputs = Vec::new();
    // cmd.exe has no here-documents.
    let docs = match shell {
        Shell::Posix => here_docs(&chars),
        Shell::Cmd => Vec::new(),
    };
    // The delimiter lines of the here-documents read as commands: where
    // each starts, and where the command goes on past it.
    let mut delimiters: Vec<(usize, usize)> = Vec::new();
    let mut backgrounds = false;

    let mut i = 0;
    while i < chars.len() {
        // A here-document's lines are its command's input, not commands,
        // unless a shell reads them (but the `$(…)` in them run).
        if let Some(doc) = docs.iter().find(|d| d.body.start == i) {
            if doc.script && !doc.nested && quote.is_none() {
                delimiters.push((doc.body.end, doc.end));
            } else {
                opaque |= !doc.literal && runs_commands(&chars[doc.body.clone()]);
                i = doc.end;
                continue;
            }
        }
        if let Some(&(_, end)) = delimiters.iter().find(|(start, _)| *start == i) {
            i = end;
            continue;
        }
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if shell == Shell::Posix && c == '\\' && q == '"' && next.is_some() {
                i += 1;
                s.word.push(chars[i]);
            } else {
                if shell == Shell::Posix
                    && q == '"'
                    && (c == '`' || (c == '$' && next == Some('(')))
                {
                    opaque = true;
                }
                if shell == Shell::Posix && q == '"' && c == '$' {
                    s.expanding = true;
                }
                // cmd.exe expands `%VAR%` even inside double quotes, so a
                // program name can hide there.
                if shell == Shell::Cmd && c == '%' {
                    opaque = true;
                    s.expanding = true;
                }
                s.word.push(c);
            }
            i += 1;
            continue;
        }

        if shell.is_quote(c) {
            quote = Some(c);
            s.in_word = true;
            i += 1;
            continue;
        }
        if c == shell.escape() {
            if let Some(n) = next {
                s.word.push(n);
                s.in_word = true;
                i += 2;
                continue;
            }
        }
        // A comment, to the end of its line: not run.
        if shell == Shell::Posix && c == '#' && !s.in_word {
            while chars.get(i + 1).is_some_and(|&c| c != '\n') {
                i += 1;
            }
            i += 1;
            continue;
        }
        let subshell = match shell {
            Shell::Posix => {
                c == '`'
                    || (c == '$' && next == Some('('))
                    || (c == '<' && next == Some('('))
                    || c == '('
                    || c == ')'
            }
            // cmd.exe has no `$(`/backtick substitution; `(` `)` group.
            Shell::Cmd => c == '(' || c == ')',
        };
        if subshell {
            opaque = true;
            s.in_word = true;
            s.word.push(c);
            i += 1;
            continue;
        }
        if shell == Shell::Posix && c == '&' && next == Some('>') {
            // "&>file": redirects both streams; the ">" is handled next.
            i += 1;
            continue;
        }
        if c == '&' || c == '|' || c == '\n' || (shell.semicolon_separates() && c == ';') {
            // The `&` of a `|&` comes here after its `|`.
            let after_pipe = i > 0 && chars[i - 1] == '|';
            if shell == Shell::Posix && c == '&' && next != Some('&') && !after_pipe {
                backgrounds = true;
            }
            s.end_segment(c == '|' && next != Some('|'));
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
            // `>>` appends; `>|` writes where noclobber would refuse to.
            if matches!(chars.get(j), Some('>' | '|')) {
                j += 1;
            }
            let duplicates = chars.get(j) == Some(&'&');
            if duplicates {
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
            // ">&2", ">&-" duplicate or close a descriptor; ">&file" (bash)
            // writes both streams to the file.
            let descriptor = duplicates
                && !target.is_empty()
                && target.chars().all(|d| d.is_ascii_digit() || d == '-');
            if !descriptor && target != "/dev/null" && target.to_lowercase() != "nul" {
                writes_to.push(target.trim_matches(['"', '\'']).to_string());
                writes_in.push(s.segments.len());
            }
            i = j;
            continue;
        }
        if c == '<' {
            // Input redirection: not a word of the command; the file is
            // kept apart (it's read: round 2, P2.5). `<<` and `<<<` are
            // text, not a file: the command's input.
            s.end_word();
            if let Some(doc) = docs.iter().find(|d| d.at == i) {
                if !doc.nested && !doc.script {
                    inputs.push(Input {
                        segment: s.segments.len(),
                        text: chars[doc.body.clone()].iter().collect(),
                    });
                }
                i = doc.after;
                continue;
            }
            let text = next == Some('<');
            let mut j = i + 1;
            while chars.get(j) == Some(&'<') {
                j += 1;
            }
            while matches!(chars.get(j), Some(' ' | '\t')) {
                j += 1;
            }
            if shell == Shell::Posix && j > i + 2 && chars[i + 2] == '<' {
                // A here-string: its word, as the shell reads it.
                let (word, end, _) = shell_word(&chars, j);
                opaque |= runs_commands(&chars[j..end]);
                inputs.push(Input {
                    segment: s.segments.len(),
                    text: word,
                });
                i = end;
                continue;
            }
            let mut target = String::new();
            while j < chars.len() && !is_target_end(chars[j]) {
                target.push(chars[j]);
                j += 1;
            }
            if !text && !target.is_empty() {
                reads_from.push(target.trim_matches(['"', '\'']).to_string());
                reads_in.push(s.segments.len());
            }
            i = j;
            continue;
        }
        if c == ' ' || c == '\t' {
            s.end_word();
            i += 1;
            continue;
        }
        match shell {
            Shell::Posix if matches!(c, '*' | '?' | '[' | '~' | '{' | '$') => {
                s.expanding = true;
            }
            // cmd.exe expands `%VAR%`, which can hide a program name.
            Shell::Cmd if c == '%' => {
                s.expanding = true;
                opaque = true;
            }
            _ => {}
        }
        s.word.push(c);
        s.in_word = true;
        i += 1;
    }
    s.end_segment(false);

    ParsedCommand {
        segments: s.segments,
        writes_to,
        opaque,
        expands: s.expands,
        reads_from,
        writes_in,
        reads_in,
        piped: s.piped,
        inputs,
        backgrounds,
    }
}

/// Programs that run the lines they read as commands: a here-document one
/// reads is a script (`bash <<'EOF'`, `cat <<'EOF' | sh`).
pub(super) const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "ash", "mksh", "yash", "fish", "source", ".",
];

/// Shell keywords that come before a command (`then cat x`).
pub(super) const KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "do", "while", "until", "!", "{", "}", "fi", "done", "esac",
];

/// A here-document: lines after its command's line that the shell gives
/// the command as its input (`cat <<'EOF'` … `EOF`), which it doesn't run.
/// Places are indexes into the command's chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HereDoc {
    /// Where its `<<` is.
    pub at: usize,
    /// Past its delimiter's word: where its command's line goes on.
    pub after: usize,
    /// Its lines: from the line after its command's to its delimiter's.
    pub body: Range<usize>,
    /// Past its delimiter's line: where the command goes on.
    pub end: usize,
    /// Its delimiter is quoted (`<<'EOF'`, `<<"EOF"`, `<<\EOF`): its lines
    /// are taken as written. Otherwise the shell runs the `$(…)` and
    /// `` `…` `` in them, and their quotes are text.
    pub literal: bool,
    /// A shell may read it ([`SHELLS`]): its lines are commands.
    pub script: bool,
    /// It's in a `$(…)` or `` `…` ``: the input of a command in there.
    pub nested: bool,
}

/// An `<<` whose lines are still to come.
struct Pending {
    at: usize,
    after: usize,
    delimiter: Vec<char>,
    /// `<<-`: the tabs before its lines' text don't count.
    tabs: bool,
    literal: bool,
    /// Where the command it's part of starts: the line's start, or its
    /// `$(`'s.
    line: usize,
    nested: bool,
}

/// A command's here-documents, as a POSIX shell reads them: each `<<` in
/// its code (not quoted, nor a shift in `$((…))`), whose lines start on the
/// line after the one it's on, and end at the line that is its delimiter
/// (with `<<-`, past the tabs before it) or at the command's end; in
/// `$(…)`, the delimiter right before its `)` ends them too.
pub(crate) fn here_docs(chars: &[char]) -> Vec<HereDoc> {
    /// What the text being read is.
    #[derive(Clone, Copy, PartialEq)]
    enum Frame {
        /// The command's code, or a `` `…` ``'s.
        Code,
        Backtick,
        /// The code of a `$(…)`, `<(…)` or `>(…)`, with the `(`s opened
        /// in it.
        Paren(usize),
        Double,
        Single,
        /// `$((…))`, where `<<` shifts, with the `(`s opened in it.
        Arith(usize),
    }
    let mut docs = Vec::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut frames = vec![Frame::Code];
    let mut line = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let frame = frames.last().copied().unwrap_or(Frame::Code);
        let top = frames.len() - 1;
        // `$(` opens code, `$((` arithmetic; in code and in double quotes.
        if c == '$' && next == Some('(') && !matches!(frame, Frame::Single) {
            if chars.get(i + 2) == Some(&'(') {
                frames.push(Frame::Arith(0));
                i += 3;
            } else {
                frames.push(Frame::Paren(0));
                i += 2;
                line = i;
            }
            continue;
        }
        match frame {
            Frame::Single => {
                if c == '\'' {
                    frames.pop();
                }
                i += 1;
                continue;
            }
            Frame::Double => {
                match c {
                    '\\' => i += 1,
                    '"' => {
                        frames.pop();
                    }
                    '`' => {
                        frames.push(Frame::Backtick);
                        line = i + 1;
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            Frame::Arith(depth) => {
                match c {
                    '(' => frames[top] = Frame::Arith(depth + 1),
                    ')' if depth > 0 => frames[top] = Frame::Arith(depth - 1),
                    // Its `))`.
                    ')' => {
                        frames.pop();
                        i += 1;
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            Frame::Code | Frame::Backtick | Frame::Paren(_) => {}
        }
        match c {
            '\\' => i += 1,
            '\'' => frames.push(Frame::Single),
            '"' => frames.push(Frame::Double),
            '`' if frame == Frame::Backtick => {
                frames.pop();
            }
            '`' => {
                frames.push(Frame::Backtick);
                line = i + 1;
            }
            '<' | '>' if next == Some('(') => {
                frames.push(Frame::Paren(0));
                i += 2;
                line = i;
                continue;
            }
            // `((…))`: arithmetic too.
            '(' if next == Some('(') => {
                frames.push(Frame::Arith(0));
                i += 2;
                continue;
            }
            '(' => {
                if let Frame::Paren(depth) = frame {
                    frames[top] = Frame::Paren(depth + 1);
                }
            }
            ')' => match frame {
                Frame::Paren(0) => {
                    frames.pop();
                }
                Frame::Paren(depth) => frames[top] = Frame::Paren(depth - 1),
                _ => {}
            },
            // A comment, to the end of its line.
            '#' if starts_comment(chars, i) => {
                while chars.get(i + 1).is_some_and(|&c| c != '\n') {
                    i += 1;
                }
            }
            '<' if next == Some('<') && chars.get(i + 2) == Some(&'<') => i += 2,
            '<' if next == Some('<') => {
                let mut j = i + 2;
                let tabs = chars.get(j) == Some(&'-');
                if tabs {
                    j += 1;
                }
                while matches!(chars.get(j), Some(' ' | '\t')) {
                    j += 1;
                }
                let (delimiter, after, literal) = shell_word(chars, j);
                if !delimiter.is_empty() {
                    pending.push(Pending {
                        at: i,
                        after,
                        delimiter: delimiter.chars().collect(),
                        tabs,
                        literal,
                        line,
                        nested: frames.len() > 1,
                    });
                }
                i = after.max(i + 2);
                continue;
            }
            '\n' => {
                let in_paren = matches!(frame, Frame::Paren(_));
                i = read_bodies(chars, i, &mut pending, &mut docs, in_paren);
                line = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    // An `<<` on the last line: its lines are none.
    read_bodies(chars, chars.len(), &mut pending, &mut docs, false);
    docs
}

/// Reads the lines of the `pending` here-documents, in order, after the
/// line that ends at `newline`: each ends at its delimiter's line. Returns
/// where the command goes on.
fn read_bodies(
    chars: &[char],
    newline: usize,
    pending: &mut Vec<Pending>,
    docs: &mut Vec<HereDoc>,
    in_paren: bool,
) -> usize {
    let mut i = (newline + 1).min(chars.len());
    for doc in pending.drain(..) {
        let start = i;
        let (body_end, end) = loop {
            if i >= chars.len() {
                break (chars.len(), chars.len());
            }
            let line_end = chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |n| i + n);
            let line = &chars[i..line_end];
            let tabs = if doc.tabs {
                line.iter().take_while(|&&c| c == '\t').count()
            } else {
                0
            };
            let text = &line[tabs..];
            if text == doc.delimiter.as_slice() {
                break (i, (line_end + 1).min(chars.len()));
            }
            // `EOF)`: the `)` closes the `$(` it's in.
            if in_paren
                && text.starts_with(&doc.delimiter)
                && text.get(doc.delimiter.len()) == Some(&')')
            {
                break (i, i + tabs + doc.delimiter.len());
            }
            i = line_end + 1;
        };
        let line: String = chars[doc.line.min(newline)..newline].iter().collect();
        docs.push(HereDoc {
            at: doc.at,
            after: doc.after,
            body: start..body_end,
            end,
            literal: doc.literal,
            script: feeds_a_shell(&line),
            nested: doc.nested,
        });
        i = end;
    }
    i.max((newline + 1).min(chars.len()))
}

/// Whether one of a line's commands is a shell ([`SHELLS`]), past the
/// assignments, keywords and wrappers before it: one may run the lines of
/// a here-document on that line.
fn feeds_a_shell(line: &str) -> bool {
    line.split(['|', ';', '&', '(', ')', '`']).any(|part| {
        let words: Vec<String> = part
            .split_whitespace()
            .map(|w| w.trim_matches(['"', '\'']).to_string())
            .skip_while(|w| is_assignment(w) || KEYWORDS.contains(&w.as_str()))
            .collect();
        unwrap_wrappers(&words)
            .first()
            .is_some_and(|w| SHELLS.contains(&program_name(w).as_str()))
    })
}

/// The word at `from`, as the shell reads it (`'EOF'`, `"E O F"`, `\EOF`,
/// `EOF`): its text without its quotes, where it ends, and whether any of
/// it is quoted.
fn shell_word(chars: &[char], from: usize) -> (String, usize, bool) {
    let (mut word, mut quoted, mut i) = (String::new(), false, from);
    while let Some(&c) = chars.get(i) {
        match c {
            '\'' | '"' => {
                quoted = true;
                i += 1;
                while let Some(&d) = chars.get(i) {
                    i += 1;
                    if d == c {
                        break;
                    }
                    let escaped = chars
                        .get(i)
                        .filter(|&&e| matches!(e, '"' | '\\' | '$' | '`'));
                    match escaped {
                        Some(&e) if c == '"' && d == '\\' => {
                            word.push(e);
                            i += 1;
                        }
                        _ => word.push(d),
                    }
                }
            }
            '\\' => {
                quoted = true;
                word.extend(chars.get(i + 1));
                i += 2;
            }
            c if c.is_whitespace() || ";&|<>()".contains(c) => break,
            c => {
                word.push(c);
                i += 1;
            }
        }
    }
    (word, i.min(chars.len()), quoted)
}

/// Whether the `#` at `i` starts a comment: it starts a word.
pub(super) fn starts_comment(chars: &[char], i: usize) -> bool {
    chars[i] == '#' && (i == 0 || chars[i - 1].is_whitespace() || ";&|()".contains(chars[i - 1]))
}

/// Whether text the shell expands runs commands in it: `$(…)`, `` `…` ``.
fn runs_commands(text: &[char]) -> bool {
    text.iter()
        .enumerate()
        .any(|(at, &c)| c == '`' || (c == '$' && text.get(at + 1) == Some(&'(')))
}

/// `command` without the lines of its here-documents that aren't commands
/// (the ones no shell reads), nor any delimiter's line; and those lines.
pub(crate) fn split_here_text(command: &str) -> (String, Vec<String>) {
    let chars: Vec<char> = command.chars().collect();
    let docs = here_docs(&chars);
    if docs.is_empty() {
        return (command.to_string(), Vec::new());
    }
    let (mut code, mut texts) = (String::new(), Vec::new());
    let mut i = 0;
    while i < chars.len() {
        if let Some(doc) = docs.iter().find(|d| d.body.start == i && !d.script) {
            texts.push(chars[doc.body.clone()].iter().collect());
            i = doc.end;
        } else if let Some(doc) = docs.iter().find(|d| d.script && d.body.end == i) {
            i = doc.end;
        } else {
            code.push(chars[i]);
            i += 1;
        }
    }
    (code, texts)
}

/// Whether a word starts with an assignment (`KEY=…`).
pub(super) fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
            && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    })
}

/// Variables a leading `NAME=value` may set without changing which program
/// runs or what it runs: locale, colour, logging and CI flags. Any other
/// variable can (`GIT_EXTERNAL_DIFF`, `LD_PRELOAD`, `PAGER`, `NODE_OPTIONS`,
/// `PATH`…), so it stays part of the command it prefixes.
const HARMLESS_VARIABLES: &[&str] = &[
    "CI",
    "NO_COLOR",
    "FORCE_COLOR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "CARGO_TERM_COLOR",
    "TERM",
    "COLUMNS",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_COLLATE",
    "TZ",
    "RUST_LOG",
    "RUST_BACKTRACE",
    "PYTHONUNBUFFERED",
    "PYTHONDONTWRITEBYTECODE",
];

/// A plain value: no path (a locale or terminal name with `/` loads that
/// file), quotes or expansions.
static PLAIN_VALUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.,:@+-]*$").unwrap());

/// Whether `word` is a `NAME=value` assignment that can't change what runs:
/// a variable on [`HARMLESS_VARIABLES`] set to a plain value.
fn harmless_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, value)| {
        HARMLESS_VARIABLES.contains(&name) && PLAIN_VALUE.is_match(value)
    })
}

/// Drops leading harmless assignments (`RUST_LOG=debug cargo test` is
/// checked as `cargo test`). Any other assignment is kept, so the segment
/// matches an allowed entry only if that entry names it: an allowed
/// `git diff` doesn't permit `GIT_EXTERNAL_DIFF=./x git diff`, which runs
/// `./x`.
fn drop_assignments(words: Vec<String>) -> Vec<String> {
    let skip = words.iter().take_while(|w| harmless_assignment(w)).count();
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

/// xargs's options that take the next word as their value.
pub(super) const XARGS_VALUES: &[&str] = &[
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
];

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
        "xargs" => XARGS_VALUES,
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
        } else if takes.contains(&w.as_str()) || ends_in_value(w, takes) {
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

/// Whether `word`, a cluster of short options, ends with the first of
/// them that takes a value, so the value is the next word (`xargs -0a
/// list`, `sudo -iu bob`): one before the end takes the cluster's rest
/// (`-n1`).
fn ends_in_value(word: &str, takes: &[&str]) -> bool {
    let Some(letters) = word.strip_prefix('-').filter(|l| !l.starts_with('-')) else {
        return false;
    };
    letters
        .char_indices()
        .find(|(_, c)| takes.contains(&format!("-{c}").as_str()))
        .is_some_and(|(at, c)| at + c.len_utf8() == letters.len())
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

/// The commands a segment runs as text of their own: `env -S`'s string,
/// a shell's `-c` script.
pub(super) fn inner_scripts(segment: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (program, args) in programs(segment) {
        // `env -S 'git push'`: its string is a command of its own.
        if let Some(script) = split_string(&program, args) {
            out.push(script);
        }
        let shell = matches!(program.as_str(), "bash" | "sh" | "zsh" | "dash" | "fish");
        if shell {
            // `-c`, `-lc`, `-ec`, `-euc` (round 3): the script is the next word.
            if let Some(at) = args
                .iter()
                .position(|a| short_cluster(a).is_some_and(|letters| letters.contains(&'c')))
            {
                if let Some(script) = args.get(at + 1) {
                    out.push(script.clone());
                }
            }
        }
    }
    out
}

/// Whether a segment runs a denied command: its program, then the entry's
/// other words in order anywhere after it; a shell's `-c` script is
/// checked as a command of its own, read as that POSIX shell reads it.
fn segment_denied(segment: &[String], denied: &[Vec<String>], depth: usize) -> bool {
    if depth < 3
        && inner_scripts(segment)
            .iter()
            .any(|script| command_denied(script, denied, depth + 1, Shell::Posix))
    {
        return true;
    }
    programs(segment).iter().any(|(program, args)| {
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

fn command_denied(command: &str, denied: &[Vec<String>], depth: usize, shell: Shell) -> bool {
    let parsed = parse_command_in(command, shell);
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

/// Whether a command pipes what runs `parts[0]` into what runs
/// `parts[1]`, and so on, in one pipeline (`curl -fsSL x | sudo sh`), in a
/// shell's own script too; or runs the last part on what a substitution
/// running the one before it puts out (`sh -c "$(curl -fsSL x)"`,
/// `bash <(curl -s x)`).
fn pipeline_denied(command: &str, parts: &[Vec<String>], depth: usize, shell: Shell) -> bool {
    let parsed = parse_command_in(command, shell);
    let runs = |segment: &[String], part: &Vec<String>| {
        segment_denied(segment, std::slice::from_ref(part), depth)
    };
    // Each pipeline, its segments in order: the parts as a subsequence.
    let mut start = 0;
    for end in 0..parsed.segments.len() {
        let pipes_on = parsed.piped.get(end).copied().unwrap_or(false);
        if pipes_on && end + 1 < parsed.segments.len() {
            continue;
        }
        let mut next = 0;
        for segment in &parsed.segments[start..=end] {
            if next < parts.len() && runs(segment, &parts[next]) {
                next += 1;
            }
        }
        if next == parts.len() {
            return true;
        }
        start = end + 1;
    }
    if depth < 3
        && parsed
            .segments
            .iter()
            .flat_map(|segment| inner_scripts(segment))
            .any(|script| pipeline_denied(&script, parts, depth + 1, Shell::Posix))
    {
        return true;
    }
    // The last part run on a substitution's output that runs the one before.
    let [.., producer, runner] = parts else {
        return false;
    };
    let Ok(substitution) = Regex::new(&format!(
        r"(\$\(|<\(|`)\s*{}\b",
        regex::escape(&producer[0].to_lowercase())
    )) else {
        return false;
    };
    parsed.opaque
        && parsed.segments.iter().any(|segment| {
            runs(segment, runner) && substitution.is_match(&segment.join(" ").to_lowercase())
        })
}

/// The denied entry a command runs, if any (`Permissions::denied_commands`).
/// An entry is a program and words it's run with (`git push`), or such
/// entries joined by `|`: the first's output piped into the next
/// (`curl | sh`: a download run by a shell, which no entry of words says).
pub fn denied_entry(command: &str, permissions: &Permissions) -> Option<String> {
    permissions.denied_commands.iter().find_map(|entry| {
        let parts: Vec<Vec<String>> = entry.split('|').map(split_words).collect();
        if parts.iter().any(Vec::is_empty) {
            return None;
        }
        // Denied when any reading of the command runs it.
        let denied = Shell::readings()
            .iter()
            .any(|&shell| match parts.as_slice() {
                [words] => command_denied(command, std::slice::from_ref(words), 0, shell),
                _ => pipeline_denied(command, &parts, 0, shell),
            });
        denied.then(|| entry.trim().to_string())
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

/// awk writing a file (`> "f"`), or running a program: `system(…)`, or a
/// pipe into one (`print … | "sh"`).
static AWK_WRITES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"system\(|>\s*"|\|\s*""#).unwrap());
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
        // `-f prog`: a program it reads (from a here-document too) may
        // run or write anything.
        return !rest
            .iter()
            .any(|a| AWK_WRITES.is_match(a) || a.starts_with("--file") || a.starts_with("-f"));
    }
    if cmd == "tree" {
        // `tree -o FILE` writes the listing to FILE.
        return !rest
            .iter()
            .any(|a| a == "-o" || (a.starts_with("-o") && !a.starts_with("--")));
    }
    if cmd == "rg" && rest.iter().any(|a| a.starts_with("--pre")) {
        // `--pre CMD` runs a program over every file searched.
        return false;
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
            // `--output=FILE` (log, diff, show) writes what it shows there.
            return !args.iter().any(|a| a.starts_with("--output"));
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
/// `check_command`, without the allowed-list check), as any reading of it
/// says.
pub fn command_modifies_files(command: &str) -> bool {
    Shell::readings()
        .iter()
        .any(|&shell| modifies_files_in(&parse_command_in(command, shell)))
}

fn modifies_files_in(parsed: &ParsedCommand) -> bool {
    parsed.writes_via_redirect()
        || parsed.opaque
        || !parsed.segments.iter().all(|s| segment_is_read_only(s))
}

/// Whether the command may run: only if every reading of it may
/// ([`Shell::readings`]); it modifies files if any reading says so.
pub fn check_command(command: &str, permissions: &Permissions) -> Verdict {
    check_command_as(command, permissions, Shell::readings())
}

/// [`check_command`] with these readings: every one has to permit it.
pub(crate) fn check_command_as(
    command: &str,
    permissions: &Permissions,
    readings: &[Shell],
) -> Verdict {
    let mut modifies_files = false;
    for &shell in readings {
        match check_command_in(command, permissions, shell) {
            Verdict::Refused(why) => return Verdict::Refused(why),
            Verdict::Permitted { modifies_files: m } => modifies_files |= m,
        }
    }
    Verdict::Permitted { modifies_files }
}

/// [`check_command`], reading the command as `shell` splits it. Split out
/// so the cmd.exe reading is tested on every build, not only a Windows one.
pub(crate) fn check_command_in(command: &str, permissions: &Permissions, shell: Shell) -> Verdict {
    let parsed = parse_command_in(command, shell);
    if parsed.segments.is_empty() {
        return Verdict::Refused("The command is empty.".into());
    }

    let modifies_files = modifies_files_in(&parsed);

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
        "bash" => {
            let background = args.get("background").and_then(|v| v.as_bool()) == Some(true);
            format!(
                "Run `{}`{}",
                s("command").unwrap_or_else(|| "…".into()),
                if background { " in the background" } else { "" }
            )
        }
        "background" => {
            let id = s("id").unwrap_or_else(|| "a background command".into());
            match s("action").as_deref() {
                Some("output") => format!("Read what {id} printed"),
                Some("stop") => format!("Stop {id}"),
                _ => "List the commands running in the background".to_string(),
            }
        }
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
        r"^(shred|truncate)\b",
        r"^find\b.*\s-(delete\b|(exec|execdir|ok|okdir)\s+(rm|mv|shred|truncate|chmod|chown)\b)",
        r"^git\b.*\b(push|reset\s+--hard|clean\s+-\S*[fd]|branch\s+-[dD]|stash\s+(drop|clear)|checkout\s+--\s|restore\b)",
        r"^(perl|ruby|php)\s+(-\S+\s+)*-(e|E|r)\b",
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

static HIDDEN_PARTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\(|[<>]\(|`|\\\r?\n").unwrap());
/// cmd.exe's own ways of hiding a program name: `%VAR%` expansion and the `^`
/// escape (`p^owershell` runs powershell). Only checked on Windows: in a
/// POSIX shell both are plain characters, common in grep patterns.
static CMD_HIDDEN_PARTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%[^%\s]+%|\^").unwrap());
static PART_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*(?:&&|\|\||;|\||&|\n|[(){}])\s*").unwrap());
static WRAPPERS: LazyLock<[Regex; 4]> = LazyLock::new(|| {
    [
        Regex::new(r"^(if|then|elif|else|fi|while|until|do|done|for|in|case|esac|!)\s+").unwrap(),
        Regex::new(
            r"^(command|builtin|exec|env|nohup|time|timeout(\s+\S+)?|nice|watch|npx|bunx|pnpx)\s+",
        )
        .unwrap(),
        Regex::new(r"^\\").unwrap(),
        Regex::new(r#"^[^\s'"\\$]*/"#).unwrap(),
    ]
});
/// A leading `NAME=value` word, as `bare_command` strips it.
static LEADING_ASSIGNMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*=\S*)\s+").unwrap());

/// Strips wrappers and prefixes until the program name is bare. None when
/// a stripped assignment isn't [`harmless_assignment`]: it can change what
/// the program runs, whatever the program is. Assignments are looked for
/// before each wrapper is stripped, so the path prefix rule doesn't take
/// `GIT_EXTERNAL_DIFF=./` for a folder.
fn bare_command(part: &str) -> Option<String> {
    let mut current = part.to_string();
    loop {
        let mut next = current.trim().to_string();
        for re in WRAPPERS.iter() {
            next = re.replace(&strip_assignments(next)?, "").into_owned();
        }
        let next = strip_assignments(next)?;
        if next == current {
            return Some(current);
        }
        current = next;
    }
}

/// `part` without its leading harmless assignments; None at one that isn't.
fn strip_assignments(mut part: String) -> Option<String> {
    while let Some(m) = LEADING_ASSIGNMENT.captures(&part) {
        if !harmless_assignment(&m[1]) {
            return None;
        }
        part = part[m[0].len()..].to_string();
    }
    Some(part)
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
        .any(|part| part.is_none_or(|part| RISKY_COMMANDS.iter().any(|p| p.is_match(&part))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(seg: &[&str]) -> Vec<String> {
        seg.iter().map(|s| s.to_string()).collect()
    }

    /// cmd.exe splits a line by its own rules, so reading a Windows command
    /// with POSIX rules let a second command hide behind a quote or an
    /// escape the allowed-commands check never looked past. Checked here
    /// for both shells so it holds on every build, not only a Windows one.
    #[test]
    fn a_command_cannot_hide_from_the_cmd_exe_splitter() {
        // `'` is an ordinary character in cmd.exe, so the `&` still ends
        // the first command and `calc` is its own, checkable, segment.
        let p = parse_command_in("ls '& calc'", Shell::Cmd);
        assert_eq!(p.segments.len(), 2, "cmd: {:?}", p.segments);
        assert_eq!(p.segments[1], words(&["calc'"]));
        // `\` is a path separator in cmd.exe, not an escape, so `\&` is
        // still a separator with `calc` behind it.
        let p = parse_command_in(r"cat a\& calc", Shell::Cmd);
        assert_eq!(p.segments.len(), 2, "cmd: {:?}", p.segments);
        assert_eq!(p.segments[1], words(&["calc"]));
        // `;` is ordinary in cmd.exe: one command, not two.
        let p = parse_command_in("echo a;calc", Shell::Cmd);
        assert_eq!(p.segments.len(), 1, "cmd: {:?}", p.segments);
        // What can't be read is opaque, so check_command refuses it unless
        // every command is allowed: `%VAR%` expansion, `^` escapes (so the
        // program name is unknown), and `( )` grouping.
        assert!(parse_command_in("%COMSPEC% /c calc", Shell::Cmd).opaque);
        assert!(parse_command_in("(calc)", Shell::Cmd).opaque);
        // A POSIX shell keeps POSIX rules: `'` quotes, `\` escapes, `;`
        // separates. Reading the same lines its way is unchanged.
        let p = parse_command_in("ls '& calc'", Shell::Posix);
        assert_eq!(p.segments, vec![words(&["ls", "& calc"])]);
        // POSIX escapes only the `&`, so this is one `cat` command with
        // `a&` and `calc` as its arguments — not a second command.
        let p = parse_command_in(r"cat a\& calc", Shell::Posix);
        assert_eq!(p.segments, vec![words(&["cat", "a&", "calc"])]);
        assert_eq!(
            parse_command_in("echo a;calc", Shell::Posix).segments.len(),
            2
        );

        // The whole point: with the default allowed list, none of the
        // cmd.exe forms slip a disallowed command through.
        let perms = Permissions {
            allowed_commands: vec!["ls".into(), "cat".into(), "echo".into()],
            ..Permissions::default()
        };
        for command in [
            "ls '& calc'",
            r"cat a\& calc",
            "%COMSPEC% /c calc",
            "(calc)",
        ] {
            let refused = matches!(
                check_command_in(command, &perms, Shell::Cmd),
                Verdict::Refused(_)
            );
            assert!(refused, "cmd.exe should refuse: {command}");
        }
    }

    /// On Windows a command is read both ways, and refused if either
    /// reading refuses it: each reading alone lets one of these through.
    #[test]
    fn windows_reads_a_command_both_ways() {
        let both = &[Shell::Posix, Shell::Cmd];
        let perms = Permissions {
            allowed_commands: vec!["ls".into(), "npm test".into(), "bash".into()],
            denied_commands: vec!["git push".into()],
            ..Permissions::default()
        };
        let refused = |command: &str, readings: &[Shell]| {
            matches!(
                check_command_as(command, &perms, readings),
                Verdict::Refused(_)
            )
        };
        // cmd.exe sees `calc`; POSIX sees one quoted argument.
        assert!(!refused("ls '& calc'", &[Shell::Posix]));
        assert!(refused("ls '& calc'", both));
        // POSIX sees `rm`; cmd.exe sees one `npm test` with odd arguments.
        assert!(!refused("npm test ; rm -rf /", &[Shell::Cmd]));
        assert!(refused("npm test ; rm -rf /", both));
        // The denied list sees through `bash -c '…'` whichever shell runs it.
        let denied = denied_entry("bash -c 'git push'", &perms);
        assert_eq!(denied.as_deref(), Some("git push"));
        assert!(refused("bash -c 'git push'", both));
        // What both permit is permitted, modifying files if either says so.
        assert_eq!(
            check_command_as("ls -la", &perms, both),
            Verdict::Permitted {
                modifies_files: false
            }
        );
        assert!(matches!(
            check_command_as("ls > out.txt", &perms, both),
            Verdict::Permitted {
                modifies_files: true
            }
        ));
    }

    /// A reader told to write, or to run a program, isn't read-only: rg
    /// runs `--pre` over every file, git's readers write `--output`, tree
    /// writes `-o`, awk's pipe runs what it names, and a program awk reads
    /// (`-f`) may do either.
    #[test]
    fn readers_that_write_or_run_programs_are_not_read_only() {
        for (command, read_only) in [
            ("rg --pre rm x", false),
            ("rg --pre=rm x", false),
            ("rg --pre-glob '*.pdf' x", false),
            ("rg -n x src", true),
            ("git log --output=src/lib.rs", false),
            ("git diff --output /tmp/x", false),
            ("git log --oneline -5", true),
            ("tree -o out.txt", false),
            ("tree -L 2 src", true),
            (r#"awk '{print "rm -rf src" | "sh"}' f"#, false),
            (r#"awk '{print $1}' f"#, true),
            ("awk -F, '{print $1}' f", true),
            ("awk -f prog.awk f", false),
            (
                "awk -f - f <<'EOF'\nBEGIN { system(\"rm -rf src\") }\nEOF",
                false,
            ),
            ("awk --file=prog.awk f", false),
        ] {
            let parsed = parse_command(command);
            assert_eq!(
                segment_is_read_only(&parsed.segments[0]),
                read_only,
                "{command}"
            );
        }
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
                    parsed.writes_via_redirect(),
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
            // The frontend reads commands the POSIX way; Windows adds the
            // cmd.exe reading's refusals (its own tests).
            let verdict = check_command_as(command, &permissions, &[Shell::Posix]);
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
        let p = parse_command(r#"LC_ALL=C git status -s && echo "a && b" | wc -l; ls 2>/dev/null"#);
        assert_eq!(
            p.segments,
            vec![
                words(&["git", "status", "-s"]),
                words(&["echo", "a && b"]),
                words(&["wc", "-l"]),
                words(&["ls"])
            ]
        );
        assert!(!p.writes_via_redirect() && !p.opaque);
        assert!(parse_command("echo hi > out.txt").writes_via_redirect());
        assert!(!parse_command("echo hi >&2").writes_via_redirect());
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
    fn risky_shapes() {
        assert!(is_risky_command("git push origin main"));
        assert!(is_risky_command("npm test && rm -rf dist"));
        assert!(is_risky_command("cat .env"));
        assert!(is_risky_command("sudo ls"));
        // Deleting by other names: find's own, git's stash and branches,
        // shred and truncate, and a string run by perl or ruby.
        for risky in [
            "find . -name '*.log' -delete",
            "find build -type f -exec rm {} \\;",
            "git stash drop",
            "git stash clear",
            "git branch -d feature",
            "shred -u secrets.txt",
            "truncate -s 0 app.log",
            "perl -e 'unlink glob \"*.bak\"'",
            "ruby -e 'File.delete(\"x\")'",
        ] {
            assert!(is_risky_command(risky), "{risky}");
        }
        for plain in ["find . -name '*.rs'", "git stash list", "git branch -a"] {
            assert!(!is_risky_command(plain), "{plain}");
        }
        assert!(is_risky_command("env FOO=1 bash -c 'x'"));
        assert!(is_risky_command("python -c 'print(1)'"));
        assert!(!is_risky_command("npm test"));
        assert!(!is_risky_command("git status"));
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
        assert_eq!(
            describe_tool_call("bash", r#"{"command":"npm run dev","background":true}"#),
            "Run `npm run dev` in the background"
        );
        assert_eq!(
            describe_tool_call("background", r#"{"action":"stop","id":"bg2"}"#),
            "Stop bg2"
        );
        assert_eq!(
            describe_tool_call("background", r#"{"action":"output","id":"bg2"}"#),
            "Read what bg2 printed"
        );
        assert_eq!(
            describe_tool_call("background", r#"{"action":"list"}"#),
            "List the commands running in the background"
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
        assert!(parse_command("ls >> log.txt").writes_via_redirect());
        assert!(parse_command("ls &> all.txt").writes_via_redirect());
        assert!(!parse_command("wc -l < input.txt 2>&1").writes_via_redirect());
        assert!(!parse_command("ls > NUL").writes_via_redirect());
        // `>|` writes past noclobber, `>&file` both streams: files, not a
        // pipe or a descriptor; `>& 2` and `>&-` are descriptors.
        assert_eq!(parse_command("echo x >| .env").writes_to, [".env"]);
        assert_eq!(parse_command("echo x >| .env").segments.len(), 1);
        assert_eq!(parse_command("make >&build.log").writes_to, ["build.log"]);
        assert_eq!(parse_command("make >& build.log").writes_to, ["build.log"]);
        assert!(!parse_command("make >& 2").writes_via_redirect());
        assert!(!parse_command("make 2>&- 1>&2").writes_via_redirect());
        // Each redirect knows its command, which runs where a `cd` left it.
        let p = parse_command("cd web && echo x >> .env.local; cat < a.txt; > b.txt");
        assert_eq!(p.writes_to, [".env.local", "b.txt"]);
        assert_eq!(p.writes_in, [1, 3]);
        assert_eq!(p.reads_from, ["a.txt"]);
        assert_eq!(p.reads_in, [2]);
        // Only harmless assignments: no command. Others are the command.
        assert!(parse_command("LC_ALL=C TZ=UTC").segments.is_empty());
        assert_eq!(
            parse_command("FOO=1 BAR=2").segments,
            vec![words(&["FOO=1", "BAR=2"])]
        );
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
        // A harmless leading assignment doesn't hide the command…
        assert!(matches!(
            check_command("LC_ALL=C git status", &p),
            Verdict::Permitted { .. }
        ));
        // …and one that can change what runs is part of it.
        assert!(
            matches!(check_command("GIT_DIR=x git status", &p), Verdict::Refused(r) if r.contains("\"GIT_DIR=x git status\""))
        );
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
    fn an_assignment_that_can_change_what_runs_isnt_hidden_from_the_allowed_list() {
        let p = Permissions::default();
        // `git diff` is allowed by default; these make it run other programs.
        for command in [
            "GIT_EXTERNAL_DIFF=./x git diff",
            "GIT_PAGER=./x git diff",
            "LD_PRELOAD=./x.so git diff",
            "PATH=. git diff",
            "LANG=C GIT_EXTERNAL_DIFF=./x git diff",
            "git status && GIT_EXTERNAL_DIFF=./x git diff",
        ] {
            assert!(
                matches!(check_command(command, &p), Verdict::Refused(_)),
                "{command}"
            );
        }
        // Locale, colour and logging settings leave the command as it is.
        for command in ["RUST_LOG=debug git diff", "LC_ALL=C NO_COLOR=1 git diff"] {
            assert!(
                matches!(check_command(command, &p), Verdict::Permitted { .. }),
                "{command}"
            );
        }
        // A harmless name with a path or an expansion as its value isn't.
        assert!(matches!(
            check_command("LC_ALL=./evil git diff", &p),
            Verdict::Refused(_)
        ));
        assert!(matches!(
            check_command("TERM=$X git diff", &p),
            Verdict::Refused(_)
        ));
    }

    #[test]
    fn an_assignment_that_can_change_what_runs_is_never_approved_by_the_reviewer_alone() {
        assert!(is_risky_command("GIT_EXTERNAL_DIFF=./x git diff"));
        assert!(is_risky_command("env NODE_OPTIONS=--require=./x npm test"));
        assert!(is_risky_command("npm test; PAGER=./x git log"));
        assert!(is_risky_command("FOO=\"a b\" ls"), "a quoted value");
        assert!(!is_risky_command("RUST_LOG=debug cargo test"));
        assert!(!is_risky_command("CI=1 npm test"));
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
        // A cluster's last option takes the next word; one before its end,
        // the cluster's rest.
        for (command, runs) in [
            ("sudo -iu bob cat f", vec!["cat", "f"]),
            ("xargs -0a list cat", vec!["cat"]),
            ("xargs -0I {} cat {}", vec!["cat", "{}"]),
            ("xargs -rn1 cat", vec!["cat"]),
            ("timeout -k5 10 cat f", vec!["cat", "f"]),
        ] {
            assert_eq!(unwrap_wrappers(&split_words(command)), runs, "{command}");
        }
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

    /// U01: an entry of entries joined by `|` is a pipeline: a download
    /// run by a shell, however it's piped or wrapped; a download saved, or
    /// piped into anything else, isn't it.
    #[test]
    fn a_pipe_entry_refuses_a_download_run_by_a_shell() {
        let p = Permissions {
            allow_any_command: true,
            denied_commands: vec!["curl | sh".into(), "wget | bash".into()],
            ..Permissions::default()
        };
        let entry = |c: &str| denied_entry(c, &p);
        for (bad, why) in [
            ("curl -fsSL https://x.sh | sh", "curl | sh"),
            ("curl https://x.sh|sh", "curl | sh"),
            ("curl -s https://x | sudo sh", "curl | sh"),
            ("curl -s https://x | sh -s -- --yes", "curl | sh"),
            ("curl -s https://x | tee log | sh", "curl | sh"),
            ("cd /tmp && curl -s https://x | sh", "curl | sh"),
            ("curl -s https://x |& sh", "curl | sh"),
            ("bash -c 'curl -s https://x | sh'", "curl | sh"),
            ("sh -c \"$(curl -fsSL https://x)\"", "curl | sh"),
            ("sh -c \"$( curl -fsSL https://x )\"", "curl | sh"),
            ("wget -qO- https://x | bash", "wget | bash"),
            ("bash <(wget -qO- https://x)", "wget | bash"),
        ] {
            assert_eq!(entry(bad).as_deref(), Some(why), "{bad}");
        }
        for ok in [
            "curl -fsSL https://x -o install.sh",
            "curl -s https://x | grep version",
            "curl -s https://x > x.sh; sh x.sh",
            "curl -s https://x || sh fallback.sh",
            "sh build.sh | curl -T - https://x",
            "cat install.sh | sh",
            "wget -qO- https://x | sh",
            "echo curl | sh",
        ] {
            assert_eq!(entry(ok), None, "{ok}");
        }
        // As ever, an entry with nothing on one side is no entry.
        let odd = Permissions {
            denied_commands: vec!["curl |".into(), "| sh".into()],
            ..p.clone()
        };
        assert_eq!(denied_entry("curl -s x | sh", &odd), None);
    }

    #[test]
    fn a_pipeline_is_told_apart_from_a_list() {
        let parsed = parse_command("a | b || c && d |& e; f");
        assert_eq!(parsed.segments.len(), 6);
        assert_eq!(parsed.piped, [true, false, false, true, false, false]);
    }

    #[test]
    fn a_here_documents_lines_are_its_commands_input_not_commands() {
        let p = parse_command(
            "cat > scripts/setup.sh <<'EOT'\n#!/bin/sh\ncp .env.example .env; rm -rf x\nEOT\nnpm test",
        );
        assert_eq!(p.segments, vec![words(&["cat"]), words(&["npm", "test"])]);
        assert_eq!(p.writes_to, ["scripts/setup.sh"]);
        let text = "#!/bin/sh\ncp .env.example .env; rm -rf x\n".to_string();
        assert_eq!(p.inputs, [Input { segment: 0, text }]);
        assert!(!p.opaque);
        // A commit message in a substitution: its quotes, parens and
        // backticks are text, and the command goes on after it.
        let p = parse_command(
            "git commit -m \"$(cat <<'EOT'\nSay \"hi\" (#12), don't `cat .env`\nEOT\n)\" && git push",
        );
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.segments[1], words(&["git", "push"]));
        // `<<-` leaves out the tabs before the delimiter; two documents
        // come one after the other.
        assert_eq!(
            parse_command("cat <<-EOF\n\tx\n\tEOF\ncat <<A <<B\na\nA\nb\nB\nls").segments,
            vec![words(&["cat"]), words(&["cat"]), words(&["ls"])]
        );
        // The shell runs the substitutions in lines it expands, not in
        // quoted ones.
        assert!(parse_command("cat <<EOF\n$(rm -rf x)\nEOF").opaque);
        assert!(parse_command("cat <<EOF\n`rm -rf x`\nEOF").opaque);
        assert!(!parse_command("cat <<'EOF'\n$(rm -rf x)\nEOF").opaque);
        // A shift isn't one; a here-string is input too; a comment isn't run.
        assert!(parse_command("echo $((1 << 2)) && ((x <<= 1))")
            .inputs
            .is_empty());
        assert_eq!(parse_command("echo $((1 << 2)); ls").segments.len(), 2);
        let text = "a b".to_string();
        assert_eq!(
            parse_command("wc -w <<< \"a b\"").inputs,
            [Input { segment: 0, text }]
        );
        assert_eq!(
            parse_command("# don't\nls # it's ok\npwd").segments,
            vec![words(&["ls"]), words(&["pwd"])]
        );
    }

    #[test]
    fn a_here_document_a_shell_reads_is_commands() {
        for command in [
            "bash <<'EOF'\nrm -rf src\nEOF",
            "cat <<'EOF' | sudo sh\nrm -rf src\nEOF",
            "FOO=1 bash -s <<EOF\nrm -rf src\nEOF",
        ] {
            let p = parse_command(command);
            let rm = words(&["rm", "-rf", "src"]);
            assert!(p.segments.contains(&rm), "{command}: {:?}", p.segments);
            assert!(!p.segments.contains(&words(&["EOF"])), "{command}");
            assert!(command_modifies_files(command), "{command}");
        }
        // cmd.exe, which runs commands on Windows too, has no here-documents:
        // it runs the lines.
        let text = "cat <<'EOF'\nrm -rf src\nEOF";
        assert_eq!(command_modifies_files(text), cfg!(windows));
    }

    /// A lone `&` leaves what it ends running; `&&`, `|&`, `&>` and `>&`
    /// don't, nor a quoted `&`.
    #[test]
    fn a_part_left_running_with_an_ampersand_is_told_apart() {
        for left in [
            "npm run dev &",
            "npm run dev & sleep 5; curl -s localhost:5173",
            "npm run dev > dev.log 2>&1 & sleep 5",
            "nohup vite --port 5173 >/dev/null 2>&1 &",
            "a &\nb",
        ] {
            assert!(parse_command(left).backgrounds, "{left}");
        }
        for waited in [
            "npm test && npm run lint",
            "make 2>&1 | tee log",
            "cargo build |& tee log",
            "cargo build &> log",
            "echo 'a & b'",
            "echo \"a & b\"",
            "echo a\\&b",
        ] {
            assert!(!parse_command(waited).backgrounds, "{waited}");
        }
        // cmd.exe has no background: its `&` only separates.
        assert!(!parse_command_in("a & b", Shell::Cmd).backgrounds);
    }
}
