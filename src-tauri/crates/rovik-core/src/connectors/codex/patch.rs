//! Codex's patches, as its `apply_patch` tool takes them and reads them
//! (codex-rs/apply-patch, "apply-patch format"): the files a patch adds,
//! changes or deletes, the content each will have, and the patch made
//! over so that a file gets the content the user accepted of it. Rovik
//! asks about a patch before it's applied (codex/hooks.rs), each file as
//! Rovik's own `write` of it would be asked about.
//!
//! The reading follows Codex's own, leniency included: markers with
//! space around them, a patch inside a heredoc (`<<'EOF'`), and lines
//! found in the file as they are, then without their trailing space, then
//! trimmed, then with typographic dashes and quotes read as ASCII ones.

/// What a patch does to one file.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Hunk {
    /// A new file, its whole content.
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    /// Changes to a file, which may also move.
    Update {
        path: String,
        move_path: Option<String>,
        chunks: Vec<Chunk>,
    },
}

/// One change of an update: lines found in the file (after the line
/// `context` names, if any) and what replaces them.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Chunk {
    context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    /// The lines given as context, by their index in each side: they're
    /// kept as the file has them.
    context_lines: Vec<(usize, usize)>,
    /// Its old lines are the file's last.
    end_of_file: bool,
}

/// A patch: its hunks, and the lines each was given as.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Patch {
    pub(super) hunks: Vec<Hunk>,
    /// The patch's lines, from its first marker to its last.
    lines: Vec<String>,
    /// Each hunk's lines, by index in `lines` (start, end).
    spans: Vec<(usize, usize)>,
}

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File: ";
const DELETE: &str = "*** Delete File: ";
const UPDATE: &str = "*** Update File: ";
const MOVE: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";
const ENVIRONMENT: &str = "*** Environment ID:";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Started,
    Add,
    Delete,
    Update,
    Ended,
}

/// Reads a patch as Codex does; an error says what's wrong with it.
pub(super) fn parse(text: &str) -> Result<Patch, String> {
    let all: Vec<&str> = text.trim().lines().collect();
    let lines = within_markers(&all)?;
    let mut patch = Patch {
        hunks: Vec::new(),
        lines: lines.iter().map(|l| l.to_string()).collect(),
        spans: Vec::new(),
    };
    let mut mode = Mode::Started;
    for (n, line) in lines.iter().enumerate().skip(1) {
        mode = patch.line(mode, n, line)?;
    }
    if mode != Mode::Ended {
        return Err("The last line of the patch must be '*** End Patch'".into());
    }
    let ends: Vec<usize> = patch
        .spans
        .iter()
        .skip(1)
        .map(|(start, _)| *start)
        .chain(std::iter::once(patch.lines.len() - 1))
        .collect();
    for (span, end) in patch.spans.iter_mut().zip(ends) {
        span.1 = end;
    }
    Ok(patch)
}

/// The patch's lines from `*** Begin Patch` to `*** End Patch`, inside a
/// heredoc if it's given in one.
fn within_markers<'a>(lines: &'a [&'a str]) -> Result<&'a [&'a str], String> {
    let strict = |lines: &'a [&'a str]| -> Result<&'a [&'a str], String> {
        let first = lines.first().map(|l| l.trim());
        let last = lines.last().map(|l| l.trim());
        match (first, last) {
            (Some(BEGIN), Some(END)) if lines.len() > 1 => Ok(lines),
            (Some(first), _) if first != BEGIN => {
                Err("The first line of the patch must be '*** Begin Patch'".into())
            }
            _ => Err("The last line of the patch must be '*** End Patch'".into()),
        }
    };
    match strict(lines) {
        Ok(lines) => Ok(lines),
        Err(e) => match lines {
            [first, .., last]
                if ["<<EOF", "<<'EOF'", "<<\"EOF\""].contains(first)
                    && last.ends_with("EOF")
                    && lines.len() >= 4 =>
            {
                strict(&lines[1..lines.len() - 1])
            }
            _ => Err(e),
        },
    }
}

