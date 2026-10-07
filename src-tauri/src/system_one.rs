// src-tauri/src/system_one.rs
//
// One System One request to the reviewer model's provider: `POST
// {base_url}/systemone`, the same way chat requests go to
// `{base_url}/chat/completions`. The body is a `state` plus named typed
// `questions` at the top level (wrapped in `input` for Cloudflare's `/run`),
// answered with probabilities. Rate limits, timeouts and server errors are
// retried like chat requests.
// Used by the AI reviewer when "Use typed input" is on (agent_run/review.rs).

use crate::chat_request::{
    backoff, classify_body_error, classify_status, human_duration, ChatError, ErrorKind, MAX_ATTEMPTS, MAX_RETRY_WAIT,
};
use reqwest::Client;
use std::time::Duration;

const DEFAULT_TIMEOUT_MS: u64 = 15_000;

pub async fn request(
    base_url: &str,
    api_key: &str,
    model: &str,
    body: serde_json::Value,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value, String> {
    let model = model.to_string();
    let trimmed = base_url.trim().trim_end_matches('/');
    let is_cloudflare = trimmed.contains("api.cloudflare.com");
    let is_cloudflare_needs_url_rewrite = is_cloudflare && !trimmed.ends_with("/run");
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
    let local = matches!(parsed.host_str(), Some("localhost") | Some("127.0.0.1") | Some("::1"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
        return Err(format!("Refusing to send the review over {}: use https", parsed.scheme()));
    }

    // `/systemone` takes `state` and `questions` at the top level next to
    // `model`. Cloudflare's `/run` takes `model` plus the payload under `input`.
    let body = if is_cloudflare {
        serde_json::json!({ "model": model, "input": body })
    } else {
        match body {
            serde_json::Value::Object(mut map) => {
                map.insert("model".to_string(), serde_json::Value::String(model));
                serde_json::Value::Object(map)
            }
            _ => return Err("System One request body must be a JSON object".to_string()),
        }
    };

    let client = Client::builder()
        .timeout(Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1_000, 60_000)))
        .build()
        .map_err(|e| e.to_string())?;

    // Same retry policy as chat requests (chat_request::run): backoff with
    // jitter, or the provider's Retry-After when longer; a wait over
    // MAX_RETRY_WAIT isn't retried.
    let mut attempt_number = 1;
    loop {
        let error = match send_once(&client, &parsed, api_key, &body).await {
            Ok(value) => return Ok(value),
            Err(e) => e,
        };
        if !error.retryable || attempt_number >= MAX_ATTEMPTS {
            return Err(describe(&error, &parsed, attempt_number));
        }
        let suggested = error.retry_after_ms.map(Duration::from_millis);
        if let Some(wait) = suggested.filter(|w| *w > MAX_RETRY_WAIT) {
            return Err(format!("{} Try again in {}.", describe(&error, &parsed, attempt_number), human_duration(wait)));
        }
        tokio::time::sleep(backoff(attempt_number).max(suggested.unwrap_or_default())).await;
        attempt_number += 1;
    }
}

async fn send_once(
    client: &Client,
    url: &reqwest::Url,
    api_key: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, ChatError> {
    let mut req = client.post(url.clone()).json(body);
    let key = api_key.trim();
    if !key.is_empty() {
        req = req.bearer_auth(key);
    }

    let transport = |e: reqwest::Error| {
        let kind = if e.is_timeout() { ErrorKind::Timeout } else { ErrorKind::Network };
        ChatError::new(kind, format!("Could not reach {url}: {e}"))
    };
    let res = req.send().await.map_err(transport)?;
    let status = res.status();
    let headers = res.headers().clone();
    let text = res.text().await.map_err(transport)?;
    if !status.is_success() {
        return Err(classify_status(status.as_u16(), &headers, &text));
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| ChatError::new(ErrorKind::BadRequest, format!("Response is not JSON: {e}")).with_details(&text))?;
    // Some providers report failures inside a 200 response.
    match classify_body_error(&value) {
        Some(e) => Err(e),
        None => Ok(value),
    }
}

fn describe(error: &ChatError, url: &reqwest::Url, attempts: u32) -> String {
    let mut message = match error.kind {
        ErrorKind::NotFound => format!("{}: this provider has no /systemone endpoint at {url}.", error.message),
        _ => error.message.clone(),
    };
    // A JSON error body is already summed up in the message.
    let raw = error.details.as_deref().filter(|d| serde_json::from_str::<serde_json::Value>(d).is_err());
    if let Some(details) = raw {
        let snippet: String = details.chars().take(300).collect();
        message = format!("{message} {snippet}");
    }
    if attempts > 1 {
        message = format!("{message} (after {attempts} attempts)");
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves one canned raw HTTP response per connection, in order.
    async fn mock(replies: Vec<&'static str>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let served = Arc::new(AtomicUsize::new(0));
        let count = served.clone();
        tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                count.fetch_add(1, Ordering::SeqCst);
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (url, served)
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 34\r\nConnection: close\r\n\r\n{\"answers\":{\"safe\":{\"noul\":0.97}}}";

    async fn send(url: &str) -> Result<serde_json::Value, String> {
        request(url, "", "typesafe/jev-1", serde_json::json!({ "state": {}, "questions": {} }), Some(2_000)).await
    }

    #[tokio::test]
    async fn rate_limit_is_retried_after_retry_after() {
        let limited = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (url, served) = mock(vec![limited, OK]).await;
        let started = std::time::Instant::now();
        let value = send(&url).await.unwrap();
        assert_eq!(value.pointer("/answers/safe/noul").and_then(|v| v.as_f64()), Some(0.97));
        assert_eq!(served.load(Ordering::SeqCst), 2);
        assert!(started.elapsed() >= Duration::from_secs(1), "waits at least Retry-After");
    }

    #[tokio::test]
    async fn server_errors_are_retried_up_to_max_attempts() {
        let failing = "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (url, served) = mock(vec![failing; MAX_ATTEMPTS as usize]).await;
        let error = send(&url).await.unwrap_err();
        assert!(error.contains(&format!("after {MAX_ATTEMPTS} attempts")), "{error}");
        assert_eq!(served.load(Ordering::SeqCst), MAX_ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn long_rate_limit_is_reported_not_retried() {
        let limited = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (url, served) = mock(vec![limited]).await;
        let error = send(&url).await.unwrap_err();
        assert!(error.ends_with("Try again in 1 h."), "{error}");
        assert_eq!(served.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn bad_requests_are_not_retried() {
        let bad = "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 29\r\nConnection: close\r\n\r\n{\"error\":\"state is required\"}";
        let (url, served) = mock(vec![bad]).await;
        let error = send(&url).await.unwrap_err();
        assert!(error.contains("state is required"), "{error}");
        assert_eq!(served.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn error_inside_a_200_response_is_retried() {
        let in_body = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 45\r\nConnection: close\r\n\r\n{\"error\":{\"code\":503,\"message\":\"overloaded\"}}";
        let (url, served) = mock(vec![in_body, OK]).await;
        assert!(send(&url).await.is_ok());
        assert_eq!(served.load(Ordering::SeqCst), 2);
    }
}
