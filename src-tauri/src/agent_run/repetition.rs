//! Notices a reply that got stuck repeating itself while it streams ("Sigh.
//! I'll just output it." forty times, or the same plan over and over), so
//! the request can be stopped instead of running to its output limit. Two
//! shapes are caught:
//!   - a short unit (a phrase, a line) repeated many times in a row at the end;
//!   - a long paragraph that keeps coming back (outside code blocks, where
//!     repeated lines are normal).
//!
//! Returns where the repetition starts, to cut the reply there.

use std::collections::HashMap;

/// A unit of 4 to 400 characters repeated at least this many times in a row.
const UNIT_REPEATS: usize = 6;
/// A paragraph of at least 80 characters seen at least this many times.
const PARAGRAPH_REPEATS: usize = 3;
const WINDOW: usize = 8000;

/// The byte offset in `text` where the repetition starts (the first
/// occurrence is kept), if the reply is repeating itself.
pub fn find_repetition(text: &str) -> Option<usize> {
    let all: Vec<char> = text.chars().collect();
    if all.len() < 120 {
        return None;
    }
    let offset = all.len().saturating_sub(WINDOW);
    // Code is blanked out (same length, so offsets still match): repeated
    // lines and values are normal there (`0x00, 0x00, …`).
    let tail = blank_code(&all[offset..]);
    let len = tail.len();

    // 1. The end is one unit repeated (whitespace between them included in the unit).
    let mut p = 4;
    while p <= 400 && p * UNIT_REPEATS <= len {
        let unit = &tail[len - p..];
        // Words, not punctuation: a wide table's "| --- | --- |" row isn't a loop.
        if unit.iter().filter(|c| c.is_alphanumeric()).count() >= 6 {
            let mut k = 1;
            while (k + 1) * p <= len && tail[len - (k + 1) * p..len - k * p] == *unit {
                k += 1;
            }
            if k >= UNIT_REPEATS {
                // Keep the first occurrence.
                return Some(byte_offset(text, offset + len - k * p + p));
            }
        }
        p += 1;
    }

    // 2. Long paragraphs that keep coming back, outside code.
    let mut seen: HashMap<String, Vec<usize>> = HashMap::new();
    for (start, para) in paragraphs(&tail) {
        let para: String = para.iter().collect::<String>().trim().to_string();
        if para.chars().count() < 80 {
            continue;
        }
        let at = seen.entry(para).or_default();
        at.push(start);
        if at.len() >= PARAGRAPH_REPEATS {
            return Some(byte_offset(text, offset + at[1]));
        }
    }
    None
}

/// Fenced code blocks (to the next fence, or the end) as spaces.
fn blank_code(chars: &[char]) -> Vec<char> {
    let mut out = chars.to_vec();
    let fence = |i: usize| i + 3 <= chars.len() && chars[i..i + 3] == ['`', '`', '`'];
    let mut i = 0;
    while i < chars.len() {
        if !fence(i) {
            i += 1;
            continue;
        }
        let mut j = i + 3;
        while j < chars.len() && !fence(j) {
            j += 1;
        }
        let end = if j < chars.len() { j + 3 } else { chars.len() };
        for c in &mut out[i..end] {
            *c = ' ';
        }
        i = end;
    }
    out
}

/// Runs of consecutive non-blank lines, with where each starts.
fn paragraphs(chars: &[char]) -> Vec<(usize, &[char])> {
    let mut out = Vec::new();
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' {
            lines.push((start, i));
            start = i + 1;
        }
    }
    lines.push((start, chars.len()));

    let blank = |(s, e): (usize, usize)| chars[s..e].iter().all(|c| c.is_whitespace());
    let mut i = 0;
    while i < lines.len() {
        let (s, e) = lines[i];
        if s == e {
            i += 1;
            continue;
        }
        let mut end = e;
        let mut j = i + 1;
        while j < lines.len() && lines[j].0 < lines[j].1 && !blank(lines[j]) {
            end = lines[j].1;
            j += 1;
        }
        out.push((s, &chars[s..end]));
        i = j;
    }
    out
}

fn byte_offset(text: &str, char_index: usize) -> usize {
    text.char_indices().nth(char_index).map_or(text.len(), |(b, _)| b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phrase_repeated_at_the_end() {
        let start = "Here is my analysis of the problem, which took a while to work out properly. ";
        let text = format!("{start}{}", "Sigh. I'll just output it. ".repeat(10));
        let cut = find_repetition(&text).unwrap();
        assert_eq!(&text[..cut], format!("{start}Sigh. I'll just output it. "));
    }

    #[test]
    fn a_paragraph_that_keeps_coming_back() {
        let para = "The plan is to refactor the module, move the helpers into their own file, and add tests for each.";
        let text = format!("Intro line.\n\n{para}\n\nSomething else.\n\n{para}\n\nMore.\n\n{para}\n\nEnd.");
        let cut = find_repetition(&text).unwrap();
        assert!(text[..cut].ends_with("Something else.\n\n"), "{:?}", &text[..cut]);
    }

    #[test]
    fn repeated_code_and_tables_are_fine() {
        let code = format!("Bytes:\n```\n{}\n```\nDone, that's the table you asked for, all of it.", "0x00, ".repeat(100));
        assert_eq!(find_repetition(&code), None);
        let table = format!("| a | b |\n{}", "| --- | --- |\n".repeat(20));
        assert_eq!(find_repetition(&table), None);
        assert_eq!(find_repetition("short"), None);
    }

    #[test]
    fn repetition_edge_cases() {
        // Five repeats aren't enough.
        let five = format!("{}{}", "Intro that is long enough to look at, for sure, yes. ".repeat(3), "I will now do it. ".repeat(5));
        assert_eq!(find_repetition(&five), None);
        // Non-ASCII text: the cut lands on a character boundary.
        let text = format!("Начинаем работу над задачей прямо сейчас, без промедления. {}", "Повторяю ещё раз. ".repeat(10));
        let cut = find_repetition(&text).unwrap();
        assert!(text.is_char_boundary(cut));
        assert!(text[..cut].ends_with("Повторяю ещё раз. "));
        // An unclosed code fence blanks to the end.
        let code = format!("Here is the output of the program:\n```\n{}", "same line\n".repeat(50));
        assert_eq!(find_repetition(&code), None);
        // A short line coming back with other text between is fine...
        let varied: String = (0..10).map(|i| format!("Step {i} finished, moving on to the next one.\n\nDone.\n\n")).collect();
        assert_eq!(find_repetition(&varied), None);
        // ...but the same few words dozens of times in a row are a loop.
        assert!(find_repetition(&"Done.\n\n".repeat(30)).is_some());
    }
}