impl Patch {
    /// Reads the patch's line `n` in `mode`: the mode for the next line.
    fn line(&mut self, mode: Mode, n: usize, line: &str) -> Result<Mode, String> {
        let trimmed = line.trim();
        let invalid_header = || {
            format!(
                "'{trimmed}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
            )
        };
        match mode {
            Mode::Started => {
                if self.hunks.is_empty() && trimmed.starts_with(ENVIRONMENT) {
                    return Ok(Mode::Started);
                }
                self.header(n, trimmed)?.ok_or_else(invalid_header)
            }
            Mode::Add => {
                if let Some(next) = self.header(n, trimmed)? {
                    return Ok(next);
                }
                match (line.strip_prefix('+'), self.hunks.last_mut()) {
                    (Some(text), Some(Hunk::Add { contents, .. })) => {
                        contents.push_str(text);
                        contents.push('\n');
                        Ok(Mode::Add)
                    }
                    _ => Err(invalid_header()),
                }
            }
            Mode::Delete => self.header(n, trimmed)?.ok_or_else(invalid_header),
            Mode::Update => self.update_line(n, line),
            Mode::Ended if trimmed.is_empty() => Ok(Mode::Ended),
            Mode::Ended => Err("The last line of the patch must be '*** End Patch'".into()),
        }
    }

    /// A hunk's header or the patch's end, if `trimmed` is one.
    fn header(&mut self, n: usize, trimmed: &str) -> Result<Option<Mode>, String> {
        let hunk = if trimmed == END {
            None
        } else if let Some(path) = trimmed.strip_prefix(ADD) {
            let contents = String::new();
            Some((
                Hunk::Add {
                    path: path.into(),
                    contents,
                },
                Mode::Add,
            ))
        } else if let Some(path) = trimmed.strip_prefix(DELETE) {
            Some((Hunk::Delete { path: path.into() }, Mode::Delete))
        } else if let Some(path) = trimmed.strip_prefix(UPDATE) {
            let (move_path, chunks) = (None, Vec::new());
            let update = Hunk::Update {
                path: path.into(),
                move_path,
                chunks,
            };
            Some((update, Mode::Update))
        } else {
            return Ok(None);
        };
        self.last_update_has_lines(trimmed)?;
        let Some((hunk, mode)) = hunk else {
            return Ok(Some(Mode::Ended));
        };
        self.hunks.push(hunk);
        self.spans.push((n, n));
        Ok(Some(mode))
    }

    /// An update ends with lines of its own, at the next hunk or the end.
    fn last_update_has_lines(&self, at: &str) -> Result<(), String> {
        let Some(Hunk::Update { path, chunks, .. }) = self.hunks.last() else {
            return Ok(());
        };
        match chunks.last() {
            None => Err(format!("Update file hunk for path '{path}' is empty")),
            Some(chunk) if chunk.old_lines.is_empty() && chunk.new_lines.is_empty() => {
                Err(if at == END {
                    "Update hunk does not contain any lines".to_string()
                } else {
                    format!("Unexpected line found in update hunk: '{at}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)")
                })
            }
            Some(_) => Ok(()),
        }
    }

    /// A line of an update hunk.
    fn update_line(&mut self, n: usize, line: &str) -> Result<Mode, String> {
        let update = line.trim_end();
        if let Some(next) = self.header(n, update)? {
            return Ok(next);
        }
        let unexpected = || {
            format!("Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)")
        };
        let Some(Hunk::Update {
            move_path, chunks, ..
        }) = self.hunks.last_mut()
        else {
            return Err(unexpected());
        };
        let is_context = update == "@@" || update.starts_with("@@ ");
        if chunks.last().is_some_and(|c| c.end_of_file) {
            if update.is_empty() {
                return Ok(Mode::Update);
            }
            if !is_context {
                return Err(format!(
                    "Expected update hunk to start with a @@ context marker, got: '{line}'"
                ));
            }
        }
        if chunks.is_empty() && move_path.is_none() {
            if let Some(to) = update.strip_prefix(MOVE) {
                *move_path = Some(to.to_string());
                return Ok(Mode::Update);
            }
        }
        let last_is_empty = chunks
            .last()
            .is_some_and(|c| c.old_lines.is_empty() && c.new_lines.is_empty());
        if is_context {
            if last_is_empty {
                return Err(unexpected());
            }
            chunks.push(Chunk {
                context: update.strip_prefix("@@ ").map(str::to_string),
                ..Chunk::default()
            });
            return Ok(Mode::Update);
        }
        if update == END_OF_FILE {
            if last_is_empty {
                return Err("Update hunk does not contain any lines".into());
            }
            if let Some(chunk) = chunks.last_mut() {
                chunk.end_of_file = true;
            }
            return Ok(Mode::Update);
        }
        // A blank line is a blank line of context.
        let (kind, text) = match line.chars().next() {
            None => (' ', ""),
            Some(c @ (' ' | '+' | '-')) => (c, &line[1..]),
            Some(_)
                if chunks
                    .last()
                    .is_some_and(|c| !c.old_lines.is_empty() || !c.new_lines.is_empty()) =>
            {
                return Err(format!(
                    "Expected update hunk to start with a @@ context marker, got: '{line}'"
                ))
            }
            Some(_) => return Err(unexpected()),
        };
        if chunks.is_empty() {
            chunks.push(Chunk::default());
        }
        let Some(chunk) = chunks.last_mut() else {
            return Err(unexpected());
        };
        match kind {
            ' ' => {
                chunk
                    .context_lines
                    .push((chunk.old_lines.len(), chunk.new_lines.len()));
                chunk.old_lines.push(text.to_string());
                chunk.new_lines.push(text.to_string());
            }
            '+' => chunk.new_lines.push(text.to_string()),
            _ => chunk.old_lines.push(text.to_string()),
        }
        Ok(Mode::Update)
    }

