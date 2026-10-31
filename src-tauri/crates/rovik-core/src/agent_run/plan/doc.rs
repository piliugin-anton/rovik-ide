//! A plan's Markdown structure, indexed once and shared by its readers.

use super::steps::{self, PlanStep, StepFiles};
use regex::Regex;
use std::ops::Range;
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

static NUMBERED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\d+[.)]\s+\S").unwrap());

/// A step written as a heading or a bold line instead of a list item:
/// `### Step 1: Title`, `### 1. Title`, `**Step 1: Title**` (some models
/// write steps so).
static HEADING_STEP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*(?:#{2,6}\s+|\*\*)\s*(?:step\s*)?(\d+)[.):]?\s*\S").unwrap()
});

// Preserve the checker's backtick-only code-size rule (including inline spans).
static CODE_BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```.*?```").unwrap());

static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^##\s").unwrap());
static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#\s+\S").unwrap());
static VALIDATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^##\s+.*\b(verif\w*|validat\w*|test\w*|check\w*|acceptance)\b.*$").unwrap()
});
static FINDINGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^##\s+(?:open\s+)?(?:review\s+)?findings\s*:?\s*$").unwrap());

static LOOKS_LIKE_STEPS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^##\s+.*\bsteps?\b").unwrap());

static NUMBERED_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\d+[.)]\s").unwrap());

static TITLE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#\s+\S").unwrap());

/// A section a plan has besides its steps.
static PLAN_SECTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^##\s+.*\b(decisions?|validat\w*|verif\w*|risks?)\b").unwrap()
});

#[derive(Debug)]
pub(super) struct Line {
    pub range: Range<usize>,
    pub fenced: bool,
    pub delimiter: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Section {
    pub heading: Range<usize>,
    pub body: Range<usize>,
    pub title: String,
    pub findings: bool,
    pub fenced: bool,
}

/// A step's source and parsed fields, shared by checks and execution.
#[derive(Debug)]
pub(super) struct RawStep {
    pub range: Range<usize>,
    pub step: PlanStep,
    pub files: StepFiles,
}

/// A plan with normalized line endings and byte ranges into its text.
/// The original spelling is retained for transformations that change nothing.
#[derive(Debug)]
pub struct PlanDoc {
    original: String,
    text: String,
    lines: Vec<Line>,
    sections: Vec<Section>,
    named_sections: Vec<Section>,
    steps_section: Option<usize>,
    raw_steps: Vec<RawStep>,
    title: Option<Range<usize>>,
    open_fence: Option<&'static str>,
    code_blocks: Vec<Range<usize>>,
    validation_range: Option<Range<usize>>,
    looks_like_plan: bool,
    title_is_flush: bool,
    save_title: bool,
}

impl PlanDoc {
    /// Indexes line endings, fences, headings and steps once.
    pub fn parse(markdown: &str) -> Self {
        let text = markdown.replace("\r\n", "\n");
        let mut lines = Vec::new();
        let mut sections: Vec<Section> = Vec::new();
        let mut named_sections: Vec<Section> = Vec::new();
        let mut previous: [Option<usize>; 2] = [None, None];
        let mut at = 0;
        let mut open_fence = None;
        let code_blocks = CODE_BLOCK.find_iter(&text).map(|m| m.range()).collect();
        let mut title = None;
        let mut first = true;
        let mut title_is_flush = false;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_start();
            let delimiter = ["```", "~~~"]
                .into_iter()
                .find(|m| trimmed.starts_with(m) && !trimmed[m.len()..].contains(m));
            let fenced = open_fence.is_some() || delimiter.is_some();
            if let Some(marker) = delimiter {
                open_fence = if open_fence.is_some() {
                    None
                } else {
                    Some(marker)
                };
            }
            if first && !line.trim().is_empty() {
                first = false;
                title_is_flush = TITLE.is_match(line);
                if TITLE.is_match(line.trim_start()) {
                    title = Some(at..at + line.trim_end_matches('\n').len());
                }
            }
            if delimiter.is_none() && HEADING.is_match(line) {
                let end = at + line.trim_end_matches('\n').len();
                let name = line[2..].trim().to_lowercase();
                let name = name
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .strip_prefix(['.', ')'])
                    .filter(|_| name.starts_with(|c: char| c.is_ascii_digit()))
                    .map_or(name.as_str(), str::trim_start)
                    .to_string();
                let section = Section {
                    heading: at..end,
                    body: end..text.len(),
                    title: name,
                    findings: FINDINGS.is_match(line.trim_end()),
                    fenced,
                };
                if !fenced {
                    if let Some(previous) = sections.last_mut() {
                        previous.body.end = at;
                    }
                    sections.push(section.clone());
                }
                // Named-section readers historically reset fence state at the
                // heading, even when a previous fence was left open.
                let slot = usize::from(fenced);
                if let Some(i) = previous[slot] {
                    named_sections[i].body.end = at;
                }
                previous[slot] = Some(named_sections.len());
                named_sections.push(section);
            }
            lines.push(Line {
                range: at..at + line.len(),
                fenced,
                delimiter: delimiter.is_some(),
            });
            at += line.len();
        }
        let steps_section = [
            &*STEPS_HEADING_EXACT,
            &*STEPS_HEADING_LEADING,
            &*STEPS_HEADING,
        ]
        .into_iter()
        .find_map(|pattern| {
            sections
                .iter()
                .position(|s| pattern.is_match(&text[s.heading.clone()]))
        });
        // The checker historically recognizes a validation heading even in an
        // unclosed fence; report the fence error without inventing a second error.
        let validation_range = lines.iter().enumerate().find_map(|(i, line)| {
            let heading = text[line.range.clone()].trim_end();
            if !VALIDATION.is_match(heading)
                || steps_section.is_some_and(|n| sections[n].heading.start == line.range.start)
            {
                return None;
            }
            let end = lines[i + 1..]
                .iter()
                .find(|next| {
                    next.fenced == line.fenced
                        && !next.delimiter
                        && HEADING.is_match(&text[next.range.clone()])
                })
                .map_or(text.len(), |next| next.range.start);
            Some(line.range.end..end)
        });
        let looks_like_plan = LOOKS_LIKE_STEPS.is_match(&text)
            && NUMBERED_LINE.is_match(&text)
            && (TITLE_LINE.is_match(&text) || PLAN_SECTION.is_match(&text));
        let save_title = text.trim().starts_with("# ") || text.trim().starts_with("#\t");
        let mut doc = Self {
            original: markdown.into(),
            text,
            lines,
            sections,
            named_sections,
            steps_section,
            raw_steps: Vec::new(),
            title,
            open_fence,
            code_blocks,
            validation_range,
            looks_like_plan,
            title_is_flush,
            save_title,
        };
        if let Some(section) = doc.steps_section() {
            let lines = doc.lines_in(section.body.clone());
            let numbered: Vec<_> = lines
                .iter()
                .filter(|(_, line, fenced)| {
                    !fenced && NUMBERED.is_match(line) && !line.trim().is_empty()
                })
                .map(|(at, line, _)| (*at, line.len() - line.trim_start().len()))
                .collect();
            let indent = numbered.iter().map(|(_, i)| *i).min().unwrap_or(0);
            let mut starts: Vec<_> = numbered
                .into_iter()
                .filter(|(_, i)| *i == indent)
                .map(|(at, _)| at)
                .collect();
            if starts.is_empty() {
                starts = lines
                    .iter()
                    .filter(|(_, line, fenced)| !fenced && HEADING_STEP.is_match(line))
                    .map(|(at, _, _)| *at)
                    .collect();
            }
            let ranges: Vec<_> = starts
                .iter()
                .enumerate()
                .map(|(i, &start)| {
                    let end = starts.get(i + 1).copied().unwrap_or(section.body.end);
                    start..end
                })
                .collect();
            doc.raw_steps = steps::parse_steps(&doc, ranges);
        }
        doc
    }

