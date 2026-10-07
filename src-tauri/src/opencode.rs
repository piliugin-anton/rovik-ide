// src-tauri/src/opencode.rs
//
// Requests to opencode.ai (chat and `/systemone`) carry the headers the
// opencode client sends: its User-Agent and a session id. One chat is one
// session: the id is made the first time a chat talks to opencode.ai and
// reused for every later request of that chat while the app runs. A request
// outside any chat gets a session of its own.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const AGENT: &str = "opencode/1.18.35 ai-sdk/provider-utils/4.0.40 runtime/bun/1.3.14";
const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Sessions by chat id.
static SESSIONS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

/// Whether `url` is on opencode.ai or one of its subdomains.
pub fn is_opencode(url: &str) -> bool {
    reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_end_matches('.').to_ascii_lowercase()))
        .is_some_and(|h| h == "opencode.ai" || h.ends_with(".opencode.ai"))
}

/// `ses_` + the Unix time in seconds as 12 hex digits + 14 random base62
/// characters.
fn new_session() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let random: String = uuid::Uuid::new_v4()
        .as_bytes()
        .iter()
        .take(14)
        .map(|b| BASE62[*b as usize % BASE62.len()] as char)
        .collect();
    format!("ses_{secs:012x}{random}")
}

/// The session of chat `chat_id`, made on first use; a new one without a chat.
pub fn session(chat_id: Option<&str>) -> String {
    let Some(chat_id) = chat_id.filter(|id| !id.is_empty()) else {
        return new_session();
    };
    let mut sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    sessions.entry(chat_id.to_string()).or_insert_with(new_session).clone()
}

/// The extra headers for a request to `url` from chat `chat_id`: empty
/// unless `url` is on opencode.ai.
pub fn headers(url: &str, chat_id: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if !is_opencode(url) {
        return headers;
    }
    headers.insert(USER_AGENT, HeaderValue::from_static(AGENT));
    if let Ok(value) = HeaderValue::from_str(&session(chat_id)) {
        headers.insert(HeaderName::from_static("x-opencode-session"), value.clone());
        headers.insert(HeaderName::from_static("x-opencode-session-id"), value);
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_opencode_hosts() {
        assert!(is_opencode("https://opencode.ai/zen/v1/chat/completions"));
        assert!(is_opencode("https://api.opencode.ai/v1"));
        assert!(is_opencode(" HTTPS://OpenCode.AI/zen/v1/systemone "));
        assert!(!is_opencode("https://notopencode.ai/v1"));
        assert!(!is_opencode("https://opencode.ai.example.com/v1"));
        assert!(!is_opencode("https://api.openai.com/v1"));
        assert!(!is_opencode("not a url"));
        assert!(headers("https://api.openai.com/v1", Some("chat")).is_empty());
    }

    #[test]
    fn session_format() {
        let s = new_session();
        assert_eq!(s.len(), 4 + 12 + 14, "{s}");
        assert!(s.starts_with("ses_"));
        assert!(s[4..16].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{s}");
        assert!(s[16..].bytes().all(|b| BASE62.contains(&b)), "{s}");
    }

    #[test]
    fn one_session_per_chat() {
        let a = session(Some("opencode-test-chat-a"));
        assert_eq!(session(Some("opencode-test-chat-a")), a);
        assert_ne!(session(Some("opencode-test-chat-b")), a);
        assert_ne!(session(None), session(None));

        let h = headers("https://opencode.ai/zen/v1/chat/completions", Some("opencode-test-chat-a"));
        assert_eq!(h.get(USER_AGENT).unwrap(), AGENT);
        assert_eq!(h.get("x-opencode-session").unwrap(), a.as_str());
        assert_eq!(h.get("x-opencode-session-id").unwrap(), a.as_str());
    }
}
