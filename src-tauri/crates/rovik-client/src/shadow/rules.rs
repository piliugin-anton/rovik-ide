//! Ignore rules rebased under a folder's name (workspaces-plan.md, 4.11).
//! A workspace's composite puts each folder at the top of one tree, under
//! its name: a folder inside a repository loses there the rules git takes
//! from outside it, and its by-products (`node_modules`, which its
//! repository's root `.gitignore` names) would come back in a chat's
//! results. So the composite's worktrees on a daemon get them all, each
//! rewritten to match under the folder's name what it matches in the
//! folder where the folder is: the user's global excludes, the repository's
//! `info/exclude`, and the `.gitignore` of each folder from the
//! repository's root down to the folder's parent, in that order, as later
//! rules win and git gives each of these files more weight than the one
//! before. The folder's own `.gitignore`s are in its tree, and apply there
//! as they are.
//!
//! A rule whose parts name the folder's own path, or a folder above it, is
//! matched part by part against that path (`*`, `?`, `[…]`, `**`), as
//! libgit2 matches it: what's left of it applies inside the folder, and a
//! rule that ends at the folder or above it takes in the whole folder.

use super::{git, xdg_excludes, Result};
use git2::Repository;
use std::fs;
use std::path::{Path, PathBuf};

/// The rules a workspace's composite sends with every sync, the same for
/// all its chats (each daemon project keeps one set, rewritten by each
/// sync): those of each of `folders` (its name and its path), rebased under
/// its name, in the workspace's order.
pub fn workspace_excludes(folders: &[(String, PathBuf)]) -> Result<Vec<String>> {
    let mut rules = Vec::new();
    for (name, path) in folders {
        rules.extend(folder_excludes(name, path)?);
    }
    Ok(rules)
}

/// What git ignores in the folder at `path` from outside it, rebased
/// under `name`.
fn folder_excludes(name: &str, path: &Path) -> Result<Vec<String>> {
    let repo = Repository::discover(path)
        .ok()
        .filter(|repo| repo.workdir().is_some());
    let Some(repo) = repo else {
        // No repository: only the user's global excludes, from the folder.
        let config = git2::Config::open_default().map_err(git)?;
        let global = config
            .get_path("core.excludesfile")
            .ok()
            .or_else(xdg_excludes);
        let mut rules = Vec::new();
        if let Some(file) = global {
            rules.extend(rebase_file(&file, ("", &[]), name, false)?);
        }
        return Ok(rules);
    };
    // Both as the file system spells them, as `Shadow::open` compares them.
    let real = |path: &Path| {
        fs::canonicalize(path).map_err(|e| format!("Can't read {}: {e}", path.display()))
    };
    let folder = real(path)?;
    let top = real(repo.workdir().unwrap_or(path))?;
    let parts: Vec<String> = folder
        .strip_prefix(&top)
        .map_err(|_| format!("{} isn't in its repository", folder.display()))?
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let config = repo.config().map_err(git)?;
    let fold = config.get_bool("core.ignorecase").unwrap_or(false);
    let mut rules = Vec::new();
    let global = config
        .get_path("core.excludesfile")
        .ok()
        .or_else(xdg_excludes);
    if let Some(file) = global {
        rules.extend(rebase_file(&file, ("", &parts), name, fold)?);
    }
    let exclude = repo.commondir().join("info/exclude");
    rules.extend(rebase_file(&exclude, ("", &parts), name, fold)?);
    for depth in 0..parts.len() {
        let above = parts[..depth]
            .iter()
            .fold(top.clone(), |dir, part| dir.join(part));
        let dir: String = parts[..depth]
            .iter()
            .map(|part| format!("{part}/"))
            .collect();
        let file = above.join(".gitignore");
        rules.extend(rebase_file(&file, (&dir, &parts[depth..]), name, fold)?);
    }
    Ok(rules)
}

/// The rules of `file` (none when it isn't there), each rebased: `parts`
/// is the folder's path from `dir`, the folder the rules are relative to
/// (`apps/`; "" at the repository's root). A negated rule without
/// wildcards that takes back no earlier rule of its file is left out, as
/// libgit2 leaves it out when it reads the file.
fn rebase_file(
    file: &Path,
    (dir, parts): (&str, &[String]),
    name: &str,
    fold: bool,
) -> Result<Vec<String>> {
    let text = match fs::read(file) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Can't read {}: {e}", file.display())),
    };
    // In order, repeats too: of two rules that match, the later one wins.
    Ok(kept_rules(&text, dir, fold)
        .iter()
        .flat_map(|rule| rebase(rule, parts, name, fold))
        .collect())
}

