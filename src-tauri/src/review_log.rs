// src-tauri/src/review_log.rs
//
// Terminal log of AI reviews (src/lib/aiApproval.ts): one line when a
// review starts and one when it finishes, so approvals can be followed
// from the console running the app.

/// A review started: which tool call, and which models will be asked.
#[tauri::command]
pub fn log_ai_review_started(
    review_id: String,
    tool: String,
    summary: String,
    group: String,
    models: Vec<String>,
) {
    println!(
        "[ai-review {review_id}] started: {tool} ({group}) \"{summary}\" models: {}",
        if models.is_empty() { "(none)".to_string() } else { models.join(" -> ") }
    );
}

/// A review finished: approved / rejected / unsure / failed, by which model, and why.
#[tauri::command]
pub fn log_ai_review_finished(
    review_id: String,
    outcome: String,
    model: Option<String>,
    reason: String,
    duration_ms: u64,
) {
    println!(
        "[ai-review {review_id}] finished in {duration_ms} ms: {outcome}{} - {reason}",
        model.map(|m| format!(" by {m}")).unwrap_or_default()
    );
}
