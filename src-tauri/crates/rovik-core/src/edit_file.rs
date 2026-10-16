//! The text replacement behind the agent's `edit` tool.
//!
//! The model quotes the exact text it wants to change (`old_text`) and gives
//! the replacement (`new_text`). The quote has to match exactly once, so an
//! edit either lands where the model meant or fails loudly with a reason it
//! can act on; it never silently lands somewhere else. With a line range
//! (an editor selection the user referenced), the quote must also lie inside
//! those lines: nothing outside them can change.
//!
//! Pure string work, no file access, so it's tested on its own below.

/// Where an edit landed, 1-based lines.
#[derive(Debug, PartialEq, Eq)]
pub struct EditSpan {
    pub start_line: usize,
    /// Lines the replaced text spanned.
    pub old_lines: usize,
    /// Lines the replacement spans (0 when text was deleted).
    pub new_lines: usize,
}

/// Lines `text` spans, a trailing newline not starting a new one.
fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.matches('\n').count() + usize::from(!text.ends_with('\n'))
}

/// Byte offset where each line starts.
fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(content.match_indices('\n').map(|(i, _)| i + 1));
    // A final newline doesn't start another line.
    if starts.len() > 1 && *starts.last().unwrap() == content.len() {
        starts.pop();
    }
    starts
}

/// The lines of a range, numbered like read's line ranges, for errors.
fn numbered(content: &str, start: usize, end: usize) -> String {
    const MAX: usize = 40;
    let lines: Vec<&str> = content.lines().collect();
    let last = end.min(lines.len()).min(start + MAX - 1);
    let mut out: Vec<String> = (start..=last)
        .map(|n| format!("{n:>5} | {}", lines[n - 1].trim_end_matches('\r')))
        .collect();
    if last < end.min(lines.len()) {
        out.push(format!(
            "      … ({} more lines)",
            end.min(lines.len()) - last
        ));
    }
    out.join("\n")
}

