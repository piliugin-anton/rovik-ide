// src-tauri/src/system_one.rs
//
// One System One request to the reviewer model's provider: `POST
// {base_url}/systemone`, the same way chat requests go to
// `{base_url}/chat/completions`. The body is a `state` plus named typed
// `questions`, answered with probabilities.
// Used by the AI reviewer when "Use typed input" is on (src/lib/aiApproval.ts).
// The webview can't call these hosts itself (CORS), so the request goes
// through here, like chat requests do.

use reqwest::Client;
use std::time::Duration;

const DEFAULT_TIMEOUT_MS: u64 = 15_000;

#[tauri::command]
pub async fn system_one_request(
    base_url: String,
    api_key: String,
    model: String,
    body: serde_json::Value,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value, String> {
    let url = format!("{}/{}", base_url.trim().trim_end_matches('/'), model);
    println!("{url}");
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("Invalid URL {url}: {e}"))?;
    let local = matches!(parsed.host_str(), Some("localhost") | Some("127.0.0.1") | Some("::1"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
        return Err(format!("Refusing to send the review over {}: use https", parsed.scheme()));
    }

    let client = Client::builder()
        .timeout(Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1_000, 60_000)))
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client.post(parsed.clone()).json(&body);
    let key = api_key.trim();
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }

    let res = req.send().await.map_err(|e| format!("Could not reach {parsed}: {e}"))?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let snippet: String = text.chars().take(300).collect();
        return Err(match status.as_u16() {
            401 | 403 => format!("{status}: API key rejected. {snippet}"),
            404 => format!("{status}: this provider has no /systemone endpoint at {parsed}. {snippet}"),
            _ => format!("{status}: {snippet}"),
        });
    }
    serde_json::from_str(&text).map_err(|e| format!("Response is not JSON: {e}"))
}
