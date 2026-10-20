//! Diagnostics for tuning the agent: one JSON line per model reply, per
//! verification round, per question and per finished run, appended to
//! agent-log.jsonl in the core's log folder, the app's data folder
//! (rotated at 5 MB; see project_profile::append_agent_log). Linux:
//! ~/.local/share/rovik-ide/.
//!
//! Never fails and never blocks the agent: a failed write is dropped.

use crate::core::Core;
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

pub fn write(core: &Core, entry: Value) {
    let mut line = Map::new();
    line.insert("t".into(), Value::String(iso_now()));
    if let Value::Object(fields) = entry {
        line.extend(fields);
    }
    let text = Value::Object(line).to_string();
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    queue.lines.push_back((core.paths.log_dir.clone(), text));
    queue.written += 1;
    if !queue.writing {
        queue.writing = true;
        crate::rt::spawn_blocking(append_queued);
    }
}

/// The lines not written yet, in the order they came, and whether a writer
/// is at work on them: one at a time, so they reach the log in that order
/// whichever threads wrote them.
struct Queue {
    lines: VecDeque<(PathBuf, String)>,
    writing: bool,
    /// How many lines were written, and how many of them, the first, are
    /// in the log.
    written: u64,
    appended: u64,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    lines: VecDeque::new(),
    writing: false,
    written: 0,
    appended: 0,
});

/// Appends the queued lines, oldest first, until none is left.
fn append_queued() {
    loop {
        let (dir, text) = {
            let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            match queue.lines.pop_front() {
                Some(line) => line,
                None => {
                    queue.writing = false;
                    return;
                }
            }
        };
        // A panic in one append (a path that can't be made) is that line's:
        // the writer goes on, or `writing` would stay set and every later
        // line stay queued.
        let appended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = crate::project_profile::append_agent_log(&dir, &text);
        }));
        if appended.is_err() {
            eprintln!("agent log: a line couldn't be written to {}", dir.display());
        }
        QUEUE.lock().unwrap_or_else(|e| e.into_inner()).appended += 1;
    }
}

/// Waits until the lines written so far are in the log (5 s at most):
/// tests read it right after what they wait for wrote its lines.
#[cfg(test)]
pub(crate) async fn flushed() {
    let written = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).written;
    for _ in 0..1000 {
        if QUEUE.lock().unwrap_or_else(|e| e.into_inner()).appended >= written {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// A token count in a reply's usage: a whole number, as some proxies send
/// them as floats (`10.0`); not a fraction or a negative (round 2, P6.2).
pub fn token_count(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0)
            .map(|f| f as u64)
    })
}

/// A reply's usage: tokens in and out (OpenAI's `prompt_tokens` and
/// `completion_tokens`, or the Responses API's `input_tokens` and
/// `output_tokens`), each when reported, and the cost when the provider
/// says (OpenRouter and some others do).
pub fn usage_counts(u: &Value) -> (Option<u64>, Option<u64>, Option<f64>) {
    let n = |keys: &[&str]| keys.iter().find_map(|k| token_count(u.get(*k)?));
    (
        n(&["prompt_tokens", "input_tokens"]),
        n(&["completion_tokens", "output_tokens"]),
        u.get("cost").and_then(|c| c.as_f64()),
    )
}

/// "2026-10-06T09:41:07.123Z"
pub fn iso_now() -> String {
    iso(crate::chat_db::store::now_ms())
}

fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Today in UTC, "2026-10-06" (when the user's local date isn't known).
pub fn utc_date() -> String {
    let (y, m, d) = civil_from_days(crate::chat_db::store::now_ms().div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines reach the log in the order they're written, whichever of the
    /// runtime's threads appends them.
    #[test]
    fn lines_reach_the_log_in_their_order() {
        let test = crate::testing::TestCore::new();
        let log = test.core.paths.log_dir.join("agent-log.jsonl");
        for n in 0..300 {
            write(&test.core, serde_json::json!({ "event": "order", "n": n }));
        }
        let mut seen = Vec::new();
        for _ in 0..500 {
            seen = std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter_map(|l| l["n"].as_u64())
                .collect();
            if seen.len() == 300 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(seen, (0..300).collect::<Vec<u64>>());
    }

    #[test]
    fn formats_utc_times() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_791_280_867_123), "2026-10-06T10:01:07.123Z");
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