/// The rules of a file at `dir` that libgit2 keeps when it reads `text`:
/// all, but a negated one without wildcards that takes back no rule before
/// it (`takes_back`).
fn kept_rules(text: &str, dir: &str, fold: bool) -> Vec<Rule> {
    let mut kept: Vec<Rule> = Vec::new();
    for rule in text.split('\n').filter_map(Rule::parse) {
        if rule.negative && !rule.wild && !takes_back(&kept, &rule, dir, fold) {
            continue;
        }
        kept.push(rule);
    }
    kept
}

/// A gitignore line as libgit2 reads it (`git_attr_fnmatch__parse`).
#[derive(Debug, Clone, PartialEq)]
struct Rule {
    negative: bool,
    /// It has a slash before its end: it's matched from its file's folder.
    anchored: bool,
    /// It ends with a slash: it matches folders only.
    folder: bool,
    /// It has an unescaped `*`, `?` or `[`.
    wild: bool,
    /// Its pattern, without its `!`, its leading and trailing slash and its
    /// trailing spaces, escapes kept.
    pattern: String,
}

impl Rule {
    /// None for a blank line, a comment or an empty rule.
    fn parse(line: &str) -> Option<Rule> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with('#') {
            return None;
        }
        let (negative, body) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let body = trim_end(body);
        let (mut slashes, mut wild, mut escaped) = (0, false, false);
        for c in body.chars() {
            if c == '\\' && !escaped {
                escaped = true;
                continue;
            }
            if c == '/' {
                slashes += 1;
            } else if matches!(c, '*' | '?' | '[') && !escaped {
                wild = true;
            }
            escaped = false;
        }
        let body = body.strip_prefix('/').unwrap_or(body);
        let (body, folder) = match body.strip_suffix('/') {
            Some(rest) => (rest, true),
            None => (body, false),
        };
        if folder {
            slashes -= 1;
        }
        (!body.is_empty()).then(|| Rule {
            negative,
            anchored: slashes > 0,
            folder,
            wild,
            pattern: body.to_string(),
        })
    }
}

/// Whether the negated rule `negation` takes back one of `kept`, the rules
/// before it in its file at `dir`, as libgit2 asks (`does_negate_rule`):
/// one without wildcards it names, or its basename at the end of the
/// other; or one with wildcards that matches its path.
fn takes_back(kept: &[Rule], negation: &Rule, dir: &str, fold: bool) -> bool {
    let path = format!("{dir}{}", unescape_spaces(&negation.pattern));
    let same = |a: &[u8], b: &[u8]| match fold {
        true => a.eq_ignore_ascii_case(b),
        false => a == b,
    };
    kept.iter().any(|rule| {
        let pattern = unescape_spaces(&rule.pattern);
        if rule.wild {
            let pattern = format!("{dir}{pattern}");
            return wildmatch(&pattern, &path, rule.anchored, fold);
        }
        if rule.negative {
            return false;
        }
        let negated = unescape_spaces(&negation.pattern);
        let (a, b) = (pattern.as_bytes(), negated.as_bytes());
        if a.len() == b.len() {
            return same(a, b);
        }
        let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
        let at = long.len() - short.len();
        long[at - 1] == b'/' && !short.contains(&b'/') && same(&long[at..], short)
    })
}

