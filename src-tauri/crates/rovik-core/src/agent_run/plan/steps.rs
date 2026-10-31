//! A plan's steps (D01): where its steps section is, how it splits into
//! steps, each step's number, title, `Files:` line and group, as written.

use super::check::{step_name, without_dots};
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// P4.4: each step's number, as written when the plan's numbers go up one
/// by one, else its place (`1. 1. 1.`, as some editors number lists), and
/// the number the plan shows when that's another.
pub(super) fn step_numbers(raws: &[&str]) -> Vec<(usize, Option<usize>)> {
    let written: Vec<Option<usize>> = raws
        .iter()
        .map(|raw| {
            STEP_NUMBER
                .captures(raw.trim_start().lines().next().unwrap_or(""))
                .and_then(|c| c[1].parse().ok())
        })
        .collect();
    let increasing = written.iter().all(Option::is_some) && written.windows(2).all(|w| w[0] < w[1]);
    written
        .into_iter()
        .enumerate()
        .map(|(i, w)| {
            let number = if increasing {
                w.unwrap_or(i + 1)
            } else {
                i + 1
            };
            (number, w.filter(|w| *w != number))
        })
        .collect()
}

/// A file a plan step names.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFile {
    /// Repo-relative, as written (a folder ends with `/`).
    pub path: String,
    /// Marked "(new)": the step creates it.
    pub new: bool,
    /// Marked "(read)" or "(read-only)": context the step uses but doesn't
    /// change (F03: what a contract step settles).
    pub read_only: bool,
}

/// One numbered step of a plan's "## Steps" section.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    /// As numbered in the plan, or its place when the plan's numbers don't
    /// go up one by one (P4.4).
    pub number: usize,
    /// The number the plan shows, when it isn't `number` (`1. 2. 2. 3.`:
    /// the last is step 4, written "3.").
    pub written: Option<usize>,
    pub title: String,
    /// The files it names, each once, in order.
    pub files: Vec<PlanFile>,
    /// The whole step, its first line included.
    pub body: String,
    /// `Group: <name>`: steps that may be built side by side (F03).
    pub group: Option<String>,
}

impl PlanStep {
    /// "4", or "4 (numbered 3. in the plan)": how messages name it.
    pub fn named(&self) -> String {
        step_name(self.number, self.written)
    }

    /// The step's text without its number, in simple words: a step that
    /// only moved (a step inserted before it) reads the same (plan_review.rs).
    pub fn content_key(&self) -> String {
        let (first, rest) = self.body.split_once('\n').unwrap_or((&self.body, ""));
        let first = STEP_NUMBER
            .captures(first)
            .map_or_else(|| first.to_string(), |c| c[2].to_string());
        simple_words(&format!("{first}\n{rest}"))
    }
}

/// A step's number and the rest of its first line: `1. Title`,
/// `### Step 1: Title`, `**Step 1: Title**`.
static STEP_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:#{1,6}\s+)?\**(?:step\s*)?(\d+)[.):]?\**\s*(?:[—–-]\s+)?(.*)$").unwrap()
});

pub(super) static NEW_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\((?:new|new file|created?)\)").unwrap());

static READ_MARK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\((?:read|read-only|readonly|reads|context)\)").unwrap());

/// A `Group:` line's name, in lower case: what's in its backticks, else
/// its text; a note or a period after it isn't the name (`` `api`. ``,
/// `api (runs alongside step 2)`).
fn group_name(raw: &str) -> String {
    let t = raw.trim();
    let name = match t.strip_prefix('`').and_then(|r| r.split_once('`')) {
        Some((inside, _)) => inside,
        None => t
            .split(" (")
            .next()
            .unwrap_or(t)
            .trim_end_matches(['.', ',', ';']),
    };
    name.trim().to_lowercase()
}

static GROUP_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^[\s>*-]*\**group\**\s*:\s*\**\s*([^*\n]+?)\s*\**\s*$").unwrap()
});

/// A step's `Files:` line: the one place a step names its files. The
/// label may be bulleted or bold (`- **Files:** ...`), singular (`File:`)
/// or `Paths:`, and the items may go on list lines indented under it
/// instead of after it.
pub(super) static FILES_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[\s>*+-]*\**(?:files?|paths?|file paths?)\**\s*:\**\s*(.*?)\s*$").unwrap()
});

