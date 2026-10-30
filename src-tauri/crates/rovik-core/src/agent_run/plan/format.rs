//! Changes to a plan's text: its findings left out (they're the
//! reviewer's), mechanical format fixes (Balanced Autopilot), and its frame
//! for a plan sent back for its form.

use super::check::{files_named, TITLE, VALIDATION_HEADING};
use super::criteria::criteria;
use super::steps::{
    file_path, is_fence, lines_with_fences, section_after, split_items, step_files, steps,
    steps_heading, BadPath, FILES_LINE, NEW_MARK, NEXT_HEADING,
};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// A findings heading an agent may put in its plan (`## Findings`,
/// `## Review findings`), or the IDE's `## Open review findings` from an
/// earlier save: findings are the reviewer's, never the plan's.
static FINDINGS_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^##\s+(?:open\s+)?(?:review\s+)?findings\s*:?\s*$").unwrap());

/// The plan without its findings sections (an agent revising a reviewed
/// plan sometimes ends it with "## Findings" and "None.", or keeps the
/// open findings the IDE appended), and whether any was taken out.
pub fn without_findings(markdown: &str) -> (String, bool) {
    let crlf = markdown.contains("\r\n");
    let text = markdown.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    let mut skipping = false;
    let mut dropped = false;
    for (_, line, fenced) in lines_with_fences(&text) {
        if !fenced && NEXT_HEADING.is_match(line) {
            skipping = FINDINGS_HEADING.is_match(line.trim_end());
            dropped |= skipping;
        }
        if !skipping {
            out.push_str(line);
        }
    }
    if !dropped {
        return (markdown.to_string(), false);
    }
    let out = out.trim_end().to_string() + "\n";
    (if crlf { out.replace('\n', "\r\n") } else { out }, true)
}

/// The lines of the steps section that are `Files:` lines or the list
/// lines under one, each with its byte offset in `text` and where its
/// items start in the line.
fn files_lines(text: &str) -> Vec<(usize, usize, usize)> {
    let Some(heading) = steps_heading(text) else {
        return Vec::new();
    };
    let start = heading.end();
    let end = start + section_after(text, start).len();
    let lines: Vec<(usize, &str, bool)> = lines_with_fences(text)
        .into_iter()
        .filter(|(at, _, _)| *at >= start && *at < end)
        .collect();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let (at, line, fenced) = lines[i];
        i += 1;
        let Some(c) = (!fenced).then(|| FILES_LINE.captures(line)).flatten() else {
            continue;
        };
        let body = line.trim_end_matches('\n');
        out.push((
            at,
            body.len(),
            c.get(1).map_or(body.len(), |m| m.start().min(body.len())),
        ));
        while i < lines.len() {
            let (at, next, fenced) = lines[i];
            let t = next.trim();
            if fenced
                || t.is_empty()
                || indent(next) <= indent(line)
                || !t.starts_with(['-', '*', '+', '`'])
            {
                break;
            }
            let body = next.trim_end_matches('\n');
            out.push((at, body.len(), indent(next)));
            i += 1;
        }
    }
    out
}

/// The tokens of a `Files:` line's items as `parse_files` reads them:
/// (the token, whether it's in backticks, the text after it).
fn file_tokens(items: &str) -> Vec<(String, bool, String)> {
    let mut out = Vec::new();
    for item in split_items(items) {
        let item = item.trim_start_matches(['-', '*', '+']).trim();
        let ticks: Vec<usize> = item.match_indices('`').map(|(i, _)| i).collect();
        if ticks.len() >= 2 {
            for (n, pair) in ticks.chunks(2).enumerate() {
                let [open, close] = pair else { break };
                let after_end = ticks.get(2 * (n + 1)).copied().unwrap_or(item.len());
                out.push((
                    item[open + 1..*close].trim().to_string(),
                    true,
                    item[close + 1..after_end].to_string(),
                ));
            }
        } else {
            let plain = item.trim_matches(|c| c == '*' || c == '_').trim();
            let (token, rest) = plain.find('(').map_or((plain, ""), |at| plain.split_at(at));
            let token = token.trim();
            let token = token
                .strip_suffix('.')
                .filter(|t| !t.ends_with('.'))
                .unwrap_or(token);
            out.push((token.to_string(), false, rest.to_string()));
        }
    }
    out
}