    /// The patch with some files made to have the content the user
    /// accepted of them: each of `writes` (a hunk's index, the file's
    /// content now, and the content it gets) becomes a hunk of its own,
    /// the file whole, which Codex applies to the file as it is now. The
    /// other hunks stay as they were given.
    pub(super) fn rewritten(&self, writes: &[(usize, String, String)]) -> String {
        let mut out = vec![BEGIN.to_string()];
        // What came before the first hunk (an environment's line).
        let first = self.spans.first().map_or(self.lines.len() - 1, |s| s.0);
        out.extend(self.lines[1..first].iter().cloned());
        for (i, hunk) in self.hunks.iter().enumerate() {
            match writes.iter().find(|(at, ..)| *at == i) {
                Some((_, current, content)) => out.extend(whole_file(hunk, current, content)),
                None => {
                    let (start, end) = self.spans[i];
                    out.extend(self.lines[start..end].iter().cloned());
                }
            }
        }
        out.push(END.to_string());
        out.join("\n")
    }
}

/// A hunk that gives `hunk`'s file `content`, from `current`: a new file
/// whole, or every line of the file replaced (where it moves, it still
/// does).
fn whole_file(hunk: &Hunk, current: &str, content: &str) -> Vec<String> {
    let added = content.lines().map(|line| format!("+{line}"));
    match hunk {
        Hunk::Add { path, .. } => std::iter::once(format!("{ADD}{path}"))
            .chain(added)
            .collect(),
        Hunk::Delete { path } | Hunk::Update { path, .. } => {
            let mut lines = vec![format!("{UPDATE}{path}")];
            if let Hunk::Update {
                move_path: Some(to),
                ..
            } = hunk
            {
                lines.push(format!("{MOVE}{to}"));
            }
            lines.push("@@".into());
            lines.extend(
                SourceFile::parse(current)
                    .texts()
                    .into_iter()
                    .map(|line| format!("-{line}")),
            );
            lines.extend(added);
            lines
        }
    }
}

/// The content a file has once `chunks` are applied to `current`, as
/// Codex applies them; an error when a chunk's lines aren't in it.
pub(super) fn new_contents(current: &str, chunks: &[Chunk]) -> Result<String, String> {
    let mut file = SourceFile::parse(current);
    let lines = file.texts();
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut at = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.context {
            match seek(&lines, std::slice::from_ref(context), at, false) {
                Some(found) => at = found + 1,
                None => return Err(format!("Failed to find context '{context}'")),
            }
        }
        if chunk.old_lines.is_empty() {
            replacements.push((lines.len(), 0, chunk.new_lines.clone()));
            continue;
        }
        let mut pattern: &[String] = &chunk.old_lines;
        let mut new: &[String] = &chunk.new_lines;
        let mut found = seek(&lines, pattern, at, chunk.end_of_file);
        // A last empty line stands for the file's final newline, which the
        // file's lines don't have as a line of its own.
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new.last().is_some_and(String::is_empty) {
                new = &new[..new.len() - 1];
            }
            found = seek(&lines, pattern, at, chunk.end_of_file);
        }
        let Some(start) = found else {
            return Err(format!(
                "Failed to find expected lines:\n{}",
                chunk.old_lines.join("\n")
            ));
        };
        // Context lines stay as the file has them.
        let (mut old_from, mut new_from) = (0, 0);
        for &(old_context, new_context) in &chunk.context_lines {
            if old_context >= pattern.len() || new_context >= new.len() {
                break;
            }
            if old_from != old_context || new_from != new_context {
                replacements.push((
                    start + old_from,
                    old_context - old_from,
                    new[new_from..new_context].to_vec(),
                ));
            }
            old_from = old_context + 1;
            new_from = new_context + 1;
        }
        if old_from != pattern.len() || new_from != new.len() {
            replacements.push((
                start + old_from,
                pattern.len() - old_from,
                new[new_from..].to_vec(),
            ));
        }
        at = start + pattern.len();
    }
    replacements.sort_by_key(|(index, ..)| *index);
    file.replace(&replacements);
    Ok(file.contents())
}