/// Why an item of a `Files:` line isn't a path of the project.
#[derive(Debug, Clone, PartialEq)]
pub enum BadPath {
    /// `/etc/x`, `C:\x`: not relative to the project.
    Absolute,
    /// Has `..` parts, with the path it resolves to inside the project
    /// when it does: `src/../x.rs` is `x.rs`, and `../../src/x.rs`,
    /// written from the plan's folder, is `src/x.rs`.
    Outside(Option<String>),
    /// Has spaces, a scheme, a glob or other characters no path has, or
    /// is the project root.
    NotAPath,
}

/// The project path an item of a `Files:` line names, in one spelling:
/// `\` folded to `/`, `./` dropped, a folder keeping its closing `/`, a
/// `:line` suffix dropped.
pub fn file_path(token: &str) -> Result<String, BadPath> {
    let t = token.trim().replace('\\', "/");
    let t = t.trim_start_matches("./");
    let b = t.as_bytes();
    // A drive (`C:/x`, `D:a.rs`) or a UNC path isn't the project's.
    if t.starts_with('/') || (b.len() > 1 && b[1] == b':') {
        return Err(BadPath::Absolute);
    }
    if t.contains("://") {
        return Err(BadPath::NotAPath);
    }
    let t = t.split(':').next().unwrap_or(t);
    if t.is_empty()
        || t.contains(char::is_whitespace)
        || t.contains([
            '<', '>', '{', '}', '*', '?', '$', '=', ',', ';', '"', '\'', '|', '`',
        ])
    {
        return Err(BadPath::NotAPath);
    }
    if t.split('/').any(|part| part == "..") {
        // Leading `../` parts are the plan's folder's; the rest may be
        // the path from the root.
        let rest = t.trim_start_matches("../");
        return Err(match without_dots(rest) {
            Some(inside) if inside.is_empty() => BadPath::NotAPath,
            inside => BadPath::Outside(inside),
        });
    }
    let p = super::super::paths::norm_rel(t)
        .filter(|p| !p.is_empty())
        .ok_or(BadPath::NotAPath)?;
    Ok(if t.ends_with('/') { format!("{p}/") } else { p })
}

fn step_title(first_line: &str) -> String {
    let t = first_line.trim().trim_matches('*').trim();
    let t = t.split(" — ").next().unwrap_or(t);
    t.trim_end_matches(':').trim_matches('*').trim().to_string()
}

