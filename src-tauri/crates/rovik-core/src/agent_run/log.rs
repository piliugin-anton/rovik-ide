//! Diagnostics for tuning the agent: one JSON line per model reply, per
//! verification round, per question and per finished run, appended to
//! agent-log.jsonl in the core's log folder, the app's data folder
//! (rotated at 5 MB; see project_profile::append_agent_log). Linux:
//! ~/.local/share/rovik-ide/.
//!
//! Never fails and never blocks the agent: a failed write is dropped.

use crate::core::Core;
use serde_json::{Map, Value};

pub fn write(core: &Core, entry: Value) {
    let mut line = Map::new();
    line.insert("t".into(), Value::String(iso_now()));
    if let Value::Object(fields) = entry {
        line.extend(fields);
    }
    let text = Value::Object(line).to_string();
    let dir = core.paths.log_dir.clone();
    crate::rt::spawn_blocking(move || {
        let _ = crate::project_profile::append_agent_log(&dir, &text);
    });
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

    #[test]
    fn formats_utc_times() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_791_280_867_123), "2026-10-06T10:01:07.123Z");
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
