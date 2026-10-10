//! Three-way merge of text held in memory.
//!
//! libgit2's file merge works on plain buffers: it needs no repository and
//! no `git` program, so the same merge serves the Git panel's conflicts
//! (git_sync.rs), approvals whose file changed while they waited
//! (agent_run/execute.rs) and, later, chats' changes landing in the project.
//!
//! The merge is diff3-style with long markers; [`parse_merged`] turns the
//! result into segments, the form ConflictResolver.tsx shows.

use git2::{MergeFileInput, MergeFileOptions};
use serde::Serialize;

/// A run of the merged file: lines both sides agree on, or a conflict.
#[derive(Debug, Serialize, PartialEq, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MergeSegment {
    Common {
        text: String,
    },
    Conflict {
        ours: String,
        base: String,
        theirs: String,
    },
}

/// What merging two changed versions of one file gave.
#[derive(Debug, PartialEq)]
pub enum Merged {
    /// Both sides' changes applied: the new file.
    Clean(String),
    /// The sides changed the same lines: the merged file in segments.
    Conflict(Vec<MergeSegment>),
}

/// Long conflict markers, so lines of the files themselves can't pass for one.
pub const MARKER: usize = 32;

/// Merges `ours` and `theirs`, both changed from `base`. Bytes that aren't
/// UTF-8 are replaced: callers check for binary content first.
pub fn merge3(base: &str, ours: &str, theirs: &str) -> Result<Merged, String> {
    let raw = merge3_raw(base.as_bytes(), ours.as_bytes(), theirs.as_bytes())?;
    let text = String::from_utf8_lossy(&raw.content).into_owned();
    if raw.automergeable {
        Ok(Merged::Clean(text))
    } else {
        Ok(Merged::Conflict(parse_merged(&text)))
    }
}

/// libgit2's merge output, markers included.
pub struct RawMerge {
    pub content: Vec<u8>,
    pub automergeable: bool,
}

pub fn merge3_raw(base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<RawMerge, String> {
    // libgit2 must be initialised, which git2 does in its constructors but
    // not for a file merge: without a repository opened first, the merge
    // fails with "no error". Any constructor will do.
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| drop(git2::Config::new()));
    let mut a = MergeFileInput::new();
    a.content(base);
    let mut b = MergeFileInput::new();
    b.content(ours);
    let mut c = MergeFileInput::new();
    c.content(theirs);
    let mut opts = MergeFileOptions::new();
    opts.style_diff3(true).marker_size(MARKER as u16);
    let merged = git2::merge_file(&a, &b, &c, Some(&mut opts)).map_err(|e| e.to_string())?;
    Ok(RawMerge {
        content: merged.content().to_vec(),
        automergeable: merged.is_automergeable(),
    })
}

fn is_marker(line: &str, ch: char, labelled: bool) -> bool {
    let bare = line.trim_end_matches(['\n', '\r']);
    let run = bare.chars().take_while(|&c| c == ch).count();
    run == MARKER && (bare.len() == MARKER || (labelled && bare.as_bytes()[MARKER] == b' '))
}

/// Splits libgit2's diff3-style merge output (with MARKER-long markers) into segments.
pub fn parse_merged(text: &str) -> Vec<MergeSegment> {
    #[derive(PartialEq)]
    enum At {
        Common,
        Ours,
        Base,
        Theirs,
    }
    let mut out = Vec::new();
    let (mut common, mut ours, mut base, mut theirs) =
        (String::new(), String::new(), String::new(), String::new());
    // The open conflict's lines as written, markers included.
    let mut raw = String::new();
    let mut at = At::Common;
    for line in text.split_inclusive('\n') {
        if at != At::Common {
            raw.push_str(line);
        }
        match at {
            At::Common if is_marker(line, '<', true) => {
                if !common.is_empty() {
                    out.push(MergeSegment::Common {
                        text: std::mem::take(&mut common),
                    });
                }
                raw = line.to_string();
                at = At::Ours;
            }
            At::Ours if is_marker(line, '|', true) => at = At::Base,
            At::Ours | At::Base if is_marker(line, '=', false) => at = At::Theirs,
            At::Theirs if is_marker(line, '>', true) => {
                out.push(MergeSegment::Conflict {
                    ours: std::mem::take(&mut ours),
                    base: std::mem::take(&mut base),
                    theirs: std::mem::take(&mut theirs),
                });
                at = At::Common;
            }
            At::Common => common.push_str(line),
            At::Ours => ours.push_str(line),
            At::Base => base.push_str(line),
            At::Theirs => theirs.push_str(line),
        }
    }
    // Unterminated: not a conflict after all (the file has a line that
    // looks like a marker). Its lines stay as they were, markers and all.
    if at != At::Common {
        if let Some(MergeSegment::Common { text }) = out.last() {
            common = text.clone();
            out.pop();
        }
        common.push_str(&raw);
    }
    if !common.is_empty() {
        out.push(MergeSegment::Common { text: common });
    }
    out
}