/// Balanced Autopilot: the plan with its mechanical format problems fixed,
/// and one line per fix. Only these, never anything a reviewer found:
/// inside `Files:` lines, a `../x` path that resolves inside the project
/// becomes that path and a bare file name missing at the root becomes the
/// one path the project has for it; a missing **Out of scope** or
/// **Assumptions** part under "## Criteria" becomes "None."; a code block
/// left open at the end is closed; a missing "# " title is taken from the
/// request's first line.
pub fn normalize(
    markdown: &str,
    root: Option<&Path>,
    request: Option<&str>,
) -> (String, Vec<String>) {
    let mut text = markdown.replace("\r\n", "\n");
    let mut fixes = Vec::new();
    if text.trim().is_empty() {
        return (markdown.to_string(), fixes);
    }
    // A code block left open runs to the end.
    if text.lines().filter(|l| is_fence(l)).count() % 2 == 1 {
        let fence = text.lines().rfind(|l| is_fence(l)).map_or("```", |l| {
            if l.trim_start().starts_with("~~~") {
                "~~~"
            } else {
                "```"
            }
        });
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(fence);
        text.push('\n');
        fixes.push("closed the code block left open at the end".to_string());
    }
    // Inside `Files:` lines, token by token.
    let lines = files_lines(&text);
    let tokens: Vec<Vec<(String, bool, String)>> = lines
        .iter()
        .map(|&(at, len, items)| file_tokens(&text[at + items..at + len]))
        .collect();
    let bare: Vec<String> = match root {
        Some(root) => tokens
            .iter()
            .flatten()
            .filter(|(_, _, after)| !NEW_MARK.is_match(after))
            .filter_map(|(token, _, _)| file_path(token).ok())
            .filter(|p| !p.contains('/') && !root.join(p).exists())
            .collect(),
        None => Vec::new(),
    };
    let located = match root {
        Some(root) if !bare.is_empty() => {
            files_named(root, &bare.iter().map(String::as_str).collect::<Vec<_>>())
        }
        _ => Vec::new(),
    };
    // From the last line back, so earlier offsets hold.
    for (i, &(at, len, items)) in lines.iter().enumerate().rev() {
        let mut line = text[at + items..at + len].to_string();
        let mut changed = false;
        for (token, ticked, after) in &tokens[i] {
            let fixed = match file_path(token) {
                Err(BadPath::Outside(Some(inside))) if !inside.is_empty() => Some(inside),
                Ok(path) if bare.contains(&path) && !NEW_MARK.is_match(after) => located
                    .iter()
                    .find(|(name, _)| *name == path)
                    .and_then(|(_, paths)| match paths.as_slice() {
                        [one] => Some(one.clone()),
                        _ => None,
                    }),
                _ => None,
            };
            let Some(fixed) = fixed else { continue };
            let (from, to) = match ticked {
                true => (format!("`{token}`"), format!("`{fixed}`")),
                false => (token.clone(), fixed.clone()),
            };
            if let Some(pos) = line.find(&from) {
                line.replace_range(pos..pos + from.len(), &to);
                changed = true;
                fixes.push(format!("`{token}` → `{fixed}` in a Files line"));
            }
        }
        if changed {
            text.replace_range(at + items..at + len, &line);
        }
    }
    // The criteria's two parts.
    if let Some(c) = criteria(&text) {
        let mut parts = Vec::new();
        if !c.has_out_of_scope {
            parts.push("**Out of scope:** None.");
        }
        if !c.has_assumptions {
            parts.push("**Assumptions:** None.");
        }
        let headings: Vec<usize> = lines_with_fences(&text)
            .into_iter()
            .filter(|(_, line, fenced)| !fenced && NEXT_HEADING.is_match(line))
            .map(|(at, _, _)| at)
            .collect();
        let at = headings.iter().position(|&h| {
            let line = text[h..].lines().next().unwrap_or("");
            line.to_lowercase().contains("criteri")
        });
        if let (false, Some(i)) = (parts.is_empty(), at) {
            let end = headings.get(i + 1).copied().unwrap_or(text.len());
            let before = text[..end].trim_end();
            let after = &text[end..];
            let joint = if after.is_empty() { "\n" } else { "\n\n" };
            text = format!("{before}\n\n{}{joint}{after}", parts.join("\n\n"));
            for p in parts {
                fixes.push(format!("added {p} under \"## Criteria\""));
            }
        }
    }
    // The title, from the request.
    let first = text.lines().find(|l| !l.trim().is_empty());
    if !first.is_some_and(|l| TITLE.is_match(l)) {
        let title = request
            .and_then(|r| r.lines().map(str::trim).find(|l| !l.is_empty()))
            .map(|l| l.trim_start_matches('#').trim())
            .filter(|l| !l.is_empty())
            .map(|l| match l.char_indices().nth(100) {
                Some((cut, _)) => format!("{}…", l[..cut].trim_end()),
                None => l.to_string(),
            });
        if let Some(title) = title {
            text = format!("# {title}\n\n{}", text.trim_start_matches('\n'));
            fixes.push(format!("added the title \"# {title}\" from the request"));
        }
    }
    if fixes.is_empty() {
        return (markdown.to_string(), fixes);
    }
    if markdown.contains("\r\n") {
        text = text.replace('\n', "\r\n");
    }
    (text, fixes)
}

