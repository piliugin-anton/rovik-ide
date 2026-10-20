//! Text cut to a number of characters (not bytes: a cut never splits one).

/// The first `max` characters of `text`, and whether that left any out.
pub fn first_chars(text: &str, max: usize) -> (&str, bool) {
    match text.char_indices().nth(max) {
        Some((cut, _)) => (&text[..cut], true),
        None => (text, false),
    }
}

/// `text` cut to `max` characters, with "…" after a cut.
pub fn shortened(text: &str, max: usize) -> String {
    match first_chars(text, max) {
        (kept, true) => format!("{}…", kept.trim_end()),
        (kept, false) => kept.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_are_by_character() {
        assert_eq!(first_chars("añb", 2), ("añ", true));
        assert_eq!(first_chars("añb", 3), ("añb", false));
        assert_eq!(shortened("one two", 4), "one…");
        assert_eq!(shortened("one two", 7), "one two");
        assert_eq!(shortened("", 0), "");
    }
}