/// Where `pattern` is in `lines`, from `start`: the same lines, then
/// without trailing space, then trimmed, then with typographic dashes,
/// quotes and spaces read as ASCII. `eof` looks at the end first.
fn seek(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let from = if eof {
        (lines.len() - pattern.len()).max(start)
    } else {
        start
    };
    let last = lines.len() - pattern.len();
    let same = |eq: &dyn Fn(&str, &str) -> bool| {
        (from..=last).find(|&i| {
            pattern
                .iter()
                .enumerate()
                .all(|(j, p)| eq(&lines[i + j], p))
        })
    };
    same(&|a, b| a == b)
        .or_else(|| same(&|a, b| a.trim_end() == b.trim_end()))
        .or_else(|| same(&|a, b| a.trim() == b.trim()))
        .or_else(|| same(&|a, b| ascii(a) == ascii(b)))
}

/// A line trimmed, with typographic dashes, quotes and spaces as ASCII.
fn ascii(line: &str) -> String {
    line.trim()
        .chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{2018}'..='\u{201B}' => '\'',
            '\u{201C}'..='\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            c => c,
        })
        .collect()
}

/// A file's lines, each with the line ending it had.
struct SourceFile {
    lines: Vec<(String, Option<&'static str>)>,
    /// The first ending the file has, for lines a patch adds.
    ending: &'static str,
}

impl SourceFile {
    fn parse(contents: &str) -> SourceFile {
        let mut lines = Vec::new();
        let mut ending = None;
        let bytes = contents.as_bytes();
        let (mut start, mut at) = (0, 0);
        while at < bytes.len() {
            let (this, len) = match bytes[at] {
                b'\r' if bytes.get(at + 1) == Some(&b'\n') => ("\r\n", 2),
                b'\r' => ("\r", 1),
                b'\n' => ("\n", 1),
                _ => {
                    at += 1;
                    continue;
                }
            };
            ending.get_or_insert(this);
            lines.push((contents[start..at].to_string(), Some(this)));
            at += len;
            start = at;
        }
        if start < contents.len() {
            lines.push((contents[start..].to_string(), None));
        }
        SourceFile {
            lines,
            ending: ending.unwrap_or("\n"),
        }
    }

    fn texts(&self) -> Vec<String> {
        self.lines.iter().map(|(text, _)| text.clone()).collect()
    }

    /// Applies replacements in the file's order; every line ends with an
    /// ending afterwards, as Codex's updates have.
    fn replace(&mut self, replacements: &[(usize, usize, Vec<String>)]) {
        let mut source = std::mem::take(&mut self.lines).into_iter();
        let mut out = Vec::new();
        let mut index = 0;
        for (start, len, new) in replacements {
            out.extend(source.by_ref().take(start.saturating_sub(index)));
            source.by_ref().take(*len).for_each(drop);
            out.extend(new.iter().map(|text| (text.clone(), Some(self.ending))));
            index = start + len;
        }
        out.extend(source);
        for line in &mut out {
            line.1.get_or_insert(self.ending);
        }
        self.lines = out;
    }