/// One kept rule, relative to a folder from which the chat's folder is at
/// `parts`, as rules for the composite, where the folder is `name`: none
/// for a rule that matches nothing in the folder. A negation without
/// wildcards comes after its own rule, which libgit2 needs to keep it
/// (`takes_back`); together they take back just what it does. `fold`:
/// names match case aside (`core.ignorecase`).
fn rebase(rule: &Rule, parts: &[String], name: &str, fold: bool) -> Vec<String> {
    let mut glob: Vec<String> = rule.pattern.split('/').map(String::from).collect();
    if !rule.anchored {
        // At any depth, as `**/` is.
        glob.insert(0, "**".into());
    }
    let mut inside = Vec::new();
    within(&glob, parts, fold, &mut inside);
    inside.dedup();
    let at = literal(name);
    let mut rules = Vec::new();
    for found in inside {
        let text = match found {
            // The folder itself, as a folder above it is in the
            // repository: walks of the worktree don't go into it.
            Inside::Whole => format!("/{at}/"),
            Inside::Rest(rest) => {
                let slash = if rule.folder { "/" } else { "" };
                format!("/{at}/{}{slash}", rest.join("/"))
            }
        };
        if !rule.negative {
            rules.push(text);
            continue;
        }
        if Rule::parse(&text).is_some_and(|own| !own.wild) {
            rules.push(text.clone());
        }
        rules.push(format!("!{text}"));
    }
    rules
}

/// How a rule applies in the chat's folder.
#[derive(Debug, PartialEq)]
enum Inside {
    /// It names the folder, or a folder above it: everything in it.
    Whole,
    /// Its parts left, matched from the folder.
    Rest(Vec<String>),
}

/// Each way the rule's parts `glob` reach into the folder at `parts`.
fn within(glob: &[String], parts: &[String], fold: bool, found: &mut Vec<Inside>) {
    let (Some((first, rest)), Some((part, deeper))) = (glob.split_first(), parts.split_first())
    else {
        // The rule ended at the folder or above it, or reaches into it.
        found.push(match glob.is_empty() {
            true => Inside::Whole,
            false => Inside::Rest(glob.to_vec()),
        });
        return;
    };
    if first == "**" {
        // No folder at all, or this one and maybe more.
        within(rest, parts, fold, found);
        within(glob, deeper, fold, found);
    } else if wildmatch(first, part, true, fold) {
        within(rest, deeper, fold, found);
    }
}

/// How a pattern went (libgit2's `dowild`).
#[derive(PartialEq)]
enum Wild {
    Match,
    NoMatch,
    AbortAll,
    AbortToStarStar,
}

/// Whether `pattern` matches `text`, as git's and libgit2's wildmatch
/// decide: `slashes` (`WM_PATHNAME`) keeps `*`, `?` and `[…]` from matching
/// a `/`, and `**` between slashes matches any number of folders; `fold`
/// (`WM_CASEFOLD`) leaves ASCII case aside.
fn wildmatch(pattern: &str, text: &str, slashes: bool, fold: bool) -> bool {
    dowild(pattern.as_bytes(), text.as_bytes(), slashes, fold) == Wild::Match
}

