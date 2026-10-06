//! Parser and classification unit tests, plus end-to-end runs against a
//! local mock server that replays failure scenarios.

use super::*;
use reqwest::header::{HeaderValue, RETRY_AFTER};
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn collect() -> (Vec<ChatStreamEvent>, impl FnMut(ChatStreamEvent)) {
    (Vec::new(), |_e: ChatStreamEvent| {})
}

fn names(events: &[ChatStreamEvent]) -> Vec<String> {
    events
        .iter()
        // Diagnostics only; the tests check what the user sees.
        .filter(|e| !matches!(e, ChatStreamEvent::Finished { .. }))
        .map(|e| match e {
            ChatStreamEvent::Delta { content } => format!("delta:{content}"),
            ChatStreamEvent::Reasoning { content } => format!("reasoning:{content}"),
            ChatStreamEvent::ReasoningSupport { .. } => "reasoning_support".into(),
            ChatStreamEvent::Done => "done".into(),
            ChatStreamEvent::Finished { .. } => "finished".into(),
            ChatStreamEvent::Error { .. } => "error".into(),
            ChatStreamEvent::Retrying { attempt, .. } => format!("retrying:{attempt}"),
            ChatStreamEvent::ToolCallStart { name, .. } => format!("tool_start:{name}"),
            ChatStreamEvent::ToolCallArgumentsDelta { .. } => "tool_args".into(),
            ChatStreamEvent::ToolCallComplete { name, arguments, .. } => format!("tool_done:{name}:{arguments}"),
        })
        .collect()
}

/*
 * Parsing and classification
 */

#[test]
fn tool_call_arguments_are_assembled_by_index() {
    let mut parser = ReplyParser::default();
    let mut events = Vec::new();
    let mut emit = |e| events.push(e);
    // The id appears only in the first chunk, as in the OpenAI format.
    for chunk in [
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":""}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.ts\"}"}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ] {
        parser.feed(&chunk, &mut emit).unwrap();
    }
    parser.finish(&mut emit).unwrap();
    let last = names(&events).pop().unwrap();
    assert_eq!(last, r#"tool_done:read_file:{"path":"a.ts"}"#);
}

#[test]
fn usage_in_a_final_chunk_without_choices_is_kept() {
    // With stream_options.include_usage, OpenAI ends with `choices: []`.
    let mut parser = ReplyParser::default();
    let mut events = Vec::new();
    let mut emit = |e| events.push(e);
    for chunk in [
        json!({"choices":[{"delta":{"content":"Hi"},"finish_reason":"stop"}],"usage":null}),
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}),
    ] {
        parser.feed(&chunk, &mut emit).unwrap();
    }
    parser.finish(&mut emit).unwrap();
    let usage = events.iter().find_map(|e| match e {
        ChatStreamEvent::Finished { usage, .. } => usage.clone(),
        _ => None,
    });
    assert_eq!(usage.unwrap()["completion_tokens"], 2);
}

#[test]
fn missing_tool_call_ids_get_generated() {
    let mut parser = ReplyParser::default();
    let mut events = Vec::new();
    let mut emit = |e| events.push(e);
    parser
        .feed(&json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"list_dir","arguments":"{}"}}]}}]}), &mut emit)
        .unwrap();
    parser.finish(&mut emit).unwrap();
    // The last event is Finished (diagnostics); the call completes before it.
    let completed = events
        .iter()
        .rev()
        .find(|e| !matches!(e, ChatStreamEvent::Finished { .. }))
        .unwrap();
    match completed {
        ChatStreamEvent::ToolCallComplete { id, arguments, .. } => {
            assert!(id.starts_with("call_0_"));
            assert_eq!(arguments, "{}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn errors_inside_a_200_stream_are_classified() {
    let mut parser = ReplyParser::default();
    let (_, mut emit) = collect();
    let err = parser
        .feed(&json!({"error":{"code":429,"message":"Rate limit exceeded: free-models-per-min"}}), &mut emit)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::RateLimited);
    assert!(err.retryable);

    let err = ReplyParser::default()
        .feed(&json!({"error":{"message":"Upstream model crashed"}}), &mut emit)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Provider);
}

