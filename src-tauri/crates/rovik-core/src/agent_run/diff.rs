//! Line-level diff of a proposed file edit: Myers' algorithm on the lines
//! that differ (common prefix and suffix trimmed first), grouped into hunks.
//! The same algorithm as the editor's review (src/lib/lineDiff.ts), so the
//! AI reviewer and the user see the same changes.

pub use rovik_protocol::diff::Hunk;

/// Above this many differing lines the diff is one big hunk.
const MAX_EDIT_DISTANCE: usize = 2_000;

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Eq,
    Del,
    Ins,
}

fn myers(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    // At least the difference in length apart: past the limit, no search.
    if a.len().abs_diff(b.len()) > MAX_EDIT_DISTANCE {
        return None;
    }
    let n = a.len() as isize;
    let m = b.len() as isize;
    // The search stops at the limit, so the diagonals it visits, and the
    // copy of them each step keeps, are bounded by it, not by the files.
    let max = (n + m).min(MAX_EDIT_DISTANCE as isize);
    let off = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    for d in 0..=max {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x =
                if k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]) {
                    v[(off + k + 1) as usize]
                } else {
                    v[(off + k - 1) as usize] + 1
                };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(off + k) as usize] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m, off));
            }
            k += 2;
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize, off: isize) -> Vec<Op> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize])
        {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(off + prev_k) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Eq);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            ops.push(if x == prev_x { Op::Ins } else { Op::Del });
        }
        x = prev_x;
        y = prev_y;
    }
    ops.reverse();
    ops
}

/// Hunks turning `a` into `b`, in order.
pub fn diff_lines(a: &[&str], b: &[&str]) -> Vec<Hunk> {
    let mut start = 0;
    while start < a.len() && start < b.len() && a[start] == b[start] {
        start += 1;
    }
    let (mut end_a, mut end_b) = (a.len(), b.len());
    while end_a > start && end_b > start && a[end_a - 1] == b[end_b - 1] {
        end_a -= 1;
        end_b -= 1;
    }
    let mid_a = &a[start..end_a];
    let mid_b = &b[start..end_b];
    let owned = |lines: &[&str]| lines.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    if mid_a.is_empty() && mid_b.is_empty() {
        return Vec::new();
    }
    let Some(ops) = myers(mid_a, mid_b) else {
        return vec![Hunk {
            orig_start: start + 1,
            deleted: owned(mid_a),
            added: owned(mid_b),
        }];
    };

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut open = false;
    let (mut ai, mut bi) = (0, 0);
    for op in ops {
        if op == Op::Eq {
            open = false;
            ai += 1;
            bi += 1;
            continue;
        }
        if !open {
            hunks.push(Hunk {
                orig_start: start + ai + 1,
                deleted: Vec::new(),
                added: Vec::new(),
            });
            open = true;
        }
        let hunk = hunks.last_mut().unwrap();
        if op == Op::Del {
            hunk.deleted.push(mid_a[ai].to_string());
            ai += 1;
        } else {
            hunk.added.push(mid_b[bi].to_string());
            bi += 1;
        }
    }
    hunks
}

/// Lines without their \r.
pub fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// The change as a compact diff for the reviewer: changed lines only, with positions.
pub fn compact_diff(hunks: &[Hunk]) -> String {
    hunks
        .iter()
        .map(|h| {
            let mut lines = vec![format!("@@ line {} @@", h.orig_start)];
            lines.extend(h.deleted.iter().map(|l| format!("-{l}")));
            lines.extend(h.added.iter().map(|l| format!("+{l}")));
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Past the limit a diff is one hunk, found without a search the size
    /// of the files: a whole file written over, or one rewritten.
    #[test]
    fn a_rewrite_of_a_big_file_is_one_hunk() {
        let big: Vec<String> = (0..200_000).map(|n| format!("line {n}")).collect();
        let big: Vec<&str> = big.iter().map(String::as_str).collect();
        let hunks = diff_lines(&["old"], &big);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].added.len(), 200_000);
        let other: Vec<String> = (0..200_000).map(|n| format!("other {n}")).collect();
        let other: Vec<&str> = other.iter().map(String::as_str).collect();
        let hunks = diff_lines(&big, &other);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].deleted.len(), 200_000);
    }

    #[test]
    fn hunks_of_a_change() {
        let a = split_lines("a\nb\nc\nd\ne");
        let b = split_lines("a\nB\nc\nd\nx\ne");
        let hunks = diff_lines(&a, &b);
        assert_eq!(
            hunks,
            vec![
                Hunk {
                    orig_start: 2,
                    deleted: vec!["b".into()],
                    added: vec!["B".into()]
                },
                Hunk {
                    orig_start: 5,
                    deleted: vec![],
                    added: vec!["x".into()]
                },
            ]
        );
        assert_eq!(
            compact_diff(&hunks),
            "@@ line 2 @@\n-b\n+B\n@@ line 5 @@\n+x"
        );
        assert!(diff_lines(&split_lines("a\r\nb"), &split_lines("a\nb")).is_empty());
    }

    #[test]
    fn diff_edge_cases() {
        fn lines(t: &str) -> Vec<&str> {
            split_lines(t)
        }
        // Insertion at the end, deletion of everything, an empty file.
        assert_eq!(
            diff_lines(&lines("a"), &lines("a\nb")),
            vec![Hunk {
                orig_start: 2,
                deleted: vec![],
                added: vec!["b".into()]
            }]
        );
        assert_eq!(
            diff_lines(&lines("a\nb"), &lines("")),
            vec![Hunk {
                orig_start: 1,
                deleted: vec!["a".into(), "b".into()],
                added: vec!["".into()]
            }]
        );
        assert!(diff_lines(&lines(""), &lines("")).is_empty());
        // Repeated lines: the change lands on one of them only.
        let hunks = diff_lines(&lines("x\nx\nx"), &lines("x\nx"));
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].deleted, ["x"]);
        // Past the edit-distance limit: one hunk for the whole middle.
        let a: Vec<String> = (0..2_100).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..2_100).map(|i| format!("b{i}")).collect();
        let (a, b): (Vec<&str>, Vec<&str>) = (
            a.iter().map(|s| s.as_str()).collect(),
            b.iter().map(|s| s.as_str()).collect(),
        );
        let hunks = diff_lines(&a, &b);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].deleted.len(), 2_100);
    }
}