/// libgit2's `dowild`, byte for byte: what's past either end reads as NUL.
fn dowild(pattern: &[u8], text: &[u8], slashes: bool, fold: bool) -> Wild {
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    let low = |c: u8| match fold {
        true => c.to_ascii_lowercase(),
        false => c,
    };
    let (mut p, mut t) = (0, 0);
    while at(pattern, p) != 0 {
        let mut p_ch = low(at(pattern, p));
        let mut t_ch = at(text, t);
        if t_ch == 0 && p_ch != b'*' {
            return Wild::AbortAll;
        }
        t_ch = low(t_ch);
        match p_ch {
            b'?' => {
                if slashes && t_ch == b'/' {
                    return Wild::NoMatch;
                }
            }
            b'*' => {
                p += 1;
                let match_slash;
                if at(pattern, p) == b'*' {
                    let before = p.checked_sub(2).map(|i| pattern[i]);
                    while at(pattern, p) == b'*' {
                        p += 1;
                    }
                    let next = at(pattern, p);
                    if !slashes {
                        match_slash = true;
                    } else if before.is_none_or(|c| c == b'/')
                        && (next == 0
                            || next == b'/'
                            || (next == b'\\' && at(pattern, p + 1) == b'/'))
                    {
                        if next == b'/'
                            && dowild(&pattern[p + 1..], &text[t..], slashes, fold) == Wild::Match
                        {
                            return Wild::Match;
                        }
                        match_slash = true;
                    } else {
                        match_slash = false;
                    }
                } else {
                    match_slash = !slashes;
                }
                if at(pattern, p) == 0 {
                    // A trailing `**` matches everything, a trailing `*`
                    // only what has no more slashes.
                    if !match_slash && text[t..].contains(&b'/') {
                        return Wild::NoMatch;
                    }
                    return Wild::Match;
                } else if !match_slash && at(pattern, p) == b'/' {
                    // One `*` and a slash match the next folder.
                    match text[t..].iter().position(|&c| c == b'/') {
                        Some(slash) => t += slash,
                        None => return Wild::NoMatch,
                    }
                    t += 1;
                    p += 1;
                    continue;
                }
                loop {
                    if t_ch == 0 {
                        break;
                    }
                    let special = matches!(at(pattern, p), b'*' | b'?' | b'[' | b'\\');
                    if !special {
                        let want = low(at(pattern, p));
                        loop {
                            t_ch = at(text, t);
                            if t_ch == 0 || (!match_slash && t_ch == b'/') {
                                break;
                            }
                            t_ch = low(t_ch);
                            if t_ch == want {
                                break;
                            }
                            t += 1;
                        }
                        if t_ch != want {
                            return Wild::NoMatch;
                        }
                    }
                    let matched = dowild(&pattern[p..], &text[t..], slashes, fold);
                    if matched != Wild::NoMatch {
                        if !match_slash || matched != Wild::AbortToStarStar {
                            return matched;
                        }
                    } else if !match_slash && t_ch == b'/' {
                        return Wild::AbortToStarStar;
                    }
                    t += 1;
                    t_ch = at(text, t);
                }
                return Wild::AbortAll;
            }
            b'[' => {
                p += 1;
                p_ch = at(pattern, p);
                if p_ch == b'^' {
                    p_ch = b'!';
                }
                let negated = p_ch == b'!';
                if negated {
                    p += 1;
                    p_ch = at(pattern, p);
                }
                let (mut prev_ch, mut matched) = (0u8, false);
                loop {
                    if p_ch == 0 {
                        return Wild::AbortAll;
                    }
                    if p_ch == b'\\' {
                        p += 1;
                        p_ch = at(pattern, p);
                        if p_ch == 0 {
                            return Wild::AbortAll;
                        }
                        matched |= t_ch == p_ch;
                    } else if p_ch == b'-'
                        && prev_ch != 0
                        && at(pattern, p + 1) != 0
                        && at(pattern, p + 1) != b']'
                    {
                        p += 1;
                        p_ch = at(pattern, p);
                        if p_ch == b'\\' {
                            p += 1;
                            p_ch = at(pattern, p);
                            if p_ch == 0 {
                                return Wild::AbortAll;
                            }
                        }
                        let upper = t_ch.to_ascii_uppercase();
                        matched |= (prev_ch..=p_ch).contains(&t_ch)
                            || (fold
                                && t_ch.is_ascii_lowercase()
                                && (prev_ch..=p_ch).contains(&upper));
                        p_ch = 0;
                    } else if p_ch == b'[' && at(pattern, p + 1) == b':' {
                        let start = p + 2;
                        let mut end = start;
                        while at(pattern, end) != 0 && at(pattern, end) != b']' {
                            end += 1;
                        }
                        if at(pattern, end) == 0 {
                            return Wild::AbortAll;
                        }
                        if end == start || pattern[end - 1] != b':' {
                            // No `:]`: a `[` of the set, as it is.
                            matched |= t_ch == b'[';
                            prev_ch = b'[';
                            p += 1;
                            p_ch = at(pattern, p);
                            if p_ch == b']' {
                                break;
                            }
                            continue;
                        }
                        match in_class(&pattern[start..end - 1], t_ch, fold) {
                            Some(found) => matched |= found,
                            None => return Wild::AbortAll,
                        }
                        p = end;
                        p_ch = 0;
                    } else {
                        matched |= t_ch == p_ch;
                    }
                    prev_ch = p_ch;
                    p += 1;
                    p_ch = at(pattern, p);
                    if p_ch == b']' {
                        break;
                    }
                }
                if matched == negated || (slashes && t_ch == b'/') {
                    return Wild::NoMatch;
                }
            }
            _ => {
                if p_ch == b'\\' {
                    // The next as it is, its case too.
                    p += 1;
                    p_ch = at(pattern, p);
                }
                if t_ch != p_ch {
                    return Wild::NoMatch;
                }
            }
        }
        t += 1;
        p += 1;
    }
    match at(text, t) {
        0 => Wild::Match,
        _ => Wild::NoMatch,
    }
}