#[test]
fn empty_and_length_finishes() {
    let (_, mut emit) = collect();
    let mut p = ReplyParser::default();
    p.feed(&json!({"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}), &mut emit).unwrap();
    assert_eq!(p.finish(&mut emit).unwrap_err().kind, ErrorKind::EmptyResponse);

    let mut p = ReplyParser::default();
    p.feed(&json!({"choices":[{"delta":{},"finish_reason":"length"}]}), &mut emit).unwrap();
    assert_eq!(p.finish(&mut emit).unwrap_err().kind, ErrorKind::Length);
}

#[test]
fn status_classification_and_waits() {
    let mut h = HeaderMap::new();
    h.insert(RETRY_AFTER, HeaderValue::from_static("7"));
    let e = classify_status(429, &h, r#"{"error":{"message":"slow down"}}"#);
    assert_eq!((e.kind, e.retry_after_ms, e.retryable), (ErrorKind::RateLimited, Some(7000), true));

    let e = classify_status(400, &HeaderMap::new(), "This model's maximum context length is 8192 tokens");
    assert_eq!((e.kind, e.retryable), (ErrorKind::ContextLength, false));
    assert_eq!(classify_status(401, &HeaderMap::new(), "").kind, ErrorKind::Auth);
    assert!(classify_status(503, &HeaderMap::new(), "").retryable);

    // OpenRouter: the reset time (epoch ms) is inside the error body.
    let reset = (now_epoch() + Duration::from_secs(90)).as_millis();
    let body = json!({"error":{"code":429,"message":"limit","metadata":{"headers":{"X-RateLimit-Reset": reset.to_string()}}}});
    let e = classify_status(429, &HeaderMap::new(), &body.to_string());
    let ms = e.retry_after_ms.unwrap();
    assert!((88_000..=90_000).contains(&ms), "{ms}");

    assert_eq!(parse_duration_text("2m59.5s"), Some(Duration::from_secs_f64(179.5)));
    assert_eq!(parse_duration_text("750ms"), Some(Duration::from_millis(750)));
    assert_eq!(parse_duration_text("1h2m"), Some(Duration::from_secs(3720)));
    assert_eq!(human_duration(Duration::from_secs(4 * 3600 + 12 * 60)), "4 h 12 min");
}

/*
 * End to end against a mock server
 */

enum Reply {
    /// Full HTTP response, sent at once, then close.
    Raw(String),
    /// SSE headers, then events with a delay before each, then close or hang.
    Sse { events: Vec<(u64, String)>, hang: bool },
}

fn sse(events: &[&str]) -> Reply {
    Reply::Sse { events: events.iter().map(|e| (0, e.to_string())).collect(), hang: false }
}

const TEXT: &str = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
const DONE: &str = "data: [DONE]";
const EMPTY: &str = r#"data: {"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}"#;

/// Serves the replies in order, one per connection; returns the base URL.
async fn mock(replies: Vec<Reply>) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/chat/completions", listener.local_addr().unwrap());
    let served = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = served.clone();
    tokio::spawn(async move {
        for reply in replies {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = vec![0u8; 65536];
            let _ = socket.read(&mut buf).await;
            tokio::spawn(async move {
                match reply {
                    Reply::Raw(text) => {
                        let _ = socket.write_all(text.as_bytes()).await;
                    }
                    Reply::Sse { events, hang } => {
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                            .await;
                        for (delay, event) in events {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                            let _ = socket.write_all(format!("{event}\n\n").as_bytes()).await;
                        }
                        if hang {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                    }
                }
                let _ = socket.shutdown().await;
            });
        }
    });
    (url, served)
}

fn fast_limits() -> Limits {
    Limits { first_byte: Duration::from_millis(300), idle: Duration::from_millis(300), overall: Duration::from_secs(5) }
}

async fn run_against(url: &str, limits: Limits, cancel_after: Option<u64>) -> (Result<(), ChatError>, Vec<ChatStreamEvent>) {
    let client = Client::new();
    let body = json!({"model":"m","messages":[]});
    let spec = RequestSpec { client: &client, url, api_key: "k", body: &body, streaming: true, limits };
    let (tx, mut rx) = watch::channel(false);
    if let Some(ms) = cancel_after {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            let _ = tx.send(true);
        });
    } else {
        std::mem::forget(tx); // keep the channel open for the whole run
    }
    let mut events = Vec::new();
    let result = run(&spec, &mut rx, &mut |e| events.push(e)).await;
    (result, events)
}

#[tokio::test]
async fn empty_responses_are_retried_until_one_succeeds() {
    let (url, served) = mock(vec![sse(&[EMPTY, DONE]), sse(&[DONE]), sse(&[TEXT, DONE])]).await;
    let (result, events) = run_against(&url, fast_limits(), None).await;
    assert!(result.is_ok());
    assert_eq!(names(&events), ["retrying:2", "retrying:3", "delta:Hello", "done"]);
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[tokio::test]
async fn gives_up_after_max_attempts_with_typed_error() {
    let (url, _) = mock((0..MAX_ATTEMPTS).map(|_| sse(&[EMPTY, DONE])).collect()).await;
    let (result, events) = run_against(&url, fast_limits(), None).await;
    let error = result.unwrap_err();
    assert_eq!((error.kind, error.attempts, error.partial), (ErrorKind::EmptyResponse, MAX_ATTEMPTS, false));
    assert_eq!(events.iter().filter(|e| matches!(e, ChatStreamEvent::Retrying { .. })).count(), 3);
}

#[tokio::test]
async fn long_rate_limit_waits_are_reported_not_retried() {
    let raw = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3600\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":{\"message\":\"daily limit\"}}";
    let (url, served) = mock(vec![Reply::Raw(raw.into())]).await;
    let (result, events) = run_against(&url, fast_limits(), None).await;
    let error = result.unwrap_err();
    assert_eq!(error.kind, ErrorKind::RateLimited);
    assert_eq!(error.retry_after_ms, Some(3_600_000));
    assert!(error.message.ends_with("Try again in 1 h."), "{}", error.message);
    assert!(events.is_empty());
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn short_rate_limit_waits_are_honoured() {
    let raw = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nConnection: close\r\n\r\n";
    let (url, _) = mock(vec![Reply::Raw(raw.into()), sse(&[TEXT, DONE])]).await;
    let started = Instant::now();
    let (result, events) = run_against(&url, fast_limits(), None).await;
    assert!(result.is_ok());
    match &events[0] {
        ChatStreamEvent::Retrying { delay_ms, reason, .. } => {
            assert!(*delay_ms >= 1000, "waits at least Retry-After");
            assert_eq!(reason.kind, ErrorKind::RateLimited);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(started.elapsed() >= Duration::from_secs(1));
}

#[tokio::test]
async fn keepalive_comments_do_not_count_as_progress() {
    // Only ": PROCESSING" comments, then silence: a stall, then success.
    let stalled = Reply::Sse { events: vec![(0, ": OPENROUTER PROCESSING".into()), (150, ": OPENROUTER PROCESSING".into())], hang: true };
    let (url, _) = mock(vec![stalled, sse(&[TEXT, DONE])]).await;
    let (result, events) = run_against(&url, fast_limits(), None).await;
    assert!(result.is_ok());
    match &events[0] {
        ChatStreamEvent::Retrying { reason, .. } => assert_eq!(reason.kind, ErrorKind::Stalled),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn partial_reply_then_stall_stops_without_retrying() {
    let partial = Reply::Sse { events: vec![(0, TEXT.into())], hang: true };
    let (url, served) = mock(vec![partial, sse(&[TEXT, DONE])]).await;
    let (result, events) = run_against(&url, fast_limits(), None).await;
    let error = result.unwrap_err();
    assert_eq!((error.kind, error.partial, error.retryable), (ErrorKind::Stalled, true, false));
    assert_eq!(names(&events), ["delta:Hello"]);
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancel_stops_a_hanging_request_and_a_retry_wait() {
    let hanging = Reply::Sse { events: vec![], hang: true };
    let (url, _) = mock(vec![hanging]).await;
    let limits = Limits { first_byte: Duration::from_secs(10), ..fast_limits() };
    let started = Instant::now();
    let (result, _) = run_against(&url, limits, Some(150)).await;
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));

    // During the backoff wait after a 503.
    let raw = "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 30\r\nConnection: close\r\n\r\n";
    let (url, _) = mock(vec![Reply::Raw(raw.into())]).await;
    let started = Instant::now();
    let (result, events) = run_against(&url, fast_limits(), Some(200)).await;
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(names(&events), ["retrying:2"]);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn auth_errors_are_not_retried() {
    let raw = "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n{\"error\":{\"message\":\"bad key\"}}";
    let (url, served) = mock(vec![Reply::Raw(raw.into()), sse(&[TEXT, DONE])]).await;
    let (result, _) = run_against(&url, fast_limits(), None).await;
    let error = result.unwrap_err();
    assert_eq!((error.kind, error.attempts), (ErrorKind::Auth, 1));
    assert!(error.details.as_deref().unwrap().contains("bad key"));
    assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancel_through_the_registry_stops_a_waiting_request() {
    let hanging = Reply::Sse { events: vec![(0, ": OPENROUTER PROCESSING".into())], hang: true };
    let (url, _) = mock(vec![hanging]).await;
    let requests = Arc::new(ChatRequests::default());

    let r2 = requests.clone();
    let url2 = url.clone();
    let task = tokio::spawn(async move {
        let client = Client::new();
        let body = json!({"model":"m","messages":[]});
        let limits = Limits { first_byte: Duration::from_secs(20), ..fast_limits() };
        let spec = RequestSpec { client: &client, url: &url2, api_key: "k", body: &body, streaming: true, limits };
        let mut registration = r2.register(Some("req-1".into()));
        let mut events = Vec::new();
        run(&spec, &mut registration.cancel, &mut |e| events.push(e)).await
    });

    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    assert!(requests.cancel("req-1"), "the running request is registered");
    let result = tokio::time::timeout(Duration::from_secs(3), task).await.expect("cancel must end the request").unwrap();
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(1));
    // Finished requests are unregistered; a late cancel is just remembered.
    assert!(requests.cancel("req-1"));
}

#[test]
fn a_cancel_before_registration_is_not_lost() {
    let requests = ChatRequests::default();
    assert!(requests.cancel("early"), "accepted even before the request exists");
    let registration = requests.register(Some("early".into()));
    assert!(*registration.cancel.borrow(), "starts out cancelled");
    let other = requests.register(Some("other".into()));
    assert!(!*other.cancel.borrow());
}

#[test]
fn gemini_signatures_survive_and_unindexed_parallel_calls_stay_separate() {
    let mut parser = ReplyParser::default();
    let mut events = Vec::new();
    let mut emit = |e| events.push(e);
    // Gemini: no `index`, each call complete in its own chunk, signature on the first.
    for chunk in [
        json!({"choices":[{"delta":{"role":"assistant","tool_calls":[{"extra_content":{"google":{"thought_signature":"SIG-A"}},"id":"function-call-1","type":"function","function":{"name":"list_dir","arguments":"{}"}}]}}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"id":"function-call-2","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}]}}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ] {
        parser.feed(&chunk, &mut emit).unwrap();
    }
    parser.finish(&mut emit).unwrap();
    let done: Vec<(String, Option<serde_json::Value>)> = events
        .iter()
        .filter_map(|e| match e {
            ChatStreamEvent::ToolCallComplete { id, extra_content, .. } => Some((id.clone(), extra_content.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(done.len(), 2, "two separate calls");
    assert_eq!(done[0].0, "function-call-1");
    assert_eq!(done[0].1, Some(json!({"google":{"thought_signature":"SIG-A"}})));
    assert_eq!(done[1], ("function-call-2".to_string(), None));
}