/// Replaces the one occurrence of `old_text` in `content` (inside lines
/// `start_line..=end_line` when given) with `new_text`.
pub fn apply_edit(
    content: &str,
    old_text: &str,
    new_text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<(String, EditSpan), String> {
    if old_text.is_empty() {
        return Err(
            "old_text must not be empty. To insert text, put an existing line next to \
                    the insertion point in old_text and repeat it in new_text with the new lines."
                .into(),
        );
    }
    if old_text == new_text {
        return Err("old_text and new_text are the same: nothing would change.".into());
    }

    // Models write "\n". Match a CRLF file's line endings, and keep them.
    let crlf = content.contains("\r\n");
    let (old, new) = if crlf && !old_text.contains('\r') {
        (
            old_text.replace('\n', "\r\n"),
            if new_text.contains('\r') {
                new_text.to_string()
            } else {
                new_text.replace('\n', "\r\n")
            },
        )
    } else {
        (old_text.to_string(), new_text.to_string())
    };

    let starts = line_starts(content);
    let total_lines = line_count(content);

    // The part of the file the edit may touch.
    let range = match (start_line, end_line) {
        (None, None) => None,
        (start, end) => {
            let s = start.or(end).unwrap();
            let e = end.or(start).unwrap();
            if s == 0 || e < s {
                return Err(format!(
                    "invalid line range {s}-{e}: lines are 1-based and start_line must not be after end_line."
                ));
            }
            if e > total_lines {
                return Err(format!(
                    "line range {s}-{e} is past the end of the file, which has {total_lines} lines. Read it again."
                ));
            }
            let from = starts[s - 1];
            // Through the end of line e, its line break included.
            let to = starts.get(e).copied().unwrap_or(content.len());
            Some((s, e, from, to))
        }
    };
    let (from, to) = range
        .map(|(_, _, f, t)| (f, t))
        .unwrap_or((0, content.len()));
    let region = &content[from..to];

    let found: Vec<usize> = region
        .match_indices(old.as_str())
        .map(|(i, _)| i + from)
        .collect();

    let at = match found.as_slice() {
        [one] => *one,
        [] => {
            return Err(match range {
                Some((s, e, _, _)) => {
                    let elsewhere = content
                        .match_indices(old.as_str())
                        .map(|(i, _)| content[..i].matches('\n').count() + 1)
                        .collect::<Vec<_>>();
                    let hint = if elsewhere.is_empty() {
                        String::new()
                    } else {
                        let lines: Vec<String> = elsewhere.iter().map(|l| l.to_string()).collect();
                        format!(
                            " It does appear outside that range (line {}), which this edit may not change.",
                            lines.join(", ")
                        )
                    };
                    format!(
                        "old_text was not found in lines {s}-{e}.{hint} Copy it exactly, \
                         indentation and whitespace included. Lines {s}-{e} currently are:\n{}",
                        numbered(content, s, e)
                    )
                }
                None => "old_text was not found in the file. Copy it exactly, indentation and \
                         whitespace included; read the lines again if the file may have changed."
                    .into(),
            })
        }
        many => {
            let lines: Vec<String> = many
                .iter()
                .map(|&i| (content[..i].matches('\n').count() + 1).to_string())
                .collect();
            return Err(format!(
                "old_text matches {} places (lines {}). Include more surrounding lines so it \
                 matches only one, or pass start_line and end_line.",
                many.len(),
                lines.join(", ")
            ));
        }
    };

    let mut result = String::with_capacity(content.len() + new.len());
    result.push_str(&content[..at]);
    result.push_str(&new);
    result.push_str(&content[at + old.len()..]);

    Ok((
        result,
        EditSpan {
            start_line: content[..at].matches('\n').count() + 1,
            old_lines: line_count(&old),
            new_lines: line_count(&new),
        },
    ))
}

/// The tool result the model reads after a successful edit.
pub fn describe_edit(path: &str, span: &EditSpan) -> String {
    let EditSpan {
        start_line: s,
        old_lines,
        new_lines,
    } = *span;
    let old = if old_lines <= 1 {
        format!("line {s}")
    } else {
        format!("lines {s}-{}", s + old_lines - 1)
    };
    if new_lines == 0 {
        return format!("Edited {path}: removed {old}.");
    }
    let now = if new_lines == 1 {
        format!("line {s}")
    } else {
        format!("lines {s}-{}", s + new_lines - 1)
    };
    let shift = new_lines as isize - old_lines as isize;
    let after = if shift == 0 {
        String::new()
    } else {
        format!(" Lines after it moved by {shift:+}; use the new line numbers for further edits.")
    };
    format!("Edited {path}: replaced {old} ({old_lines} → {new_lines} lines), now {now}.{after}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "fn a() {\n    one();\n}\n\nfn b() {\n    one();\n}\n";

    #[test]
    fn replaces_a_unique_match() {
        let (out, span) = apply_edit(FILE, "fn a() {", "fn alpha() {", None, None).unwrap();
        assert!(out.starts_with("fn alpha() {\n"));
        assert_eq!(
            span,
            EditSpan {
                start_line: 1,
                old_lines: 1,
                new_lines: 1
            }
        );
    }

    #[test]
    fn ambiguous_match_is_refused_and_says_where() {
        let err = apply_edit(FILE, "    one();", "    two();", None, None).unwrap_err();
        assert!(
            err.contains("2 places") && err.contains("lines 2, 6"),
            "{err}"
        );
    }

    #[test]
    fn line_range_picks_the_occurrence_and_confines_the_edit() {
        let (out, span) = apply_edit(FILE, "    one();", "    two();", Some(5), Some(7)).unwrap();
        assert_eq!(out, "fn a() {\n    one();\n}\n\nfn b() {\n    two();\n}\n");
        assert_eq!(span.start_line, 6);
    }

    #[test]
    fn text_outside_the_range_is_refused_with_a_pointer() {
        let err = apply_edit(FILE, "fn a() {", "x", Some(5), Some(7)).unwrap_err();
        assert!(err.contains("outside that range (line 1)"), "{err}");
        assert!(err.contains("    5 | fn b() {"), "{err}");
    }

    #[test]
    fn a_match_must_not_run_past_the_range() {
        // "}\n\nfn b" starts in line 3 but continues into lines 4-5.
        let err = apply_edit(FILE, "}\n\nfn b", "x", Some(3), Some(3)).unwrap_err();
        assert!(err.contains("not found in lines 3-3"), "{err}");
    }

    #[test]
    fn whole_range_with_trailing_newline_matches() {
        let (out, span) = apply_edit(
            FILE,
            "fn b() {\n    one();\n}\n",
            "fn b() {}\n",
            Some(5),
            Some(7),
        )
        .unwrap();
        assert_eq!(out, "fn a() {\n    one();\n}\n\nfn b() {}\n");
        assert_eq!(
            span,
            EditSpan {
                start_line: 5,
                old_lines: 3,
                new_lines: 1
            }
        );
    }

    #[test]
    fn single_line_range_and_last_line_without_newline() {
        let content = "a\nb\nc";
        let (out, _) = apply_edit(content, "c", "C", Some(3), None).unwrap();
        assert_eq!(out, "a\nb\nC");
        let err = apply_edit(content, "c", "C", Some(4), Some(4)).unwrap_err();
        assert!(err.contains("has 3 lines"), "{err}");
    }

    #[test]
    fn crlf_files_keep_crlf() {
        let content = "one\r\ntwo\r\nthree\r\n";
        let (out, _) = apply_edit(content, "two\nthree", "2\n3", None, None).unwrap();
        assert_eq!(out, "one\r\n2\r\n3\r\n");
    }

    #[test]
    fn deleting_and_describing() {
        let (out, span) = apply_edit(FILE, "\nfn b() {\n    one();\n}\n", "", None, None).unwrap();
        assert_eq!(out, "fn a() {\n    one();\n}\n");
        assert_eq!(
            describe_edit("x.rs", &span),
            "Edited x.rs: removed lines 4-7."
        );
        let (_, span) = apply_edit(
            FILE,
            "    one();\n}\n\n",
            "    one();\n    one();\n}\n\n",
            Some(1),
            Some(4),
        )
        .unwrap();
        assert!(describe_edit("x.rs", &span).contains("moved by +1"));
    }

    #[test]
    fn empty_and_noop_edits_are_refused() {
        assert!(apply_edit(FILE, "", "x", None, None).is_err());
        assert!(apply_edit(FILE, "fn a", "fn a", None, None).is_err());
        assert!(apply_edit(FILE, "fn a", "x", Some(3), Some(2)).is_err());
    }

    #[test]
    fn multibyte_text_is_sliced_on_char_boundaries() {
        let content = "привет\nмир\n";
        let (out, span) = apply_edit(content, "мир", "свет", Some(2), Some(2)).unwrap();
        assert_eq!(out, "привет\nсвет\n");
        assert_eq!(span.start_line, 2);
    }
}