    /// The plan as written, with LF line endings.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Whether a reply has the structure of a plan to submit.
    pub fn looks_like_plan(&self) -> bool {
        self.looks_like_plan
    }
    pub(super) fn title_is_flush(&self) -> bool {
        self.title_is_flush
    }
    pub(super) fn save_title(&self) -> bool {
        self.save_title
    }
    pub(super) fn original(&self) -> &str {
        &self.original
    }
    pub(super) fn lines_in(&self, range: Range<usize>) -> Vec<(usize, &str, bool)> {
        self.lines
            .iter()
            .filter(|l| l.range.start >= range.start && l.range.start < range.end)
            .map(|l| (l.range.start, &self.text[l.range.clone()], l.fenced))
            .collect()
    }
    pub(super) fn lines(&self) -> &[Line] {
        &self.lines
    }
    pub(super) fn sections(&self) -> &[Section] {
        &self.sections
    }
    pub(super) fn steps_section(&self) -> Option<&Section> {
        self.steps_section.map(|i| &self.sections[i])
    }
    pub(super) fn criteria_heading(&self) -> Option<&Section> {
        self.sections.iter().find(|s| s.title.contains("criteri"))
    }
    pub(super) fn named_section(&self, name: &str) -> Option<&Section> {
        let name = name.to_lowercase();
        self.named_sections
            .iter()
            .find(|s| s.title.starts_with(&name))
    }
    pub(super) fn raw_steps(&self) -> &[RawStep] {
        &self.raw_steps
    }
    pub(super) fn title(&self) -> Option<&str> {
        self.title.as_ref().map(|r| self.text[r.clone()].trim())
    }
    pub(super) fn open_fence(&self) -> Option<&'static str> {
        self.open_fence
    }
    pub(super) fn code_blocks(&self) -> &[Range<usize>] {
        &self.code_blocks
    }
    pub(super) fn validation(&self) -> Option<&str> {
        self.validation_range
            .as_ref()
            .map(|r| self.text[r.clone()].trim())
    }
    pub(super) fn prose(&self, range: Range<usize>) -> String {
        self.lines_in(range)
            .into_iter()
            .filter(|(_, _, fenced)| !fenced)
            .map(|(_, l, _)| l)
            .collect()
    }
}
