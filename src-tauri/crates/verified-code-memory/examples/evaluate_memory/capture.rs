//! Evaluation-only episode projections from frozen run inputs, never query text.
use serde_json::Value;

type Error = Box<dyn std::error::Error>;

/// Reproduces episode observations, optionally quoting the bounded source task.
pub(super) fn compose(input: &Value, heading: Option<&str>) -> Result<(String, String), Error> {
    let task = input["task"].as_str().ok_or("capture task must be text")?;
    if task.chars().count() > 2000 {
        return Err(
            "capture projection requires a bounded source task, at most 2000 characters".into(),
        );
    }
    let first = task
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("A run of this project");
    let line = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title: String = line.chars().take(80).collect();
    if title.chars().count() < line.chars().count() {
        title.push('…');
    }
    let outcome = input["outcome"].as_str().ok_or("missing capture outcome")?;
    let mut content = format!("Outcome: {outcome}");
    let files = super::fixture_strings(&input["files"])?;
    if !files.is_empty() {
        content.push_str(&format!("\nChanged: {}", files.join(", ")));
    }
    let checks = input["checks"]
        .as_array()
        .ok_or("capture checks must be an array")?
        .iter()
        .map(|check| {
            Ok(format!(
                "{} {}",
                check["command"].as_str().ok_or("missing capture command")?,
                check["status"].as_str().ok_or("missing capture status")?
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    if !checks.is_empty() {
        content.push_str(&format!("\nChecks: {}", checks.join("; ")));
    }
    // A request describes intended work, not a verified corrective action.
    if let Some(heading) = heading.filter(|_| !task.trim().is_empty()) {
        content.push_str("\n\n");
        content.push_str(heading);
        content.push('\n');
        content.push_str(task);
    }
    Ok((title, content))
}

/// Validates source-backed projections without reading the retrieval query.
pub(super) fn project(dataset: &mut Value, mode: &Value) -> Result<(), Error> {
    let heading = match mode {
        Value::Null => return Ok(()),
        Value::String(mode) if mode == "legacy_episode" => None,
        Value::String(mode) if mode == "requested_episode" => Some("Requested task (not proof of implementation):"),
        Value::String(mode) if mode == "intent_episode" => Some("Intent only (not an implementation claim):"),
        _ => {
            return Err(
                "episode_capture_projection must be legacy_episode, requested_episode or intent_episode".into(),
            )
        }
    };
    for group in ["corpus", "cases"] {
        for entry in dataset[group]
            .as_array_mut()
            .ok_or("missing capture fixture entries")?
        {
            if entry.get("capture").is_none() {
                continue;
            }
            if entry["kind"] != "episode" {
                return Err("capture input must describe an episode".into());
            }
            let (title, legacy) = compose(&entry["capture"], None)?;
            if entry["title"].as_str() != Some(title.as_str())
                || entry["content"].as_str() != Some(legacy.as_str())
            {
                return Err("capture metadata does not reproduce the frozen legacy episode".into());
            }
            let (_, content) = compose(&entry["capture"], heading)?;
            entry["content"] = content.into();
            entry["record_files"] = entry["capture"]["files"].clone();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requested_work_preserves_legacy_observations_and_quotes_only_the_source() {
        let input = json!({"task":"\n  A title  \nA requested change, not an observed repair.","outcome":"unverified","files":["src/a.rs"],"checks":[{"command":"cargo test","status":"failed"}]});
        let (title, legacy) = compose(&input, None).unwrap();
        assert_eq!(title, "A title");
        assert_eq!(
            legacy,
            "Outcome: unverified\nChanged: src/a.rs\nChecks: cargo test failed"
        );
        let (next_title, next) = compose(
            &input,
            Some("Requested task (not proof of implementation):"),
        )
        .unwrap();
        assert_eq!(title, next_title);
        assert_eq!(
            next,
            format!(
                "{legacy}\n\nRequested task (not proof of implementation):\n{}",
                input["task"].as_str().unwrap()
            )
        );
        let mut dataset = json!({"corpus":[{"kind":"episode","title":title,"content":legacy,"capture":input}],"cases":[{"task":"Unrelated future query","title":"No match","content":"Not source context"}]});
        let query = dataset["cases"][0].clone();
        project(&mut dataset, &json!("requested_episode")).unwrap();
        assert_eq!(dataset["corpus"][0]["content"], next);
        assert_eq!(dataset["cases"][0], query);
    }

    #[test]
    fn capture_projection_rejects_drift_and_overlong_sources_instead_of_guessing() {
        let input = json!({"task":"題".repeat(2001),"outcome":"done","files":[],"checks":[]});
        assert!(compose(&input, Some("Intent only:")).is_err());
        let input = json!({"task":"task","outcome":"done","files":[],"checks":[]});
        let mut dataset = json!({"corpus":[{"kind":"episode","title":"task","content":"Invented repair","capture":input}],"cases":[]});
        assert!(project(&mut dataset, &json!("requested_episode")).is_err());
        assert!(project(&mut dataset, &json!("unknown")).is_err());
    }
}
