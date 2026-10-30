//! A plan's steps (D01): where its steps section is, how it splits into
//! steps, each step's number, title, `Files:` line and group, as written.

use super::check::{step_name, without_dots};
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

static STEPS_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^##\s+.*\bsteps?\b.*$").unwrap());

/// `## Steps` itself, taken before any other heading with "step" in it
/// (P4.4: a "## Next steps" note before it isn't the steps).
static STEPS_HEADING_EXACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^##\s+(?:the\s+)?steps\s*:?\s*$").unwrap());

/// Then one that starts with "Steps" (`## Steps (ordered)`, `## 3. Steps`),
/// still before one that only mentions them.
static STEPS_HEADING_LEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^##[ \t]+(?:\d+[.)][ \t]*)?(?:the[ \t]+)?steps\b[^\n]*$").unwrap()
});

/// The steps section's heading (P4.4).
pub(super) fn steps_heading(text: &str) -> Option<regex::Match<'_>> {
    STEPS_HEADING_EXACT
        .find(text)
        .or_else(|| STEPS_HEADING_LEADING.find(text))
        .or_else(|| STEPS_HEADING.find(text))
}

pub(super) static NEXT_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^##\s").unwrap());

static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\d+[.)]\s+\S").unwrap());

/// A step written as a heading or a bold line instead of a list item:
/// `### Step 1: Title`, `### 1. Title`, `**Step 1: Title**` (some models
/// write steps so).
static HEADING_STEP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*(?:#{2,6}\s+|\*\*)\s*(?:step\s*)?(\d+)[.):]?\s*\S").unwrap()
});

pub(super) static CODE_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```.*?```").unwrap());

/// Whether a line opens or closes a fenced code block.
pub(super) fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    // A span closed on its own line (```ts x = 1```) opens nothing (round 3).
    ["```", "~~~"]
        .iter()
        .any(|m| t.starts_with(m) && !t[m.len()..].contains(m))
}

/// The lines of `text` with their byte offsets and whether each is inside
/// a fenced code block (fence lines count as inside).
pub(super) fn lines_with_fences(text: &str) -> Vec<(usize, &str, bool)> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut fence = false;
    for line in text.split_inclusive('\n') {
        let opens = is_fence(line);
        out.push((at, line, fence || opens));
        if opens {
            fence = !fence;
        }
        at += line.len();
    }
    out
}

/// The text of the `## ...` section that starts at `heading_end`: up to
/// the next `## ` heading outside a code block.
pub(super) fn section_after(text: &str, heading_end: usize) -> &str {
    let after = &text[heading_end..];
    let end = lines_with_fences(after)
        .into_iter()
        .find(|(_, line, fenced)| !fenced && NEXT_HEADING.is_match(line))
        .map_or(after.len(), |(at, _, _)| at);
    &after[..end]
}

/// The numbered items of a steps section, each with its sub-lines: the
/// least indented numbered lines outside code blocks (a numbered list
/// nested in a step is part of that step).
pub(super) fn split_steps(section: &str) -> Vec<&str> {
    let lines = lines_with_fences(section);
    let numbered: Vec<(usize, usize)> = lines
        .iter()
        .filter(|(_, line, fenced)| !fenced && NUMBERED.is_match(line) && !line.trim().is_empty())
        .map(|(at, line, _)| (*at, line.len() - line.trim_start().len()))
        .collect();
    let indent = numbered.iter().map(|(_, i)| *i).min().unwrap_or(0);
    let mut starts: Vec<usize> = numbered
        .into_iter()
        .filter(|(_, i)| *i == indent)
        .map(|(at, _)| at)
        .collect();
    // No list: steps written as headings or bold lines.
    if starts.is_empty() {
        starts = lines
            .iter()
            .filter(|(_, line, fenced)| !fenced && HEADING_STEP.is_match(line))
            .map(|(at, _, _)| *at)
            .collect();
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| &section[start..starts.get(i + 1).copied().unwrap_or(section.len())])
        .collect()
}

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
    let lines = lines_with_fences(raw);
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
    // P4.4: one kind of line ending.
    let markdown = markdown.replace("\r\n", "\n");
    let text = markdown.trim();
    let Some(heading) = steps_heading(text) else {
        return Vec::new();
    };
    // Code blocks stay in a step's body (an exact signature, a schema).
    let section = section_after(text, heading.end());
    let raws: Vec<&str> = split_steps(section)
        .into_iter()
        // NUMBERED's `^\s*` can start at the blank line before a step.
        .map(str::trim_start)
        .collect();
    let numbers = step_numbers(&raws);
    raws.into_iter()
        .enumerate()
        .map(|(i, raw)| {
            let first = raw.lines().next().unwrap_or("");
            let rest = STEP_NUMBER
                .captures(first)
                .map_or_else(|| first.to_string(), |c| c[2].to_string());
            let (number, written) = numbers[i];
            let files = step_files(raw).files;
            PlanStep {
                number,
                written,
                title: step_title(&rest),
                files,
                body: raw.trim_end().to_string(),
                // P4.4: outside code blocks; one name, any case; "none" isn't.
                group: lines_with_fences(raw)
                    .into_iter()
                    .filter(|(_, _, fenced)| !fenced)
                    .find_map(|(_, line, _)| GROUP_LINE.captures(line))
                    .map(|c| group_name(&c[1]))
                    .filter(|g| !g.is_empty() && !matches!(g.as_str(), "none" | "-" | "n/a")),
            }
        })
        .collect()
}

#[allow(dead_code)] // D03's worker brief reads "## Decisions" with it.
/// D01: the text of the plan's `## <name>` section (a heading that starts
/// with the name, case aside), without its heading.
pub fn section(markdown: &str, name: &str) -> Option<String> {
    let markdown = markdown.replace("\r\n", "\n");
    let markdown = markdown.as_str();
    let lower = name.to_lowercase();
    let mut at = 0;
    while let Some(h) = NEXT_HEADING.find(&markdown[at..]) {
        let start = at + h.start();
        let line_end = markdown[start..]
            .find('\n')
            .map_or(markdown.len(), |e| start + e);
        let title = markdown[start + 2..line_end].trim().to_lowercase();
        // `## 5. Decisions` is the Decisions section, as `## 3. Steps` is the steps.
        let title = title
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .strip_prefix(['.', ')'])
            .filter(|_| title.starts_with(|c: char| c.is_ascii_digit()))
            .map_or(title.as_str(), str::trim_start);
        if title.starts_with(&lower) {
            return Some(section_after(markdown, line_end).trim().to_string());
        }
        at = line_end;
    }
    None
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