/// Balanced Autopilot, after repeated format rejections: the plan's frame
/// to fill in. Its "# " title, its steps with their `Files:` lines, and
/// every section it lacks, with `TODO:` where the model must write.
pub fn skeleton(markdown: &str) -> String {
    let text = markdown.replace("\r\n", "\n");
    let mut out = String::new();
    match text.lines().map(str::trim).find(|l| !l.is_empty()) {
        Some(l) if TITLE.is_match(l) => out.push_str(l),
        _ => out.push_str("# TODO: title"),
    }
    out.push_str("\n\nTODO: a one-paragraph summary of the approach.\n\n## Steps\n\n");
    let steps = steps(&text);
    if steps.is_empty() {
        for n in 1..=2 {
            out.push_str(&format!(
                "{n}. TODO: step title\n   Files: TODO: `path/from/the/project/root` (new files marked \"(new)\")\n   TODO: what to add, change or remove there, by name.\n"
            ));
        }
    }
    for s in &steps {
        let files = step_files(&s.body);
        let mut names: Vec<String> = files
            .files
            .iter()
            .map(|f| {
                let mark = if f.new {
                    " (new)"
                } else if f.read_only {
                    " (read)"
                } else {
                    ""
                };
                format!("`{}`{mark}", f.path)
            })
            .collect();
        names.extend(files.bad.iter().map(|(t, _)| format!("TODO: fix `{t}`")));
        let title = if s.title.is_empty() {
            "TODO: step title"
        } else {
            &s.title
        };
        let files = if names.is_empty() {
            "TODO: `path/from/the/project/root`".to_string()
        } else {
            names.join(", ")
        };
        out.push_str(&format!(
            "{}. {title}\n   Files: {files}\n   TODO: what changes there, by name (as you wrote it).\n",
            s.number
        ));
    }
    let steps_at = steps_heading(&text).map(|h| h.start());
    let has_validation = VALIDATION_HEADING
        .find_iter(&text)
        .any(|h| Some(h.start()) != steps_at && !section_after(&text, h.end()).trim().is_empty());
    out.push_str("\n## Validation\n\n");
    if has_validation {
        out.push_str("(as you wrote it)\n");
    } else {
        out.push_str("TODO: the commands to run (build, typecheck, tests, lint), the tests to add or update, and what to check by hand.\n");
    }
    out.push_str("\n## Criteria\n\n");
    match criteria(&text) {
        None => out.push_str(
            "1. TODO: what done means, each a statement a test or a reading of the result can check.\n\n**Out of scope:** TODO, or None.\n\n**Assumptions:** TODO, or None.\n",
        ),
        Some(c) => {
            if c.items.is_empty() {
                out.push_str("1. TODO: what done means, as numbered statements.\n");
            } else {
                out.push_str("(your numbered criteria, as you wrote them)\n");
            }
            out.push_str(if c.has_out_of_scope {
                "\n**Out of scope:** (as you wrote it)\n"
            } else {
                "\n**Out of scope:** TODO, or None.\n"
            });
            out.push_str(if c.has_assumptions {
                "\n**Assumptions:** (as you wrote it)\n"
            } else {
                "\n**Assumptions:** TODO, or None.\n"
            });
        }
    }
    out
}
