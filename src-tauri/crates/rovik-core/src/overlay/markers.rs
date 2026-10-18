//! Conflict markers the IDE puts in a chat's files for the chat to settle
//! (D14): the project's version first and the chat's last, labelled so they
//! are found again and never mistaken for markers of the chat's own. A
//! local chat gets them in its overlay (chats.rs), a remote one in its
//! worktree when a sync conflicts (6.7), both written by [`text`].

use crate::merge::{is_binary, merge3_raw_as, parse_merged, MergeSegment, Style};

/// Starts the project's version.
pub const PROJECT: &str = "<<<<<<< project (now)";
/// Starts the lines before either change.
pub const BASE: &str = "||||||| before";
/// Starts the chat's version.
pub const SPLIT: &str = "=======";
/// Ends a conflict.
pub const CHAT: &str = ">>>>>>> this chat";

/// A file's versions merged, in segments, the project's side as "ours",
/// each conflict with the lines before either change: what the markers
/// show. A missing side, or a merge libgit2 refuses, makes the whole file
/// one conflict.
pub fn segments(
    base: Option<&[u8]>,
    project: Option<&[u8]>,
    chat: Option<&[u8]>,
) -> Vec<MergeSegment> {
    segments_as(Style::WithBase, base, project, chat)
}

/// [`segments`] as the conflict resolver shows them: each conflict cut as
/// git cuts it, smaller where both sides share lines, without the lines
/// before.
pub fn view(base: Option<&[u8]>, project: Option<&[u8]>, chat: Option<&[u8]>) -> Vec<MergeSegment> {
    segments_as(Style::Git, base, project, chat)
}

fn segments_as(
    style: Style,
    base: Option<&[u8]>,
    project: Option<&[u8]>,
    chat: Option<&[u8]>,
) -> Vec<MergeSegment> {
    let text = |side: Option<&[u8]>| String::from_utf8_lossy(side.unwrap_or_default()).into_owned();
    let whole = || {
        vec![MergeSegment::Conflict {
            ours: text(project),
            base: text(base),
            theirs: text(chat),
        }]
    };
    let (Some(project), Some(chat)) = (project, chat) else {
        return whole();
    };
    match merge3_raw_as(base.unwrap_or_default(), project, chat, style) {
        Ok(merged) => parse_merged(&String::from_utf8_lossy(&merged.content)),
        Err(_) => whole(),
    }
}

/// The file's text, with markers where its sides conflict.
pub fn text(segments: &[MergeSegment]) -> String {
    let mut text = String::new();
    for segment in segments {
        match segment {
            MergeSegment::Common { text: common } => text.push_str(common),
            MergeSegment::Conflict { ours, base, theirs } => {
                for (mark, part) in [(PROJECT, ours), (BASE, base), (SPLIT, theirs)] {
                    text.push_str(mark);
                    text.push('\n');
                    text.push_str(part);
                    if !part.is_empty() && !part.ends_with('\n') {
                        text.push('\n');
                    }
                }
                text.push_str(CHAT);
                text.push('\n');
            }
        }
    }
    text
}

/// `chat` merged with `project` against `base`, with markers where they
/// conflict. None when the file can't have markers: it isn't text.
pub fn mark(base: Option<&[u8]>, project: &[u8], chat: &[u8]) -> Option<Vec<u8>> {
    let sides = [base.unwrap_or_default(), project, chat];
    if sides.iter().any(|side| is_binary(side)) {
        return None;
    }
    Some(text(&segments(base, Some(project), Some(chat))).into_bytes())
}

/// Whether a file still has the IDE's markers.
pub fn left_in(file: &[u8]) -> bool {
    let text = String::from_utf8_lossy(file);
    text.lines()
        .any(|line| line.starts_with(PROJECT) || line.starts_with(CHAT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_conflict_gets_both_sides_between_markers() {
        let marked = mark(Some(b"a\n1\n"), b"PROJECT\n1\n", b"CHAT\n1\n").unwrap();
        let expected = format!("{PROJECT}\nPROJECT\n{BASE}\na\n{SPLIT}\nCHAT\n{CHAT}\n1\n");
        assert_eq!(String::from_utf8(marked.clone()).unwrap(), expected);
        assert!(left_in(&marked));
        assert!(!left_in(b"PROJECT\nCHAT\n1\n"));
        assert_eq!(mark(None, b"one\0", b"two\0"), None, "binary");
    }
}