    fn contents(self) -> String {
        self.lines
            .into_iter()
            .map(|(text, ending)| text + ending.unwrap_or(""))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(patch: &Patch, n: usize) -> &[Chunk] {
        match &patch.hunks[n] {
            Hunk::Update { chunks, .. } => chunks,
            other => panic!("not an update: {other:?}"),
        }
    }

    #[test]
    fn a_patch_reads_as_its_files_adds_deletes_updates_and_moves() {
        let patch = parse(
            "*** Begin Patch\n*** Add File: path/add.py\n+abc\n+def\n*** Delete File: path/delete.py\n*** Update File: path/update.py\n*** Move to: path/update2.py\n@@ def f():\n-    pass\n+    return 123\n*** End Patch",
        )
        .unwrap();
        assert_eq!(
            patch.hunks[0],
            Hunk::Add {
                path: "path/add.py".into(),
                contents: "abc\ndef\n".into()
            }
        );
        assert_eq!(
            patch.hunks[1],
            Hunk::Delete {
                path: "path/delete.py".into()
            }
        );
        let Hunk::Update {
            path, move_path, ..
        } = &patch.hunks[2]
        else {
            panic!("an update");
        };
        assert_eq!(
            (path.as_str(), move_path.as_deref()),
            ("path/update.py", Some("path/update2.py"))
        );
        assert_eq!(chunks(&patch, 2)[0].context.as_deref(), Some("def f():"));
    }

    #[test]
    fn a_patch_is_read_as_leniently_as_codex_reads_it() {
        // Space around the markers, and a heredoc around the patch.
        let spaced = parse("*** Begin Patch \n*** Add File: a\n+hi\n *** End Patch").unwrap();
        assert_eq!(spaced.hunks.len(), 1);
        let heredoc =
            parse("<<'EOF'\n*** Begin Patch\n*** Delete File: a\n*** End Patch\nEOF\n").unwrap();
        assert_eq!(heredoc.hunks, vec![Hunk::Delete { path: "a".into() }]);
        // A first update chunk without its `@@`.
        let bare =
            parse("*** Begin Patch\n*** Update File: f\n import foo\n+bar\n*** End Patch").unwrap();
        assert_eq!(chunks(&bare, 0)[0].old_lines, vec!["import foo"]);
    }

    #[test]
    fn a_broken_patch_says_what_is_wrong() {
        assert!(parse("bad").unwrap_err().contains("first line"));
        assert!(parse("*** Begin Patch\nbad")
            .unwrap_err()
            .contains("last line"));
        assert!(
            parse("*** Begin Patch\n*** Update File: t.py\n*** End Patch")
                .unwrap_err()
                .contains("is empty")
        );
        assert!(parse("*** Begin Patch\n*** Frob File: x\n*** End Patch")
            .unwrap_err()
            .contains("not a valid hunk header"));
    }

    #[test]
    fn an_update_makes_the_content_codex_would() {
        let patch = parse(
            "*** Begin Patch\n*** Update File: f\n@@ fn main() {\n-    old();\n+    new();\n     keep();\n*** End Patch",
        )
        .unwrap();
        let now = "use x;\nfn main() {\n    old();\n    keep();\n}\n";
        assert_eq!(
            new_contents(now, chunks(&patch, 0)).unwrap(),
            "use x;\nfn main() {\n    new();\n    keep();\n}\n"
        );
        // Lines found though their spaces or quotes differ; the file's own
        // line endings kept, the new lines taking them.
        let fuzzy = parse(
            "*** Begin Patch\n*** Update File: f\n@@\n-say \"hi\"\n+say \"bye\"\n*** End Patch",
        )
        .unwrap();
        assert_eq!(
            new_contents(
                "a\r\n  say \u{201C}hi\u{201D}  \r\nz\r\n",
                chunks(&fuzzy, 0)
            )
            .unwrap(),
            "a\r\nsay \"bye\"\r\nz\r\n"
        );
        // Lines the file doesn't have.
        assert!(new_contents("other\n", chunks(&patch, 0)).is_err());
    }

    #[test]
    fn added_lines_with_no_old_ones_go_at_the_end() {
        let patch = parse("*** Begin Patch\n*** Update File: f\n@@\n+last\n*** End Patch").unwrap();
        assert_eq!(
            new_contents("first", chunks(&patch, 0)).unwrap(),
            "first\nlast\n"
        );
    }

    #[test]
    fn a_rewritten_patch_gives_a_file_the_accepted_content_and_keeps_the_rest() {
        let patch = parse(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+uno\n-two\n+dos\n*** Add File: b.txt\n+bee\n*** End Patch",
        )
        .unwrap();
        let text = patch.rewritten(&[(0, "one\ntwo\n".into(), "uno\ntwo\n".into())]);
        assert_eq!(
            text,
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n-two\n+uno\n+two\n*** Add File: b.txt\n+bee\n*** End Patch"
        );
        // Codex reads it back, and it makes what was accepted.
        let again = parse(&text).unwrap();
        assert_eq!(
            new_contents("one\ntwo\n", chunks(&again, 0)).unwrap(),
            "uno\ntwo\n"
        );
        assert_eq!(again.hunks[1], patch.hunks[1]);
        // A new file, part of it.
        let added = patch.rewritten(&[(1, String::new(), "b\n".into())]);
        assert!(
            added.ends_with("*** Add File: b.txt\n+b\n*** End Patch"),
            "{added}"
        );
    }
}