/// Whether `t` is in the POSIX class named `class` (`alpha`, `digit`, …);
/// None for a name that's none.
fn in_class(class: &[u8], t: u8, fold: bool) -> Option<bool> {
    Some(match class {
        b"alnum" => t.is_ascii_alphanumeric(),
        b"alpha" => t.is_ascii_alphabetic(),
        b"blank" => t == b' ' || t == b'\t',
        b"cntrl" => t.is_ascii_control(),
        b"digit" => t.is_ascii_digit(),
        b"graph" => t.is_ascii_graphic(),
        b"lower" => t.is_ascii_lowercase(),
        b"print" => t.is_ascii_graphic() || t == b' ',
        b"punct" => t.is_ascii_punctuation(),
        b"space" => t.is_ascii_whitespace() || t == 0x0b,
        b"upper" => t.is_ascii_uppercase() || (fold && t.is_ascii_lowercase()),
        b"xdigit" => t.is_ascii_hexdigit(),
        _ => return None,
    })
}

/// A gitignore line without its trailing spaces and tabs, but those after
/// an odd number of `\`s (libgit2's `trailing_space_length`).
fn trim_end(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], b' ' | b'\t') {
        let mut escapes = end;
        while escapes > 1 && bytes[escapes - 2] == b'\\' {
            escapes -= 1;
        }
        if (end - escapes) % 2 == 1 {
            break;
        }
        end -= 1;
    }
    &line[..end]
}

/// A pattern as libgit2 keeps it (`unescape_spaces`): without the `\` of
/// an escaped space, with the one escaping anything else.
fn unescape_spaces(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut escaped = false;
    for c in pattern.chars() {
        if !escaped && c == '\\' {
            escaped = true;
            continue;
        }
        // git__isspace: a space, a tab, a line or page break.
        if escaped && !matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r') {
            out.push('\\');
        }
        out.push(c);
        escaped = false;
    }
    out
}