/// Merges three versions of a file as bytes: `Ok(Some(merged))` when both
/// sides' changes apply, `Ok(None)` when they changed the same lines,
/// `Err` when the file isn't text. Text needn't be UTF-8: other 8-bit
/// encodings merge line by line as they are, and UTF-16 with a byte-order
/// mark is decoded, merged and encoded back.
pub fn merge_bytes(base: &[u8], ours: &[u8], theirs: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let utf16 = |b: &[u8]| match b {
        [0xFF, 0xFE, ..] => Some(false),
        [0xFE, 0xFF, ..] => Some(true),
        _ => None,
    };
    let order = utf16(ours);
    if order.is_some() && utf16(theirs) == order && (base.is_empty() || utf16(base) == order) {
        let big = order == Some(true);
        let decode = |b: &[u8]| -> String {
            let units: Vec<u16> = b[b.len().min(2)..]
                .chunks_exact(2)
                .map(|c| {
                    if big {
                        u16::from_be_bytes([c[0], c[1]])
                    } else {
                        u16::from_le_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        };
        return match merge3(&decode(base), &decode(ours), &decode(theirs))? {
            Merged::Clean(text) => {
                let mut out = if big {
                    vec![0xFE, 0xFF]
                } else {
                    vec![0xFF, 0xFE]
                };
                for u in text.encode_utf16() {
                    out.extend(if big {
                        u.to_be_bytes()
                    } else {
                        u.to_le_bytes()
                    });
                }
                Ok(Some(out))
            }
            Merged::Conflict(_) => Ok(None),
        };
    }
    let has_nul = |b: &[u8]| b.iter().take(8000).any(|&c| c == 0);
    if has_nul(base) || has_nul(ours) || has_nul(theirs) {
        return Err("not a text file".into());
    }
    let raw = merge3_raw(base, ours, theirs)?;
    Ok(raw.automergeable.then_some(raw.content))
}

/// Not text: a NUL byte near the start, or not UTF-8.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0) || std::str::from_utf8(bytes).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_that_isnt_utf8_merges() {
        // Latin-1: é is one byte, not valid UTF-8.
        let base = b"caf\xe9\n2\n3\n4\n5\n";
        let ours = b"CAF\xc9\n2\n3\n4\n5\n";
        let theirs = b"caf\xe9\n2\n3\n4\nFIVE\n";
        assert_eq!(
            merge_bytes(base, ours, theirs).unwrap().unwrap(),
            b"CAF\xc9\n2\n3\n4\nFIVE\n"
        );
        // UTF-16 with a byte-order mark.
        let enc = |t: &str| {
            let mut v = vec![0xFF, 0xFE];
            for u in t.encode_utf16() {
                v.extend(u.to_le_bytes());
            }
            v
        };
        let merged = merge_bytes(
            &enc("1\n2\n3\n4\n5\n"),
            &enc("A\n2\n3\n4\n5\n"),
            &enc("1\n2\n3\n4\nE\n"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(merged, enc("A\n2\n3\n4\nE\n"));
        assert!(merge_bytes(&enc("1\n"), &enc("A\n"), &enc("B\n"))
            .unwrap()
            .is_none());
        // Real binary: no merge.
        assert!(merge_bytes(b"\0a", b"\0b", b"\0c").is_err());
    }

    #[test]
    fn a_marker_never_closed_is_kept_as_text() {
        let lt = "<".repeat(MARKER);
        let bar = "|".repeat(MARKER);
        let text = format!("a\n{lt} x\nours\n{bar} y\nbase\nb\n");
        assert_eq!(parse_merged(&text), vec![MergeSegment::Common { text }]);
    }

    const BASE: &str = "one\ntwo\nthree\nfour\nfive\n";

    #[test]
    fn changes_to_different_lines_merge_cleanly() {
        let ours = "ONE\ntwo\nthree\nfour\nfive\n";
        let theirs = "one\ntwo\nthree\nfour\nFIVE\n";
        assert_eq!(
            merge3(BASE, ours, theirs).unwrap(),
            Merged::Clean("ONE\ntwo\nthree\nfour\nFIVE\n".into())
        );
    }

    #[test]
    fn the_same_change_on_both_sides_is_clean() {
        let both = "one\nTWO\nthree\nfour\nfive\n";
        assert_eq!(
            merge3(BASE, both, both).unwrap(),
            Merged::Clean(both.into())
        );
    }

    #[test]
    fn changes_to_the_same_line_conflict() {
        let ours = "one\nours\nthree\nfour\nfive\n";
        let theirs = "one\ntheirs\nthree\nfour\nfive\n";
        let Merged::Conflict(segments) = merge3(BASE, ours, theirs).unwrap() else {
            panic!("expected a conflict")
        };
        assert_eq!(
            segments,
            vec![
                MergeSegment::Common {
                    text: "one\n".into()
                },
                MergeSegment::Conflict {
                    ours: "ours\n".into(),
                    base: "two\n".into(),
                    theirs: "theirs\n".into()
                },
                MergeSegment::Common {
                    text: "three\nfour\nfive\n".into()
                },
            ]
        );
    }

    #[test]
    fn a_side_that_emptied_the_file_against_one_that_changed_it_conflicts() {
        let theirs = "one\ntwo\nTHREE\nfour\nfive\n";
        assert!(matches!(
            merge3(BASE, "", theirs).unwrap(),
            Merged::Conflict(_)
        ));
    }

    #[test]
    fn an_unchanged_side_takes_the_other() {
        let theirs = "one\ntwo\nthree\n";
        assert_eq!(
            merge3(BASE, BASE, theirs).unwrap(),
            Merged::Clean(theirs.into())
        );
    }

    #[test]
    fn binary_detection() {
        assert!(is_binary(b"a\0b"));
        assert!(is_binary(&[0xff, 0xfe, 0x41]));
        assert!(!is_binary("plain text, ünïcode\n".as_bytes()));
    }

    #[test]
    fn short_marker_runs_in_the_files_are_text() {
        let text = "<<<<<<< not a marker\n=======\n>>>>>>> also not\n";
        assert_eq!(
            parse_merged(text),
            vec![MergeSegment::Common { text: text.into() }]
        );
    }
}