/// The items of a `Files:` line: split on commas and semicolons outside
/// backticks.
pub(super) fn split_items(text: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let (mut start, mut ticked) = (0, false);
    for (i, c) in text.char_indices() {
        match c {
            '`' => ticked = !ticked,
            ',' | ';' if !ticked => {
                items.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(&text[start..]);
    items
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// What a `Files:` line names: its files, each once, and the items that
/// aren't the project's paths (with why). An item is a backticked path
/// with its mark after it (`` `src/a.rs` (new) ``); several backticked
/// paths in one item each take the mark after them; without backticks,
/// the item up to its mark is the path.
fn parse_files(text: &str) -> (Vec<PlanFile>, Vec<(String, BadPath)>) {
    let mut files: Vec<PlanFile> = Vec::new();
    let mut bad: Vec<(String, BadPath)> = Vec::new();
    for item in split_items(text) {
        // (path, the text after it: its mark).
        let mut pieces: Vec<(&str, &str)> = Vec::new();
        let ticks: Vec<usize> = item.match_indices('`').map(|(i, _)| i).collect();
        if ticks.len() >= 2 {
            for (n, pair) in ticks.chunks(2).enumerate() {
                let [open, close] = pair else { break };
                let after_end = ticks.get(2 * (n + 1)).copied().unwrap_or(item.len());
                pieces.push((&item[open + 1..*close], &item[close + 1..after_end]));
            }
        } else {
            // The path is what comes before the mark; words there make
            // it no path.
            let plain = item.trim_matches(|c| c == '*' || c == '_').trim();
            let (token, rest) = plain.find('(').map_or((plain, ""), |at| plain.split_at(at));
            let token = token.trim();
            let token = token
                .strip_suffix('.')
                .filter(|t| !t.ends_with('.'))
                .unwrap_or(token);
            pieces.push((token, rest));
        }
        for (token, after) in pieces {
            let token = token.trim();
            if token.is_empty()
                || matches!(
                    token.to_ascii_lowercase().as_str(),
                    "none" | "-" | "n/a" | "no files" | "no file"
                )
            {
                continue;
            }
            let new = NEW_MARK.is_match(after);
            let read_only = READ_MARK.is_match(after);
            match file_path(token) {
                Ok(path) => match files.iter_mut().find(|f| f.path == path) {
                    Some(f) => {
                        f.new |= new;
                        // Changed anywhere in the step: not read-only.
                        f.read_only &= read_only;
                    }
                    None => files.push(PlanFile {
                        path,
                        new,
                        read_only,
                    }),
                },
                Err(why) => {
                    let token = token.replace('\\', "/");
                    if !bad.iter().any(|(t, _)| *t == token) {
                        bad.push((token, why));
                    }
                }
            }
        }
    }
    (files, bad)
}

/// A step's files, as its `Files:` line names them.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StepFiles {
    /// The step has a `Files:` line.
    pub line: bool,
    pub files: Vec<PlanFile>,
    /// Items of the line that aren't the project's paths.
    pub bad: Vec<(String, BadPath)>,
}

/// The files a step names: those on its `Files:` line (outside code
/// blocks), with the list lines indented under it. Paths anywhere else in
/// the step are prose.
pub fn step_files(raw: &str) -> StepFiles {
    let doc = super::PlanDoc::parse(raw);
    step_files_in(&doc.lines_in(0..doc.text().len()))
}

fn step_files_in(lines: &[(usize, &str, bool)]) -> StepFiles {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let Some(at) = lines
        .iter()
        .position(|(_, line, fenced)| !fenced && FILES_LINE.is_match(line))
    else {
        return StepFiles::default();
    };
    let (_, line, _) = lines[at];
    let mut text = FILES_LINE
        .captures(line)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    for (_, next, fenced) in &lines[at + 1..] {
        let t = next.trim();
        if *fenced || t.is_empty() || indent(next) <= indent(line) {
            break;
        }
        // A list under the label, or a wrapped item.
        if !t.starts_with(['-', '*', '+', '`']) {
            break;
        }
        let t = t.trim_start_matches(['-', '*', '+']).trim();
        if !text.is_empty() {
            text.push_str(", ");
        }
        text.push_str(t);
    }
    let (files, bad) = parse_files(&text);
    StepFiles {
        line: true,
        files,
        bad,
    }
}

/// D01: the plan's steps, in order (empty when it has no steps section).
pub fn steps(markdown: &str) -> Vec<PlanStep> {
    super::PlanDoc::parse(markdown).steps()
}

impl super::PlanDoc {
    /// The indexed steps, preserving each body as written.
    pub fn steps(&self) -> Vec<PlanStep> {
        self.raw_steps().iter().map(|s| s.step.clone()).collect()
    }

    /// The body of the first section whose title starts with `name`.
    pub fn section(&self, name: &str) -> Option<String> {
        self.named_section(name)
            .map(|s| self.text()[s.body.clone()].trim().to_string())
    }
}

/// The text of a named section, without its heading.
pub fn section(markdown: &str, name: &str) -> Option<String> {
    super::PlanDoc::parse(markdown).section(name)
}

pub(super) fn parse_steps(
    doc: &super::PlanDoc,
    ranges: Vec<std::ops::Range<usize>>,
) -> Vec<super::doc::RawStep> {
    let raws: Vec<_> = ranges.iter().map(|r| &doc.text()[r.clone()]).collect();
    let numbers = step_numbers(&raws);
    ranges
        .into_iter()
        .zip(numbers)
        .map(|(range, (number, written))| {
            let raw = &doc.text()[range.clone()];
            let first = raw.trim_start().lines().next().unwrap_or("");
            let rest = STEP_NUMBER
                .captures(first)
                .map_or_else(|| first.to_string(), |c| c[2].to_string());
            let lines = doc.lines_in(range.clone());
            let files = step_files_in(&lines);
            let group = lines
                .iter()
                .filter(|(_, _, fenced)| !fenced)
                .find_map(|(_, line, _)| GROUP_LINE.captures(line))
                .map(|c| group_name(&c[1]))
                .filter(|g| !g.is_empty() && !matches!(g.as_str(), "none" | "-" | "n/a"));
            let step = PlanStep {
                number,
                written,
                title: step_title(&rest),
                files: files.files.clone(),
                body: raw.trim().to_string(),
                group,
            };
            super::doc::RawStep { range, step, files }
        })
        .collect()
}

/// A title's words alone: lower case, letters and digits, one space
/// between; so two spellings of the same step or finding compare equal.
pub fn simple_words(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// Kept for the existing fence regression test; parsing lives in PlanDoc.
#[cfg(test)]
pub(super) fn is_fence(line: &str) -> bool {
    super::PlanDoc::parse(line)
        .lines()
        .first()
        .is_some_and(|l| l.delimiter)
}
