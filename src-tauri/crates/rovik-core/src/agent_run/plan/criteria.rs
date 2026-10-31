//! A plan's criteria (S01, D8): what done means, what's out of scope and
//! what was assumed, read from its "## Criteria" section and put back.

use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// An Autopilot plan's "## Criteria": what done means, as numbered,
/// testable statements, then what the task leaves alone and what it
/// assumed. The test writer writes from them and the acceptance check
/// judges by them.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Criteria {
    /// The numbered statements, in order, each on one line.
    pub items: Vec<String>,
    /// What the task leaves alone; empty when it says "None".
    pub out_of_scope: Vec<String>,
    /// What the planner decided that the request doesn't say.
    pub assumptions: Vec<String>,
    /// Whether the section has its **Out of scope** part (even "None.").
    pub has_out_of_scope: bool,
    /// Whether it has its **Assumptions** part (even "None.").
    pub has_assumptions: bool,
}

/// A part's heading inside the criteria: `**Out of scope**`, `**Out of
/// scope:** the CLI`, `### Assumptions`, `- Assumptions: none`.
static CRITERIA_PART: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(?:[-*+]\s+)?(?:#{3,6}\s*)?(?:\*\*|__)?\s*(out[ -]of[ -]scope|non[- ]goals|assumptions?)\s*:?\s*(?:\*\*|__)?\s*:?\s*(.*)$",
    )
    .unwrap()
});

/// A numbered line: `1. Text`, `2) Text`.
static NUMBERED_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\d+[.)]\s+(\S.*)$").unwrap());

/// A bullet: `- Text`, `* Text`.
static BULLET_ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*[-*+]\s+(\S.*)$").unwrap());

/// Whether a part's line says it has nothing: "None.", "n/a", "—".
fn says_none(text: &str) -> bool {
    let t = text
        .trim()
        .trim_matches(|c: char| c == '*' || c == '_' || c == '.' || c == '-' || c == '—');
    matches!(
        t.to_lowercase().as_str(),
        "" | "none" | "n/a" | "na" | "nothing"
    )
}

/// The plan's criteria section: the first `## ` heading with "criteria" in
/// it (`## Criteria`, `## Acceptance criteria`), without its heading.
pub fn criteria_section(markdown: &str) -> Option<String> {
    super::PlanDoc::parse(markdown).criteria_section()
}

impl super::PlanDoc {
    /// Criteria section using this document's indexed structure.
    pub fn criteria_section(&self) -> Option<String> {
        self.criteria_heading()
            .map(|s| self.text()[s.body.clone()].trim().to_string())
    }
}

/// S01: an Autopilot plan's criteria, read from its "## Criteria"; None
/// when it has none. Strict: only numbered lines are criteria (a line
/// under one continues it), and the two parts are found by their names.
pub fn criteria(markdown: &str) -> Option<Criteria> {
    super::PlanDoc::parse(markdown).criteria()
}

impl super::PlanDoc {
    /// Criteria using this document's indexed structure.
    pub fn criteria(&self) -> Option<Criteria> {
        let section = self.criteria_heading()?;
        #[derive(PartialEq)]
        enum Part {
            Items,
            OutOfScope,
            Assumptions,
        }
        let mut part = Part::Items;
        let mut out = Criteria::default();
        // The list a line continues: the latest item of the part it's in.
        let mut open = false;
        for (_, line, fenced) in self.lines_in(section.body.clone()) {
            let line = line.trim_end_matches('\n');
            if fenced {
                continue;
            }
            if line.trim().is_empty() {
                open = false;
                continue;
            }
            if let Some(c) = CRITERIA_PART.captures(line) {
                let name = c[1].to_lowercase();
                part = if name.starts_with("assum") {
                    out.has_assumptions = true;
                    Part::Assumptions
                } else {
                    out.has_out_of_scope = true;
                    Part::OutOfScope
                };
                let rest = c[2].trim();
                open = false;
                if !says_none(rest) {
                    let list = match part {
                        Part::Assumptions => &mut out.assumptions,
                        _ => &mut out.out_of_scope,
                    };
                    list.push(rest.to_string());
                    open = true;
                }
                continue;
            }
            let list = match part {
                Part::Items => &mut out.items,
                Part::OutOfScope => &mut out.out_of_scope,
                Part::Assumptions => &mut out.assumptions,
            };
            let item = NUMBERED_ITEM
                .captures(line)
                .or_else(|| {
                    (part != Part::Items)
                        .then(|| BULLET_ITEM.captures(line))
                        .flatten()
                })
                .map(|c| c[1].trim().to_string());
            match item {
                Some(text) if part != Part::Items && says_none(&text) => open = false,
                Some(text) => {
                    list.push(text);
                    open = true;
                }
                // A line under an item continues it.
                None if open => {
                    if let Some(last) = list.last_mut() {
                        last.push(' ');
                        last.push_str(line.trim());
                    }
                }
                // A part's text that isn't a list: one item, unless it says none.
                None if part != Part::Items && !says_none(line) => {
                    list.push(line.trim().to_string());
                    open = true;
                }
                None => {}
            }
        }
        Some(out)
    }
}

/// T03: `markdown` with `body` as its "## Criteria": the criteria section
/// replaced where it has one, else put before its first `## ` section
/// (after the title and the summary), else at its end.
pub fn with_criteria(markdown: &str, body: &str) -> String {
    super::PlanDoc::parse(markdown).with_criteria(body)
}

impl super::PlanDoc {
    /// With criteria using this document's indexed structure.
    pub fn with_criteria(&self, body: &str) -> String {
        let markdown = self.text();
        let section = format!("## Criteria\n\n{}\n\n", body.trim());
        let headings: Vec<(usize, &str)> = self
            .sections()
            .iter()
            .map(|s| (s.heading.start, &markdown[s.heading.clone()]))
            .collect();
        let at = headings
            .iter()
            .position(|(_, line)| line.to_lowercase().contains("criteri"));
        let out = match (at, headings.first()) {
            (Some(i), _) => {
                let end = headings.get(i + 1).map_or(markdown.len(), |(at, _)| *at);
                format!(
                    "{}{section}{}",
                    &markdown[..headings[i].0],
                    &markdown[end..]
                )
            }
            (None, Some((first, _))) => {
                format!("{}{section}{}", &markdown[..*first], &markdown[*first..])
            }
            (None, None) => format!("{}\n\n{section}", markdown.trim_end()),
        };
        format!("{}\n", out.trim_end())
    }
}
