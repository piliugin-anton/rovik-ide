//! Wording for request failures (chat_request.rs): the status line while
//! retrying, and what's stored when a request gives up. The chat renders
//! the stored failure itself (src/lib/chatErrors.ts); the text here is the
//! fallback kept with the message.

use crate::chat_db::store::now_ms;
use crate::chat_request::{ChatError, ErrorKind};
use serde_json::{json, Value};

/// The final outcome of a failed request, as stored in `meta.failure` or
/// `meta.interrupted`.
pub fn stored(error: &ChatError) -> Value {
    let mut failure = json!({
        "kind": error.kind,
        "message": error.message,
        "attempts": error.attempts,
    });
    if let Some(ms) = error.retry_after_ms {
        failure["resetAt"] = json!(now_ms() + ms as i64);
    }
    if let Some(details) = &error.details {
        failure["details"] = json!(details);
    }
    failure
}

/// "45 s", "12 min", "4 h 12 min"
pub fn format_duration(ms: i64) -> String {
    let secs = (ms.max(0) + 999) / 1000;
    if secs < 60 {
        return format!("{secs} s");
    }
    if secs < 3600 {
        return format!("{} min", (secs + 59) / 60);
    }
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if m > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{h} h")
    }
}

/// Short reason for the status line while retrying.
pub fn retry_reason(error: &ChatError) -> String {
    match error.kind {
        ErrorKind::EmptyResponse => "Empty response from the model.".into(),
        ErrorKind::Stalled => "No response from the model.".into(),
        ErrorKind::Timeout => "The reply took too long.".into(),
        ErrorKind::RateLimited => "The model is busy (rate limited).".into(),
        ErrorKind::ServerError | ErrorKind::Provider => match error.status {
            Some(status) => format!("Provider error ({status})."),
            None => "Provider error.".into(),
        },
        ErrorKind::Network => "Connection problem.".into(),
        _ => "The request failed.".into(),
    }
}

/// The sentence stored with a failed request.
pub fn describe(error: &ChatError) -> String {
    let after = if error.attempts > 1 {
        format!(" (after {} attempts)", error.attempts)
    } else {
        String::new()
    };
    match error.kind {
        ErrorKind::RateLimited => match error.retry_after_ms {
            Some(ms) if ms > 0 => format!("Rate limit reached. Try again in {}.", format_duration(ms as i64)),
            _ => format!("The provider is rate limiting requests{after}. Try again shortly."),
        },
        ErrorKind::EmptyResponse => format!("The model returned an empty response{after}."),
        ErrorKind::Stalled => format!("The model stopped responding{after}."),
        ErrorKind::Timeout => format!("The reply took too long{after}."),
        ErrorKind::ServerError => format!("The provider had a server error{after}."),
        ErrorKind::Network => format!("Couldn't reach the provider{after}."),
        ErrorKind::Auth => "The API key was rejected. Check it in Model Settings.".into(),
        ErrorKind::NotFound => "The model or endpoint wasn't found. Check the model ID in Model Settings.".into(),
        ErrorKind::ContextLength => "The conversation is too long for this model. Start a new chat, or set a smaller context window for it in Model Settings.".into(),
        ErrorKind::Length => "The model reached its output limit before writing anything.".into(),
        ErrorKind::Cancelled => "Stopped.".into(),
        ErrorKind::BadRequest => "The provider rejected the request.".into(),
        ErrorKind::Provider => format!("The provider reported an error{after}."),
    }
}

pub fn stopped() -> ChatError {
    ChatError::new(ErrorKind::Cancelled, "Stopped.")
}

/// A problem before the request was sent (e.g. building the history).
pub fn before_request(message: String) -> ChatError {
    let mut error = ChatError::new(ErrorKind::Provider, message.clone());
    error.retryable = false;
    error.details = Some(message);
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wording() {
        assert_eq!(format_duration(45_000), "45 s");
        assert_eq!(format_duration(61_000), "2 min");
        assert_eq!(format_duration(3_600_000 * 4 + 12 * 60_000), "4 h 12 min");
        let mut e = ChatError::new(ErrorKind::RateLimited, "x");
        e.retry_after_ms = Some(90_000);
        assert_eq!(describe(&e), "Rate limit reached. Try again in 2 min.");
        assert_eq!(stored(&e)["kind"], "rate_limited");
        assert!(stored(&e)["resetAt"].as_i64().unwrap() > now_ms());
        assert_eq!(describe(&stopped()), "Stopped.");
    }
}