/// A folder's name as a rule matches it, as it is.
fn literal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '*' | '?' | '[' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(path: &str) -> Vec<String> {
        path.split('/')
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect()
    }

    /// `line`, relative to a folder from which the chat's folder is at
    /// `at`, rebased under `name`.
    fn rebased(line: &str, at: &str, name: &str) -> Vec<String> {
        let rule = Rule::parse(line);
        rule.map_or_else(Vec::new, |rule| rebase(&rule, &parts(at), name, false))
    }

    #[test]
    fn a_rule_is_rebased_under_the_folders_name() {
        let rebase = |line: &str| rebased(line, "apps/web", "front");
        // At any depth: under the folder at any depth, and the whole folder
        // when it names a folder on its way.
        assert_eq!(rebase("*.log"), ["/front/**/*.log"]);
        assert_eq!(rebase("apps/"), ["/front/", "/front/**/apps/"]);
        // Into the folder: what's left of it, from the folder.
        assert_eq!(rebase("/apps/web/dist"), ["/front/dist"]);
        assert_eq!(rebase("*/web/cache"), ["/front/cache"]);
        assert_eq!(rebase("apps/**/gen/"), ["/front/**/gen/"]);
        assert_eq!(rebase("/apps/*"), ["/front/"]);
        // Elsewhere: nothing.
        assert!(rebase("/docs/*.md").is_empty());
        assert!(rebase("apps/other/x").is_empty());
        // Negated: one without wildcards after its own rule, which libgit2
        // needs to keep it.
        assert_eq!(rebase("!keep.log"), ["!/front/**/keep.log"]);
        assert_eq!(
            rebase("!/apps/web/keep.txt"),
            ["/front/keep.txt", "!/front/keep.txt"]
        );
        assert_eq!(rebase("!/apps/web"), ["/front/", "!/front/"]);
        // Comments, blank lines and empty rules.
        for nothing in ["# a comment", "   ", "", "!", "/"] {
            assert!(rebase(nothing).is_empty(), "{nothing:?}");
        }
        // A folder at the rules' own folder: each rule, from it.
        assert_eq!(rebased("/build", "", "api"), ["/api/build"]);
        assert_eq!(rebased("tmp/", "", "api"), ["/api/**/tmp/"]);
    }

    #[test]
    fn a_rule_is_read_as_libgit2_reads_it() {
        let rule = |line: &str| Rule::parse(line).unwrap();
        let read = rule("!/a/b/  ");
        assert!(read.negative && read.anchored && read.folder && !read.wild);
        assert_eq!(read.pattern, "a/b");
        assert!(!rule("build/").anchored, "a trailing slash alone");
        assert!(rule("a/b").anchored && rule("/a").anchored);
        assert!(rule("*.o").wild && !rule("\\*.o").wild);
        assert_eq!(rule("x\\ ").pattern, "x\\ ", "an escaped space stays");
        assert_eq!(rule("x\t\r").pattern, "x");
        assert!(Rule::parse("#x").is_none() && rule("\\#x").pattern == "\\#x");
    }

    #[test]
    fn a_negation_without_wildcards_is_kept_only_after_a_rule_it_takes_back() {
        let kept = |text: &str| -> Vec<String> {
            let rules = kept_rules(text, "", false);
            rules.into_iter().map(|r| r.pattern).collect()
        };
        // A wild rule that matches its path: without a slash, `*` matches
        // slashes too.
        assert_eq!(kept("ap*/\n!apps/web/keep.txt\n").len(), 2);
        assert_eq!(kept("*.log\n!keep.log\n").len(), 2);
        assert_eq!(
            kept("apps/*\n!apps/web/x\n").len(),
            1,
            "`*` stops at a slash"
        );
        // One without wildcards: itself, or its basename at the end.
        assert_eq!(kept("/x/y\n!/x/y\n").len(), 2);
        assert_eq!(kept("build\n!sub/build\n").len(), 2);
        assert_eq!(kept("apps/\n!apps/web/keep.txt\n").len(), 1);
        assert_eq!(kept("!keep.txt\n").len(), 0, "nothing before it");
        // A negation with wildcards always stays.
        assert_eq!(kept("!*.txt\n").len(), 1);
        // An escaped space is a space there.
        assert_eq!(kept("my?file\n!my\\ file\n").len(), 2);
        assert_eq!(kept("my file\n!my\\ file\n").len(), 2);
        assert_eq!(unescape_spaces("a\\ b\\*\\"), "a b\\*");
    }

    #[test]
    fn a_pattern_matches_as_libgit2_wildmatch_does() {
        for (pattern, text, slashes, matched) in [
            ("web", "web", true, true),
            ("we", "web", true, false),
            ("w*", "web", true, true),
            ("*b", "web", true, true),
            ("w?b", "web", true, true),
            ("w[a-f]b", "web", true, true),
            ("w[!e]b", "web", true, false),
            ("w[^x]b", "web", true, true),
            ("w[[:alpha:]]b", "web", true, true),
            ("w[[:digit:]]b", "web", true, false),
            ("w\\*b", "w*b", true, true),
            ("w\\*b", "web", true, false),
            ("[]x]", "]", true, true),
            ("w[e", "web", true, false),
            ("a/*", "a/b", true, true),
            ("a/*", "a/b/c", true, false),
            ("a*", "a/b/c", false, true),
            ("a/**", "a/b/c", true, true),
            ("a/**/c", "a/c", true, true),
            ("a/**/c", "a/b/x/c", true, true),
            ("**/c", "a/b/c", true, true),
            ("a/*/c", "a/b/c", true, true),
            ("a/*/c", "a/b/x/c", true, false),
            ("a?b", "a/b", true, false),
            ("a?b", "a/b", false, true),
        ] {
            let found = wildmatch(pattern, text, slashes, false);
            assert_eq!(found, matched, "{pattern} {text} {slashes}");
        }
        assert!(wildmatch("WEB", "web", true, true) && !wildmatch("WEB", "web", true, false));
        assert!(wildmatch("[A-Z]eb", "web", true, true));
    }

    #[test]
    fn a_folders_name_is_matched_as_it_is() {
        assert_eq!(literal("a*b?[c]\\d"), "a\\*b\\?\\[c]\\\\d");
        assert_eq!(trim_end("x  "), "x");
        assert_eq!(trim_end("x\\ "), "x\\ ");
        assert_eq!(trim_end("x\\\\ "), "x\\\\", "an escaped backslash");
    }
}
