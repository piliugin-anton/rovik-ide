// src-tauri/src/system_one.rs
//
// One System One request to the reviewer model's provider: `POST
// {base_url}/systemone`, the same way chat requests go to
// `{base_url}/chat/completions`. The body is a `state` plus named typed
// `questions`, answered with probabilities.
// Used by the AI reviewer when "Use typed input" is on (agent_run/review.rs).

use reqwest::Client;
use std::time::Duration;

const DEFAULT_TIMEOUT_MS: u64 = 15_000;

pub async fn request(
    base_url: &str,
    api_key: &str,
    model: &str,
    mut body: serde_json::Value,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value, String> {
    let model = model.to_string();
    let trimmed = base_url.trim().trim_end_matches('/');
    let is_cloudflare_needs_url_rewrite = trimmed.contains("api.cloudflare.com") && !trimmed.ends_with("/run");
    // For Cloudflare, keep only up to "...ai/" and drop anything after it
    let url = if is_cloudflare_needs_url_rewrite {
        // Ensure we keep the trailing slash after "ai"
        trimmed
            .split("/v1")
            .next()
            .map(|s| format!("{s}/run"))
            .unwrap_or_else(|| trimmed.to_string())
    } else {
        if !trimmed.ends_with("/systemone") {
            format!("{}/systemone", trimmed).to_string()
        } else {
            trimmed.to_string()
        }
    };
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("Invalid URL {url}: {e}"))?;
    println!("{url}");
    
    let local = matches!(parsed.host_str(), Some("localhost") | Some("127.0.0.1") | Some("::1"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
        return Err(format!("Refusing to send the review over {}: use https", parsed.scheme()));
    }

    // Inject `model` into the body
    if let serde_json::Value::Object(ref mut map) = body {
        map.insert("model".to_string(), serde_json::Value::String(model));
    } else {
        // If body is not an object, wrap it
        let mut new_body = serde_json::Map::new();
        new_body.insert("model".to_string(), serde_json::Value::String(model));
        if let serde_json::Value::Object(old) = body {
            for (k, v) in old {
                new_body.insert(k, v);
            }
        }
        body = serde_json::Value::Object(new_body);
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
